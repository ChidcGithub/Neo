"""Controlled local GPU fine-tuning, fixed evaluation, and conservative release gate."""
from __future__ import annotations

import argparse
import copy
from functools import lru_cache
import json
from math import gcd
from pathlib import Path
import random
import shutil
import time

import numpy as np
import onnxruntime as ort
from scipy.signal import fftconvolve, resample_poly
import soundfile as sf
import torch
import torch.nn.functional as F

from optimize_wake import ASSETS, OLD, ROOT, RUN, SEED, save_json, sha256

SR = 16000
WINDOW = 32000
HOP = 1280
THRESHOLD = 0.25
NEGATIVE_PHRASES = ['hi', 'hey', 'hello', 'hi everyone', 'hi guys', 'hello everyone', 'good morning', 'neo', 'hi neil', 'hey neil', 'hi leo', 'hi neon', 'hi nia', 'i know', 'hi no', 'hello neo', 'okay neo', 'ok neo', 'how are you', 'thank you', 'open your books', 'be quiet', 'listen to me', 'we need to know']


def seed_all(seed=SEED):
    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)
    torch.cuda.manual_seed_all(seed)
    torch.set_num_threads(2)
    torch.backends.cudnn.benchmark = False
    torch.backends.cuda.matmul.allow_tf32 = False
    torch.backends.cudnn.allow_tf32 = False


def normalize_audio(audio, sr):
    audio = np.asarray(audio, dtype=np.float32)
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    if sr != SR:
        g = gcd(sr, SR)
        audio = resample_poly(audio, SR // g, sr // g)
    return np.asarray(audio, dtype=np.float32)


@lru_cache(maxsize=48)
def load_audio(path):
    a, sr = sf.read(path, dtype='float32', always_2d=True)
    return normalize_audio(a, sr)


def trim(audio):
    active = np.flatnonzero(np.abs(audio) > max(0.001, float(np.max(np.abs(audio))) * 0.04))
    if not len(active):
        return audio
    return audio[max(0, active[0] - 800):min(len(audio), active[-1] + 1600)]


def segment(audio, length, rng):
    if not len(audio):
        raise ValueError('音频为空，无法抽取片段')
    if len(audio) < length:
        audio = np.tile(audio, int(np.ceil(length / len(audio))) + 1)
    start = int(rng.integers(0, len(audio) - length + 1))
    return audio[start:start + length].copy()


def render(speech, noise, snr, rng, rir=None, length=WINDOW, end=None):
    speech = trim(speech).copy()
    if rir is not None:
        rir = rir / max(float(np.sqrt(np.sum(rir ** 2))), 1e-8)
        speech = fftconvolve(speech, rir, mode='full').astype(np.float32)
    # Preserve complete reverberation tail; exclude overlong clips upstream if necessary.
    if len(speech) > length - 800:
        speech = resample_poly(speech, length - 800, len(speech)).astype(np.float32)
    speech *= 10 ** (float(rng.uniform(-6, 1)) / 20)
    if end is None:
        end = length - int(rng.integers(0, 4801))
    end = max(len(speech), min(end, length))
    clean = np.zeros(length, np.float32)
    clean[end - len(speech):end] = speech
    mixed = clean.copy()
    if noise is not None and snr is not None:
        n = segment(noise, length, rng)
        active = np.abs(speech) > max(float(np.max(np.abs(speech))) * 0.04, 1e-6)
        rms = np.sqrt(np.mean(speech[active] ** 2)) if active.any() else 0.0
        n *= rms / max(float(np.sqrt(np.mean(n ** 2))), 1e-8) * 10 ** (-snr / 20)
        mixed += n
    mixed /= max(1.0, float(np.max(np.abs(mixed))) / 0.95)
    # Same quantization scale as the Rust i16 input path.
    return (np.round(mixed * 32767).astype(np.int16).astype(np.float32) / 32768).astype(np.float32)


def partition(paths):
    groups = {'train': [], 'dev': [], 'test': []}
    # Content hashes also group duplicate noise/RIR files under the same split.
    for p in sorted(paths):
        key = int(sha256(p)[:8], 16) % 10
        groups['test' if key == 0 else 'dev' if key == 1 else 'train'].append(p)
    return groups


class Frontend:
    def __init__(self):
        opt = ort.SessionOptions()
        opt.intra_op_num_threads = 2
        opt.inter_op_num_threads = 1
        self.mel = ort.InferenceSession(str(ASSETS / 'melspectrogram.onnx'), opt, providers=['CPUExecutionProvider'])
        self.emb = ort.InferenceSession(str(ASSETS / 'embedding_model.onnx'), opt, providers=['CPUExecutionProvider'])

    def extract(self, clips):
        if not len(clips):
            raise ValueError('前端输入片段为空')
        windows = []
        for clip in clips:
            m = self.mel.run(None, {self.mel.get_inputs()[0].name: clip[None].astype(np.float32)})[0][0, 0] / 10 + 2
            ws = np.stack([m[i:i + 76] for i in range(0, len(m) - 75, 8)])[-16:]
            assert ws.shape == (16, 76, 32)
            windows.append(ws)
        flat = np.concatenate(windows)[..., None].astype(np.float32)
        out = []
        for i in range(0, len(flat), 128):
            out.append(self.emb.run(None, {self.emb.get_inputs()[0].name: flat[i:i + 128]})[0].reshape(-1, 96))
        return np.concatenate(out).reshape(len(clips), 16, 96)


def make_model(path):
    from livekit.wakeword.config import WakeWordConfig
    from livekit.wakeword.models.pipeline import WakeWordClassifier
    config = WakeWordConfig(model_name='hi_neo', target_phrases=['hi neo'],
                            model={'model_type': 'conv_attention', 'model_size': 'small'})
    m = WakeWordClassifier(config)
    m.load_state_dict(torch.load(path, map_location='cpu', weights_only=True), strict=True)
    return m.eval()


def cpu_state_dict(model):
    """仅复制权重，不能迁移正在被优化器引用的模型参数。"""
    return {k: v.detach().cpu().clone() for k, v in model.state_dict().items()}


def synthesize_negatives():
    from livekit.wakeword.data.piper.synthesis import _load_vits_model, _generate_audio, _right_pad_lists, get_phonemes
    from livekit.wakeword.data.piper.text import normalize_phrases_for_piper
    p = ROOT / 'data/piper/en-us-libritts-high.pt'
    cfg = json.loads(p.with_suffix('.json').read_text(encoding='utf-8'))
    model = _load_vits_model(p, torch.device('cuda'))
    texts = normalize_phrases_for_piper(NEGATIVE_PHRASES)
    ids = [get_phonemes(cfg, t, cfg['espeak']['voice']) for t in texts]
    rng = np.random.default_rng(SEED)
    records = []
    for split, count, low, high in [('train', 1200, 20, 700), ('dev', 240, 700, 800), ('test', 240, 800, 904)]:
        directory = RUN / 'negative_audio' / split
        directory.mkdir(parents=True, exist_ok=True)
        for start in range(0, count, 16):
            n = min(16, count - start)
            pairs = [rng.choice(np.arange(low, high), 2, replace=False).tolist() for _ in range(n)]
            indices = [(start + i) % len(ids) for i in range(n)]
            batch = [ids[i] for i in indices]
            with torch.no_grad():
                audio = _generate_audio(model, torch.tensor([p[0] for p in pairs], device='cuda'), torch.tensor([p[1] for p in pairs], device='cuda'), _right_pad_lists(batch), [len(x) for x in batch], float(rng.choice([0.25, 0.5, 0.75])), 0.667, 0.8, float(rng.choice([0.8, 1.0, 1.2])), torch.device('cuda')).cpu().numpy()
            for i, a in enumerate(audio):
                a = normalize_audio(a.flatten(), 22050)
                a = trim(a)
                a /= max(1.0, float(np.max(np.abs(a))) / 0.95)
                path = directory / f'negative_{start + i:05d}.wav'
                sf.write(path, a, SR)
                records.append({'path': str(path.relative_to(ROOT)), 'split': split, 'phrase': NEGATIVE_PHRASES[indices[i]], 'speakers': pairs[i], 'sha256': sha256(path)})
            if start % 160 == 0:
                print('negative TTS', split, start, '/', count, flush=True)
    del model
    torch.cuda.empty_cache()
    save_json('negative_manifest.json', records)


def prepare():
    seed_all()
    RUN.mkdir(parents=True, exist_ok=True)
    for name in ['hi_neo.onnx', 'hi_neo.pt']:
        backup = RUN / ('baseline_' + name)
        if not backup.exists():
            shutil.copy2(OLD / name, backup)
    assert sha256(RUN / 'baseline_hi_neo.onnx') == sha256(ASSETS / 'hi_neo.onnx')
    if not (RUN / 'negative_manifest.json').exists():
        synthesize_negatives()
    noise = partition((ROOT / 'data/backgrounds').rglob('*.wav'))
    rirs = partition((ROOT / 'data/rirs').rglob('*.wav'))
    rng = np.random.default_rng(SEED)
    front = Frontend()
    provenance = []
    feats = {}
    train_sources = [OLD / 'positive_train' / f'clip_{i:06d}.wav' for i in rng.choice(10000, 2000, replace=False)]
    evaluation = np.random.default_rng(SEED + 1).permutation(1000)
    sources = {'train': train_sources, 'dev': [OLD / 'positive_test' / f'clip_{i:06d}.wav' for i in evaluation[:200]], 'test': [OLD / 'positive_test' / f'clip_{i:06d}.wav' for i in evaluation[200:400]]}
    real = sorted((ROOT / 'data/recordings').rglob('*.wav'))
    real_train = [p for p in real if 'positive_train' in p.name]
    real_test = [p for p in real if 'positive_test' in p.name]
    if not real_train or not real_test:
        raise ValueError('缺少真人训练或诊断录音；请先导入数据，不能生成空特征')
    for kind, groups in [('噪声', noise), ('混响', rirs), ('正例', sources)]:
        if any(not files for files in groups.values()):
            raise ValueError(kind + '的训练/开发/诊断划分存在空集')
    assert not set(real_train) & set(real_test)
    source_hashes = {split: {sha256(p) for p in files} for split, files in sources.items()}
    assert not source_hashes['train'] & (source_hashes['dev'] | source_hashes['test'])
    assert not source_hashes['dev'] & source_hashes['test']

    def generate(name, paths, split, snrs, repeats=1, reverberate=True):
        if not paths:
            raise ValueError('特征数据集为空：' + name)
        clips, result = [], []
        for repeat in range(repeats):
            for i, path in enumerate(paths):
                snr = snrs[(i + repeat) % len(snrs)]
                npth = noise[split][int(rng.integers(len(noise[split])))] if snr is not None else None
                rpth = rirs[split][int(rng.integers(len(rirs[split])))] if reverberate and rng.random() < 0.5 else None
                clip = render(load_audio(path), load_audio(npth) if npth else None, snr, rng, load_audio(rpth) if rpth else None)
                clips.append(clip)
                provenance.append({'set': name, 'source': str(path.relative_to(ROOT)), 'round': repeat, 'snr_db': snr, 'noise': str(npth.relative_to(ROOT)) if npth else None, 'rir': str(rpth.relative_to(ROOT)) if rpth else None})
                if len(clips) == 16:
                    result.append(front.extract(clips))
                    clips = []
                if i % 500 == 0:
                    print('features', name, repeat, i, '/', len(paths), flush=True)
        if clips:
            result.append(front.extract(clips))
        feats[name] = np.concatenate(result)

    generate('train_aug', sources['train'], 'train', [None, 20, 10, 5, 0], repeats=2)
    generate('train_real', real_train, 'train', [None, 20, 10, 5, 0], repeats=32)
    for split in ['train', 'dev', 'test']:
        negpaths = sorted((RUN / 'negative_audio' / split).glob('*.wav'))
        generate(split + '_negative', negpaths, split, [None, 20, 10, 5], repeats=2 if split == 'train' else 1)
        if split != 'train':
            for snr in [None, 20, 10, 5, 0]:
                generate(split + '_' + ('clean' if snr is None else f'snr{snr}'), sources[split], split, [snr], reverberate=snr is not None)
    for snr in [None, 20, 10, 5, 0]:
        generate('real_' + ('clean' if snr is None else f'snr{snr}'), real_test, 'test', [snr], repeats=8, reverberate=snr is not None)
    background = []
    for i in range(800):
        path = noise['train'][int(rng.integers(len(noise['train'])))]
        a = segment(load_audio(path), WINDOW, rng)
        a *= 10 ** (rng.uniform(-10, 3) / 20)
        a /= max(1.0, float(np.max(np.abs(a))) / 0.95)
        background.append(a)
        provenance.append({'set': 'train_background', 'source': str(path.relative_to(ROOT)), 'round': i})
    feats['train_background'] = np.concatenate([front.extract(background[i:i + 16]) for i in range(0, len(background), 16)])
    np.savez(RUN / 'controlled_features.npz', **feats)
    manifest_files = set(sources['train'] + sources['dev'] + sources['test'] + real)
    manifest_files.update(p for group in noise.values() for p in group)
    manifest_files.update(p for group in rirs.values() for p in group)
    manifest_files.update((RUN / 'negative_audio').rglob('*.wav'))
    save_json('data_manifest.json', {'seed': SEED, 'files': [{'path': str(p.relative_to(ROOT)), 'sha256': sha256(p), 'bytes': p.stat().st_size} for p in sorted(manifest_files)], 'noise_split': {k: [str(p.relative_to(ROOT)) for p in v] for k, v in noise.items()}, 'rir_split': {k: [str(p.relative_to(ROOT)) for p in v] for k, v in rirs.items()}, 'positive_sources': {k: [str(p.relative_to(ROOT)) for p in v] for k, v in sources.items()}, 'features': {k: list(v.shape) for k, v in feats.items()}, 'examples': provenance, 'limitations': ['Legacy synthetic train/test repeat speaker-pair schedule; not unknown-speaker evaluation.', 'New negative TTS uses disjoint speaker IDs by split, but baseline historical data can contain those speakers.', 'Real 14 train / 2 diagnostic clips share ONE original session; NOT session-independent validation.', 'New noise/RIR files are content-hash-disjoint for new augmentation; legacy replay/model can have historical exposure.', 'Legacy adversarial features excluded from optimization: phrase provenance absent and high neo is homophonic with target.']})
    print('PREPARE COMPLETE', {k: len(v) for k, v in feats.items()}, flush=True)


def validate_features(groups, required=()):
    for name in required:
        if name not in groups:
            raise ValueError('缺少必需特征集：' + name)
    for name, values in groups.items():
        if values.ndim != 3 or values.shape[1:] != (16, 96) or not len(values):
            raise ValueError('特征必须为非空 (N,16,96)：' + name)
        if not np.isfinite(values).all():
            raise ValueError('特征含非有限数值：' + name)


@torch.no_grad()
def predict(model, x):
    validate_features({'预测输入': x})
    model.eval()
    out = []
    for start in range(0, len(x), 512):
        b = torch.as_tensor(np.array(x[start:start + 512], dtype=np.float32), device='cuda')
        out.append(model(b).flatten().cpu().numpy())
    return np.concatenate(out)


def metric(scores, positive):
    hits = int(np.count_nonzero(scores >= THRESHOLD))
    return {'n': len(scores), 'hits': hits, 'recall' if positive else 'false_positive_fraction': hits / len(scores)}


def gate(baseline, candidate):
    reasons = []
    if candidate['test_clean']['recall'] < baseline['test_clean']['recall'] - 0.005:
        reasons.append('clean_recall_regression')
    b = np.mean([baseline[f'test_snr{s}']['recall'] for s in [20, 10, 5, 0]])
    c = np.mean([candidate[f'test_snr{s}']['recall'] for s in [20, 10, 5, 0]])
    if c < b + 0.02:
        reasons.append('noise_gain_less_than_2_percentage_points')
    for key in ['test_negative', 'legacy_negative', 'legacy_background', 'acav_validation']:
        if candidate[key]['hits'] > baseline[key]['hits']:
            reasons.append(key + '_false_positives_increased')
    if candidate['legacy_positive']['recall'] < baseline['legacy_positive']['recall'] - 0.005:
        reasons.append('legacy_positive_regression')
    for key in baseline:
        if key.startswith('real_') and candidate[key]['hits'] < baseline[key]['hits']:
            reasons.append(key + '_regression')
    return reasons


def train(steps):
    if any((RUN / name).exists() for name in ['training_summary.json', 'training_history.json', 'candidate.pt']):
        raise FileExistsError('已有训练产物，禁止覆盖混轮；请使用独立实验目录')
    if steps <= 0:
        raise ValueError('训练步数必须大于零')
    seed_all()
    assert torch.cuda.is_available()
    data = dict(np.load(RUN / 'controlled_features.npz'))
    validate_features(data, ['train_aug', 'train_real', 'train_negative', 'train_background'])
    oldpos = np.load(OLD / 'positive_features_train.npy')[:10000]
    acavpath = ROOT / 'data/features/openwakeword_features_ACAV100M_2000_hrs_16bit.npy'
    acav = np.load(acavpath, mmap_mode='r')
    rng = np.random.default_rng(SEED)
    # A fixed 100k replay pool plus its baseline hard negatives keeps I/O bounded.
    pool_indices = rng.choice(len(acav), 100000, replace=False)
    np.save(RUN / 'acav_pool_indices.npy', pool_indices)
    pool = np.array(acav[pool_indices], dtype=np.float32)
    model = make_model(RUN / 'baseline_hi_neo.pt').cuda()
    teacher = copy.deepcopy(model).eval()
    teacher.requires_grad_(False)
    pool_scores = predict(teacher, pool)
    hard = pool[np.argsort(pool_scores)[-4096:]]
    groups = {k: torch.tensor(v, device='cuda') for k, v in {'old_positive': oldpos, 'aug_positive': data['train_aug'], 'real_positive': data['train_real'], 'negative': data['train_negative'], 'background': data['train_background'], 'acav': pool, 'hard': hard}.items()}
    dev = {k: v for k, v in data.items() if k.startswith('dev_')}
    # Hold out ACAV validation rows from optimizer and split selection/final partitions.
    validation = np.load(ROOT / 'data/features/validation_set_features.npy')
    validation = validation[:len(validation) // 16 * 16].reshape(-1, 16, 96)
    dev['dev_acav'] = validation[::2]
    baseline = {k: metric(predict(model, v), k not in ['dev_negative', 'dev_acav']) for k, v in dev.items()}
    save_json('dev_baseline.json', baseline)
    optimizer = torch.optim.AdamW(model.parameters(), lr=2e-5, weight_decay=0.01)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, steps, eta_min=2e-6)
    specs = [('old_positive', 48, 1), ('aug_positive', 48, 1), ('real_positive', 8, 1), ('negative', 80, 0), ('background', 32, 0), ('acav', 192, 0), ('hard', 32, 0)]
    history = []
    best = -float('inf')
    selected = 0
    start = time.perf_counter()
    for step in range(1, steps + 1):
        model.train()
        xx, yy = [], []
        for name, count, label in specs:
            a = groups[name]
            xx.append(a[torch.randint(len(a), (count,), device='cuda')])
            yy.append(torch.full((count, 1), float(label), device='cuda'))
        x, y = torch.cat(xx), torch.cat(yy)
        pred = model(x).clamp(1e-6, 1 - 1e-6)
        loss_terms = F.binary_cross_entropy(pred, y, reduction='none')
        loss = (loss_terms * torch.where(y > 0, 1.0, 4.0)).mean()
        # Preserve the clean-positive calibration rather than just lowering the operating point.
        with torch.no_grad():
            target = teacher(x[:48])
        loss = loss + 0.5 * F.mse_loss(pred[:48], target)
        optimizer.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        optimizer.step()
        scheduler.step()
        if step % 250 == 0 or step == steps:
            metrics = {k: metric(predict(model, v), k not in ['dev_negative', 'dev_acav']) for k, v in dev.items()}
            feasible = metrics['dev_clean']['recall'] >= baseline['dev_clean']['recall'] - 0.005 and all(metrics[k]['hits'] <= baseline[k]['hits'] for k in ['dev_negative', 'dev_acav'])
            score = float(np.mean([metrics[f'dev_snr{s}']['recall'] for s in [20, 10, 5, 0]]))
            row = {'step': step, 'loss': float(loss.item()), 'elapsed_s': time.perf_counter() - start, 'metrics': metrics, 'feasible': feasible, 'noise_recall': score}
            history.append(row)
            if feasible and score > best:
                best, selected = score, step
                torch.save(cpu_state_dict(model), RUN / 'candidate.pt')
            torch.save(cpu_state_dict(model), RUN / 'last.pt')
            torch.save({'step': step, 'model': cpu_state_dict(model),
                        'optimizer': optimizer.state_dict(), 'scheduler': scheduler.state_dict()},
                       RUN / 'resume.pt')
            row['model_sha256'] = sha256(RUN / 'last.pt')
            if selected == step:
                row['candidate_sha256'] = sha256(RUN / 'candidate.pt')
            save_json('training_history.json', history)
            print('TRAIN', step, 'loss', round(row['loss'], 5), 'noise', score, 'feasible', feasible, 'selected', selected, 'seconds', round(row['elapsed_s']), flush=True)
    if not selected:
        shutil.copy2(RUN / 'last.pt', RUN / 'candidate.pt')
    torch.cuda.synchronize()
    save_json('training_summary.json', {'seed': SEED, 'steps': steps, 'selected_step': selected or steps, 'selection_feasible': bool(selected), 'batch_size': sum(n for _, n, _ in specs), 'batch_composition': specs, 'gpu': torch.cuda.get_device_name(), 'gpu_peak_allocated_bytes': torch.cuda.max_memory_allocated(), 'seconds': time.perf_counter() - start, 'starting_pt_sha256': sha256(RUN / 'baseline_hi_neo.pt'), 'candidate_pt_sha256': sha256(RUN / 'candidate.pt'), 'acav_pool_count': len(pool), 'acav_pool_indices_sha256': sha256(RUN / 'acav_pool_indices.npy'), 'learning_rate': 2e-5, 'threshold_fixed': THRESHOLD, 'controlled_features_sha256': sha256(RUN / 'controlled_features.npz'), 'legacy_positive_sha256': sha256(OLD / 'positive_features_train.npy'), 'acav_source_sha256': sha256(acavpath), 'validation_sha256': sha256(ROOT / 'data/features/validation_set_features.npy')})


def event_indices(scores):
    events = []
    streak = 0
    next_valid = 0
    for i, score in enumerate(scores):
        if i < next_valid:
            continue
        streak = streak + 1 if score >= THRESHOLD else 0
        if streak >= 2:
            events.append(i)
            streak = 0
            # After clearing the buffer, 25 new 80ms frames must arrive.
            next_valid = i + 25
    return events


def stream_eval(models, data_manifest, output_prefix=''):
    rng = np.random.default_rng(SEED + 99)
    frontend = Frontend()
    noise = [ROOT / p for p in data_manifest['noise_split']['test']]
    rirs = [ROOT / p for p in data_manifest['rir_split']['test']]
    positives = [ROOT / p for p in data_manifest['positive_sources']['test'][:24]]
    negatives = sorted((RUN / 'negative_audio/test').glob('*.wav'))[:48]
    real = sorted((ROOT / 'data/recordings').rglob('*positive_test*.wav'))
    if not all([noise, rirs, positives, negatives, real]):
        raise ValueError('流式诊断缺少噪声、混响、正负例或真人诊断录音')
    scenarios = []
    for group, files in [('synthetic', positives), ('real_same_session', real)]:
        for condition, snr in [('clean', None), ('snr10', 10), ('snr0', 0)]:
            for i, path in enumerate(files):
                a = render(load_audio(path), load_audio(noise[i % len(noise)]) if snr is not None else None, snr, rng, load_audio(rirs[i % len(rirs)]) if snr is not None else None, length=64000, end=44000)
                scenarios.append((group + '_' + condition, a, True, str(path.relative_to(ROOT))))
    for i, p in enumerate(negatives):
        a = render(load_audio(p), load_audio(noise[i % len(noise)]), 10, rng, length=64000, end=44000)
        scenarios.append(('negative_phrases', a, False, str(p.relative_to(ROOT))))
    for p in noise[:20]:
        a = load_audio(p)[:SR * 30]
        if len(a) >= WINDOW:
            scenarios.append(('continuous_background', a, False, str(p.relative_to(ROOT))))
    result = {k: {} for k in models}
    scores_saved = {}
    rows = []
    for idx, (group, audio, positive, source) in enumerate(scenarios):
        audio = np.round(np.clip(audio, -1, 1) * 32767).astype(np.int16).astype(np.float32) / 32768
        ends = list(range(WINDOW, len(audio) + 1, HOP))
        features = np.concatenate([frontend.extract([audio[e - WINDOW:e] for e in ends[s:s + 16]]) for s in range(0, len(ends), 16)])
        row = {'index': idx, 'group': group, 'source': source, 'seconds': len(audio) / SR, 'positive': positive, 'audio_sha256': __import__('hashlib').sha256(audio.tobytes()).hexdigest()}
        for name, model in models.items():
            scores = predict(model, features)
            scores_saved[f'{name}_{idx}'] = scores
            events = event_indices(scores)
            times = [(WINDOW + HOP * i) / SR for i in events]
            # Positives are placed to end at 2.75s; unrelated early triggers are not true detections.
            valid = [t for t in times if 1.8 <= t <= 3.8] if positive else []
            entry = result[name].setdefault(group, {'clips': 0, 'detected': 0, 'events': 0, 'seconds': 0.0, 'false_events': 0})
            entry['clips'] += 1
            entry['detected'] += int(bool(valid))
            entry['events'] += len(events)
            entry['false_events'] += len(events) - min(1, len(valid))
            entry['seconds'] += len(audio) / SR
            row[name] = times
        rows.append(row)
        if idx % 20 == 0:
            print('STREAM', idx, '/', len(scenarios), flush=True)
    save_json(output_prefix + 'stream_scenarios.json', rows)
    np.savez(RUN / (output_prefix + 'stream_scores.npz'), **scores_saved)
    return result


def evaluate():
    seed_all()
    training = json.loads((RUN / 'training_summary.json').read_text(encoding='utf-8'))
    assert sha256(RUN / 'candidate.pt') == training['candidate_pt_sha256'], '评估检查点与训练结果不符'
    data = dict(np.load(RUN / 'controlled_features.npz'))
    models = {'baseline': make_model(RUN / 'baseline_hi_neo.pt').cuda().eval(), 'candidate': make_model(RUN / 'candidate.pt').cuda().eval()}
    arrays = {k: v for k, v in data.items() if k.startswith(('test_', 'real_'))}
    arrays['legacy_positive'] = np.load(OLD / 'positive_features_test.npy')[:1000]
    arrays['legacy_negative'] = np.load(OLD / 'negative_features_test.npy')
    arrays['legacy_background'] = np.load(OLD / 'background_noise_features_test.npy')
    v = np.load(ROOT / 'data/features/validation_set_features.npy')
    arrays['acav_validation'] = v[:len(v) // 16 * 16].reshape(-1, 16, 96)[1::2]
    negative = {'test_negative', 'legacy_negative', 'legacy_background', 'acav_validation'}
    results = {name: {k: metric(predict(model, a), k not in negative) for k, a in arrays.items()} for name, model in models.items()}
    # Export using the legacy exporter to retain the deployed dynamic-batch signature.
    candidate = models['candidate'].cpu()
    torch.backends.mha.set_fastpath_enabled(False)
    torch.onnx.export(candidate, torch.randn(2, 16, 96), str(RUN / 'candidate.onnx'), input_names=['embeddings'], output_names=['score'], dynamic_axes={'embeddings': {0: 'batch'}, 'score': {0: 'batch'}}, opset_version=18, dynamo=False)
    import onnx
    onnx.checker.check_model(onnx.load(RUN / 'candidate.onnx'))
    opt = ort.SessionOptions()
    opt.intra_op_num_threads = 2
    sessions = {name: ort.InferenceSession(str(RUN / filename), opt, providers=['CPUExecutionProvider']) for name, filename in [('baseline', 'baseline_hi_neo.onnx'), ('candidate', 'candidate.onnx')]}
    parity = {}
    sample = np.concatenate([arrays['test_clean'][:16], arrays['test_snr0'][:16], arrays['test_negative'][:16]])
    for name, session in sessions.items():
        model = models[name].cpu()
        differences = []
        for n in [1, 2, len(sample)]:
            with torch.no_grad():
                expected = model(torch.from_numpy(sample[:n])).numpy()
            actual = session.run(None, {'embeddings': sample[:n]})[0]
            assert np.isfinite(actual).all()
            differences.append(float(np.max(np.abs(actual - expected))))
        assert max(differences) < 1e-4
        parity[name] = {'max_abs_error': max(differences), 'input': session.get_inputs()[0].shape, 'output': session.get_outputs()[0].shape, 'finite': True}
        model.cuda()
    assert parity['baseline']['input'] == parity['candidate']['input']
    assert parity['baseline']['output'] == parity['candidate']['output']
    save_json('offline_results.json', results)
    save_json('onnx_validation.json', parity)
    stream = stream_eval(models, json.loads((RUN / 'data_manifest.json').read_text(encoding='utf-8')))
    reasons = gate(results['baseline'], results['candidate'])
    for group, base in stream['baseline'].items():
        cand = stream['candidate'][group]
        if cand['false_events'] > base['false_events']:
            reasons.append('stream_' + group + '_false_events_increased')
        if ('synthetic' in group or 'real_' in group) and cand['detected'] < base['detected']:
            reasons.append('stream_' + group + '_recall_regression')
    if not json.loads((RUN / 'training_summary.json').read_text())['selection_feasible']:
        reasons.append('no_feasible_development_checkpoint')
    # Even a numerical pass is not school acceptance: require independent real session evidence.
    reasons.append('independent_real_session_and_classroom_negative_validation_unavailable')
    initial = json.loads((RUN / 'inspection.json').read_text(encoding='utf-8'))['models']
    unchanged = all(sha256(ASSETS / name) == initial[str((ASSETS / name).relative_to(ROOT.parent))]['sha256'] for name in ['hi_neo.onnx', 'melspectrogram.onnx', 'embedding_model.onnx'])
    assert unchanged
    result = {'threshold': THRESHOLD, 'confirm_frames': 2, 'debounce_seconds': 2, 'stream_hop_seconds': 0.08, 'window_seconds': 2, 'offline': results, 'stream': stream, 'onnx': parity, 'deployed': False, 'production_unchanged': unchanged, 'rejection_reasons': reasons, 'baseline_sha256': sha256(RUN / 'baseline_hi_neo.onnx'), 'candidate_sha256': sha256(RUN / 'candidate.onnx'), 'limitations': ['Offline independent clips are NOT continuous classroom hours.', 'ACAV feature frame duration/continuity is not assumed; report counts, not false alarms/hour.', 'Streaming uses exact frozen frontend/window/hop/confirm/clear behavior, but not microphone capture, resampler drift or callback scheduling.', 'Synthetic positives overlap historical speaker identities. Real diagnostics comprise only two utterances from the same session as personalization data.', 'Legacy adversarial evaluation may contain untraceable homophones; reported separately and not used for fine-tuning.']}
    result['candidate_pt_sha256'] = training['candidate_pt_sha256']
    result['training_summary_sha256'] = sha256(RUN / 'training_summary.json')
    result['evaluation_status'] = '历史 test 曾参与模型选择，只作回归诊断，不是独立测试'
    save_json('results.json', result)
    print(json.dumps(result, ensure_ascii=True, indent=2), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('stage', choices=['prepare', 'train', 'retain', 'evaluate'])
    parser.add_argument('--steps', type=int)
    args = parser.parse_args()
    if args.stage == 'prepare':
        prepare()
    elif args.stage == 'train':
        train(args.steps if args.steps is not None else 4000)
    elif args.stage == 'retain':
        from refine_candidate import train_trials
        train_trials(args.steps if args.steps is not None else 2500)
    else:
        evaluate()
