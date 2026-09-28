"""从原基线出发的保留约束实验；仅使用训练集和开发集选点。"""
import argparse
import hashlib
import json
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
import torch
import torch.nn.functional as F

import robust_experiment as r
from optimize_wake import ASSETS, OLD, ROOT, RUN, SEED, sha256

OUT = RUN / 'retention_v1'


def write_json(path, value):
    r.save_json(path, value)


def save_checkpoint(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    torch.save(value, temporary)
    temporary.replace(path)


def verify_trial(directory, trial, steps):
    assert trial['steps'] == steps, '已完成策略步数与请求不符，禁止混轮'
    assert sha256(directory / 'last.pt') == trial['last_pt_sha256'], '末点哈希不符'
    history = json.loads((directory / 'history.json').read_text(encoding='utf-8'))
    assert history[-1]['step'] == steps
    assert history[-1]['model_sha256'] == trial['last_pt_sha256']
    if trial['selection_feasible']:
        assert sha256(directory / 'candidate.pt') == trial['candidate_pt_sha256'], '选点哈希不符'
        selected = next(row for row in history if row['step'] == trial['selected_step'])
        assert selected['candidate_sha256'] == trial['candidate_pt_sha256']
    return trial


def parity(model, path, arrays):
    opt = ort.SessionOptions()
    opt.intra_op_num_threads = 2
    session = ort.InferenceSession(str(path), opt, providers=['CPUExecutionProvider'])
    model.eval()
    device = next(model.parameters()).device
    errors = []
    for a in arrays:
        r.validate_features({'一致性输入': a})
        for n in [1, 2, min(64, len(a))]:
            x = np.array(a[:n], dtype=np.float32)
            with torch.no_grad():
                expected = model(torch.as_tensor(x, device=device)).cpu().numpy()
            actual = session.run(None, {session.get_inputs()[0].name: x})[0]
            assert np.isfinite(actual).all()
            errors.append(float(np.max(np.abs(expected - actual))))
    assert max(errors) < 1e-4
    assert session.get_inputs()[0].shape == ['batch', 16, 96]
    assert session.get_outputs()[0].shape == ['batch', 1]
    latency = {}
    for n in [1, 64]:
        x = np.array(arrays[0][:n], dtype=np.float32)
        feed = {session.get_inputs()[0].name: x}
        for _ in range(5):
            session.run(None, feed)
        durations = []
        for _ in range(50):
            start = time.perf_counter()
            session.run(None, feed)
            durations.append((time.perf_counter() - start) * 1000)
        latency[str(len(x))] = {'median_ms': float(np.median(durations)), 'p95_ms': float(np.percentile(durations, 95))}
    return {'max_abs_error': max(errors), 'finite': True, 'rust_signature_compatible': True,
            'ort_cpu_latency_by_batch': latency, 'latency_repeats': 50, 'ort_threads': 2}


def development_reasons(base, metrics):
    reasons = []
    for key in ['dev_clean', 'dev_legacy_positive']:
        if metrics[key]['hits'] < base[key]['hits']:
            reasons.append(key + '_regression')
    for key in ['dev_negative', 'dev_acav', 'dev_background']:
        if metrics[key]['hits'] > base[key]['hits']:
            reasons.append(key + '_false_positives_increased')
    if noise_score(metrics) < noise_score(base) + .02:
        reasons.append('noise_gain_less_than_2_percentage_points')
    return reasons


def noise_score(metrics):
    return float(np.mean([metrics[f'dev_snr{s}']['recall'] for s in [20, 10, 5, 0]]))


def metrics_for(model, data):
    return {k: r.metric(r.predict(model, v), not any(t in k for t in ['negative', 'acav', 'background'])) for k, v in data.items()}


def audit_and_replay(data, teacher):
    manifest = json.loads((RUN / 'data_manifest.json').read_text(encoding='utf-8'))
    known = {item['sha256'] for item in manifest['files']}
    new_noise = [p for p in sorted((ROOT / 'data/backgrounds').rglob('*.wav')) if sha256(p) not in known]
    # 新噪声只登记并封存，不用于优化、开发集或本轮诊断。
    sealed = {'opened_for_evaluation': False, 'noise': [{'sha256': sha256(p), 'path': str(p.relative_to(ROOT))} for p in new_noise],
              'independent_positive_sources_available': False,
              'limitation': '历史 legacy_positive 已评估全部原 positive_test；不能把剩余原音频伪称独立封存正例。'}
    write_json(OUT / 'sealed_manifest.json', sealed)
    source_sets = {k: {sha256(ROOT / p) for p in v} for k, v in manifest['positive_sources'].items()}
    for a, b in [('train', 'dev'), ('train', 'test'), ('dev', 'test')]:
        assert not source_sets[a] & source_sets[b]
    for key in ['noise_split', 'rir_split']:
        sets = {k: {sha256(ROOT / p) for p in v} for k, v in manifest[key].items()}
        assert not sets['train'] & (sets['dev'] | sets['test'])
        assert not sets['dev'] & sets['test']
    if not manifest['positive_sources']['train'] or not manifest['noise_split']['dev']:
        raise ValueError('保留检查缺少训练正例或开发噪声')
    cache = OUT / 'replay_features.npz'
    if not cache.exists():
        front = r.Frontend()
        rng = np.random.default_rng(SEED + 301)
        clips = [r.render(r.load_audio(ROOT / p), None, None, rng) for p in manifest['positive_sources']['train'][:1024]]
        clean = np.concatenate([front.extract(clips[i:i + 16]) for i in range(0, len(clips), 16)])
        clips = []
        noises = [ROOT / p for p in manifest['noise_split']['dev']]
        for i in range(256):
            a = r.segment(r.load_audio(noises[i % len(noises)]), r.WINDOW, rng)
            a /= max(1., float(np.max(np.abs(a))) / .95)
            clips.append(a)
        background = np.concatenate([front.extract(clips[i:i + 16]) for i in range(0, len(clips), 16)])
        np.savez(cache, clean=clean, dev_background=background)
    replay = dict(np.load(cache))
    r.validate_features(replay, ['clean', 'dev_background'])
    sampled = {k: v for k, v in data.items() if k.startswith('train_')}
    sampled['original_positive'] = np.load(OLD / 'positive_features_train.npy')[:10000]
    sampled['clean_replay'] = replay['clean']
    stats = {k: {'n': len(v), 'min': float(v.min()), 'max': float(v.max()), 'std': float(v.std()),
                  'finite': bool(np.isfinite(v).all())} for k, v in sampled.items()}
    for value in stats.values():
        assert value['finite']
    phrases = json.loads((RUN / 'negative_manifest.json').read_text(encoding='utf-8'))
    conflict = sorted({p['phrase'].lower().strip() for p in phrases} & {'hi neo', 'hey neo', 'hai neo', 'high neo'})
    assert not conflict
    audit = {'baseline_pt_sha256': sha256(RUN / 'baseline_hi_neo.pt'),
             'baseline_onnx_sha256': sha256(ASSETS / 'hi_neo.onnx'),
             'baseline_parity': parity(teacher, ASSETS / 'hi_neo.onnx', [data['dev_clean'], data['dev_snr0'], data['dev_negative'], replay['clean']]),
             'model': 'conv_attention_small_strict_state_dict', 'feature_scale': stats,
             'exact_negative_label_conflicts': conflict,
             'excluded_legacy_adversarial': True,
             'limitations': ['近音词难度不等于已证实标签冲突；旧 adversarial 缺少短语溯源，不参与优化。',
                             '原 test 已暴露，仅作诊断；没有新的独立真人会话或教室连续负例。'],
             'source_hash_split_disjoint': True, 'sealed_new_noise_count': len(new_noise),
             'frontend_scale': '归一化 float32 PCM；冻结 mel /10+2；embedding 不重新缩放'}
    write_json(OUT / 'audit.json', audit)
    return replay, audit, manifest


def train_trials(steps):
    assert steps > 0 and torch.cuda.is_available()
    OUT.mkdir(parents=True, exist_ok=True)
    assert not (OUT / 'summary.json').exists(), '已有实验结果，不能覆盖'
    r.seed_all(SEED + 301)
    initial_assets = {p.name: sha256(p) for p in ASSETS.glob('*.onnx')}
    assert sha256(RUN / 'baseline_hi_neo.pt') == sha256(OLD / 'hi_neo.pt')
    assert sha256(RUN / 'baseline_hi_neo.onnx') == sha256(ASSETS / 'hi_neo.onnx')
    data = dict(np.load(RUN / 'controlled_features.npz'))
    r.validate_features(data, ['train_aug', 'train_real', 'train_negative', 'train_background', 'real_clean'])
    teacher = r.make_model(RUN / 'baseline_hi_neo.pt').cuda().eval().requires_grad_(False)
    replay, audit, manifest = audit_and_replay(data, teacher)
    acav = np.load(ROOT / 'data/features/openwakeword_features_ACAV100M_2000_hrs_16bit.npy', mmap_mode='r')
    pool = np.array(acav[np.load(RUN / 'acav_pool_indices.npy')], dtype=np.float32)
    pool_scores = r.predict(teacher, pool)
    hard = pool[np.argsort(pool_scores)[-4096:]]
    original = np.load(OLD / 'positive_features_train.npy')[:10000]
    # 原训练 replay 末段只作本轮保留检查，历史训练暴露不能变成独立测试。
    groups = {'clean': replay['clean'], 'old': original[:9000], 'aug': data['train_aug'],
              'real': data['train_real'], 'negative': data['train_negative'],
              'background': np.concatenate([data['train_background'], np.load(OLD / 'background_noise_features_train.npy')]),
              'acav': pool, 'hard': hard}
    dev = {k: v for k, v in data.items() if k.startswith('dev_')}
    v = np.load(ROOT / 'data/features/validation_set_features.npy')
    dev['dev_acav'] = v[:len(v) // 16 * 16].reshape(-1, 16, 96)[::2]
    dev['dev_legacy_positive'] = original[9000:]
    dev['dev_background'] = replay['dev_background']
    base = metrics_for(teacher, dev)
    write_json(OUT / 'dev_baseline.json', base)
    targets = {k: torch.as_tensor(r.predict(teacher, a)[:, None], device='cuda') for k, a in groups.items()}
    tensors = {k: torch.as_tensor(a, device='cuda') for k, a in groups.items()}
    specs = [('clean', 64, 1), ('old', 64, 1), ('aug', 64, 1), ('real', 16, 1),
             ('negative', 64, 0), ('background', 32, 0), ('acav', 80, 0), ('hard', 32, 0)]
    strategies = [dict(name='frozen_conv_distill', lr=3e-6, freeze_conv=True, kd=.15, aug_weight=.4),
                  dict(name='full_low_lr_distill', lr=1e-6, freeze_conv=False, kd=.15, aug_weight=.4),
                  dict(name='full_strong_retention', lr=2e-6, freeze_conv=False, kd=.6, aug_weight=.25)]
    protocol = {'steps_per_trial': steps, 'strategies': strategies, 'batch': specs,
                'threshold': r.THRESHOLD, 'selection': '仅 dev；干净命中不减、负例不增、平均噪声召回至少提升 0.02',
                'final_test_status': '历史 test 曾参与模型选择，只作回归诊断，不独立', 'seed': SEED + 301,
                'inputs_sha256': {name: sha256(path) for name, path in {
                    'baseline': RUN / 'baseline_hi_neo.pt', 'features': RUN / 'controlled_features.npz',
                    'pool_indices': RUN / 'acav_pool_indices.npy', 'replay': OUT / 'replay_features.npz',
                    'pool_content': ROOT / 'data/features/openwakeword_features_ACAV100M_2000_hrs_16bit.npy',
                    'manifest': RUN / 'data_manifest.json',
                    'original_positive': OLD / 'positive_features_train.npy',
                    'original_background': OLD / 'background_noise_features_train.npy',
                    'validation': ROOT / 'data/features/validation_set_features.npy'}.items()},
                'scripts_sha256': {p.name: sha256(p) for p in [Path(__file__), Path(r.__file__), ROOT / 'optimize_wake.py']}}
    protocol = json.loads(json.dumps(protocol))
    if (OUT / 'protocol.json').exists():
        assert json.loads((OUT / 'protocol.json').read_text(encoding='utf-8')) == protocol, '实验协议或输入已变化，禁止混轮'
    else:
        write_json(OUT / 'protocol.json', protocol)
    protocol_sha256 = sha256(OUT / 'protocol.json')
    trials = []
    for config in strategies:
        r.seed_all(SEED + 301)
        directory = OUT / config['name']
        directory.mkdir(exist_ok=True)
        if (directory / 'summary.json').exists():
            trial = verify_trial(directory, json.loads((directory / 'summary.json').read_text(encoding='utf-8')), steps)
            assert trial['protocol_sha256'] == protocol_sha256, '已完成策略协议不符'
            trials.append(trial)
            print(config['name'], 'already complete; skipped', flush=True)
            continue
        model = r.make_model(RUN / 'baseline_hi_neo.pt').cuda()
        if config['freeze_conv']:
            model.classifier.conv.requires_grad_(False)
        params = [p for p in model.parameters() if p.requires_grad]
        anchors = [p.detach().clone() for p in params]
        optimizer = torch.optim.AdamW(params, lr=config['lr'], weight_decay=0.)
        scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, steps, eta_min=config['lr'] * .2)
        best = -1.
        selected = 0
        history = []
        train_gpu_ms = 0.
        start_step, previous_wall, previous_peak = 0, 0., 0
        if (directory / 'resume.pt').exists():
            state = torch.load(directory / 'resume.pt', map_location='cpu', weights_only=True)
            assert state['protocol_sha256'] == protocol_sha256, '断点协议不符'
            model.load_state_dict(state['model'])
            optimizer.load_state_dict(state['optimizer'])
            scheduler.load_state_dict(state['scheduler'])
            torch.set_rng_state(state['torch_rng'])
            torch.cuda.set_rng_state_all(state['cuda_rng'])
            history, selected, best = state['history'], state['selected'], state['best']
            start_step = state['step']
            train_gpu_ms = state['gpu_training_seconds'] * 1000
            previous_wall, previous_peak = state['wall_seconds'], state['peak_bytes']
            assert sha256(directory / 'last.pt') == history[-1]['model_sha256'], '断点末点哈希不符'
        elif (directory / 'history.json').exists():
            raise RuntimeError('存在历史但无可恢复断点，禁止静默重训')
        torch.cuda.reset_peak_memory_stats()
        torch.cuda.synchronize()
        start = time.perf_counter()
        for step in range(start_step + 1, steps + 1):
            model.train()
            begin, end = torch.cuda.Event(enable_timing=True), torch.cuda.Event(enable_timing=True)
            begin.record()
            xx, yy, tt, weights = [], [], [], []
            for name, count, label in specs:
                idx = torch.randint(len(tensors[name]), (count,), device='cuda')
                xx.append(tensors[name][idx])
                yy.append(torch.full((count, 1), float(label), device='cuda'))
                tt.append(targets[name][idx])
                weights.append(torch.full((count, 1), config['aug_weight'] if name in ['aug', 'real'] else 1., device='cuda'))
            x, y, target, w = map(torch.cat, [xx, yy, tt, weights])
            scores = model(x).clamp(1e-5, 1 - 1e-5)
            # 蒸馏原干净正例及所有负例；增强正例允许摆脱教师的噪声漏检。
            mask = torch.ones_like(y)
            mask[128:208] = 0
            kd = ((torch.logit(scores) - torch.logit(target.clamp(1e-5, 1 - 1e-5))) ** 2 * mask).sum() / mask.sum()
            retention = sum((p - a).square().sum() for p, a in zip(params, anchors)) / sum(p.numel() for p in params)
            loss = (F.binary_cross_entropy(scores, y, reduction='none') * w).mean() + config['kd'] * kd + 100 * retention
            optimizer.zero_grad(set_to_none=True)
            loss.backward()
            torch.nn.utils.clip_grad_norm_(params, 1.)
            optimizer.step()
            scheduler.step()
            end.record()
            end.synchronize()
            train_gpu_ms += begin.elapsed_time(end)
            if step % 250 == 0 or step == steps:
                current = metrics_for(model, dev)
                reasons = development_reasons(base, current)
                score = noise_score(current)
                save_checkpoint(directory / 'last.pt', r.cpu_state_dict(model))
                row = {'step': step, 'loss': float(loss.item()), 'metrics': current, 'rejection_reasons': reasons,
                       'noise_recall': score, 'gpu_training_seconds': train_gpu_ms / 1000,
                       'synchronized_wall_seconds': previous_wall + time.perf_counter() - start,
                       'model_sha256': sha256(directory / 'last.pt')}
                history.append(row)
                if not reasons and score > best:
                    best, selected = score, step
                    save_checkpoint(directory / 'candidate.pt', r.cpu_state_dict(model))
                    row['candidate_sha256'] = sha256(directory / 'candidate.pt')
                save_checkpoint(directory / 'resume.pt', {
                    'step': step, 'model': r.cpu_state_dict(model), 'optimizer': optimizer.state_dict(),
                    'scheduler': scheduler.state_dict(), 'torch_rng': torch.get_rng_state(),
                    'cuda_rng': torch.cuda.get_rng_state_all(), 'history': history, 'selected': selected,
                    'best': best, 'protocol_sha256': protocol_sha256, 'gpu_training_seconds': train_gpu_ms / 1000,
                    'wall_seconds': previous_wall + time.perf_counter() - start,
                    'peak_bytes': max(previous_peak, torch.cuda.max_memory_allocated())})
                write_json(directory / 'history.json', history)
                print(config['name'], step, 'clean', current['dev_clean']['hits'], 'negative', current['dev_negative']['hits'],
                      'acav', current['dev_acav']['hits'], 'noise', round(score, 4), 'selected', selected,
                      'gpu_s', round(train_gpu_ms / 1000, 1), flush=True)
        write_json(directory / 'history.json', history)
        torch.cuda.synchronize()
        trial = {**config, 'steps': steps, 'selected_step': selected or None,
                 'selection_feasible': bool(selected), 'gpu_training_seconds': train_gpu_ms / 1000,
                 'synchronized_wall_seconds': previous_wall + time.perf_counter() - start, 'gpu': torch.cuda.get_device_name(),
                 'gpu_peak_allocated_bytes': max(previous_peak, torch.cuda.max_memory_allocated()),
                 'protocol_sha256': protocol_sha256,
                 'starting_pt_sha256': audit['baseline_pt_sha256'], 'last_pt_sha256': sha256(directory / 'last.pt'),
                 'candidate_pt_sha256': sha256(directory / 'candidate.pt') if selected else None,
                 'selected_dev_metrics': next((row['metrics'] for row in history if row['step'] == selected), None),
                 'last_dev_metrics': history[-1]['metrics'], 'last_rejection_reasons': history[-1]['rejection_reasons']}
        write_json(directory / 'summary.json', trial)
        trials.append(trial)
        del model, optimizer
    feasible = [t for t in trials if t['selection_feasible']]
    chosen = max(feasible, key=lambda t: noise_score(t['selected_dev_metrics'])) if feasible else None
    # 无可行点时仅诊断开发集约束违例最少的末点，不把它称为选中模型。
    diagnostic = chosen or min(trials, key=lambda t: (len(t['last_rejection_reasons']), -noise_score(t['last_dev_metrics'])))
    filename = 'candidate.pt' if chosen else 'last.pt'
    candidate_path = OUT / diagnostic['name'] / filename
    candidate = r.make_model(candidate_path).cuda().eval()
    expected_hash = diagnostic['candidate_pt_sha256'] if chosen else diagnostic['last_pt_sha256']
    assert sha256(candidate_path) == expected_hash, '诊断模型与选点结果不符'
    diagnostic_result = evaluate_diagnostic(teacher, candidate, data, manifest)
    diagnostic_result['candidate_pt_sha256'] = expected_hash
    diagnostic_result['baseline_pt_sha256'] = audit['baseline_pt_sha256']
    reasons = list(diagnostic_result['rejection_reasons'])
    if not chosen:
        reasons.append('no_feasible_development_checkpoint')
    reasons.append('independent_real_session_and_classroom_negative_validation_unavailable')
    unchanged = all(sha256(ASSETS / name) == value for name, value in initial_assets.items())
    assert unchanged
    summary = {'schema_version': 1, 'threshold_fixed': r.THRESHOLD, 'additional_steps': steps * len(trials),
               'previous_steps': 12000, 'total_experiment_steps': 12000 + sum(t['steps'] for t in trials),
               'protocol_sha256': protocol_sha256, 'trials': trials, 'dev_baseline': base,
               'selected_trial': chosen['name'] if chosen else None, 'diagnostic_trial': diagnostic['name'],
               'deployed': False, 'production_unchanged': unchanged, 'decision': 'rejected',
               'rejection_reasons': reasons, 'audit': audit, 'diagnostic': diagnostic_result,
               'candidate_pt_sha256': sha256(candidate_path), 'script_sha256': sha256(__file__),
               'sealed_holdout': {'new_noise_count': audit['sealed_new_noise_count'], 'opened': False,
                                  'independent_positive_sources_available': False},
               'limitations': ['旧 test 曾参与历史模型选择，本轮仅为回归诊断，未用于本轮选点，也不是独立测试。',
                               '教师约束不能替代独立真人会话和教室持续负例验收。']}
    write_json(OUT / 'summary.json', summary)
    compact = {k: summary[k] for k in ['schema_version', 'threshold_fixed', 'additional_steps', 'previous_steps',
               'selected_trial', 'diagnostic_trial', 'deployed', 'production_unchanged', 'decision', 'rejection_reasons',
               'candidate_pt_sha256', 'script_sha256', 'sealed_holdout', 'limitations', 'total_experiment_steps', 'protocol_sha256']}
    compact['baseline_onnx_sha256'] = audit['baseline_onnx_sha256']
    compact['baseline_parity'] = audit['baseline_parity']
    compact['trials'] = [{k: t[k] for k in ['name', 'steps', 'selected_step', 'selection_feasible',
                           'gpu_training_seconds', 'synchronized_wall_seconds', 'gpu_peak_allocated_bytes',
                           'starting_pt_sha256', 'last_pt_sha256', 'candidate_pt_sha256',
                           'last_dev_metrics', 'last_rejection_reasons', 'selected_dev_metrics']} for t in trials]
    compact['dev_baseline'] = base
    compact['diagnostic'] = diagnostic_result
    write_json(ROOT / 'retention_metrics.json', compact)
    print(json.dumps({'selected_trial': summary['selected_trial'], 'rejection_reasons': reasons,
                      'additional_steps': summary['additional_steps']}, ensure_ascii=True), flush=True)


def evaluate_diagnostic(baseline, candidate, data, manifest):
    arrays = {k: v for k, v in data.items() if k.startswith(('test_', 'real_'))}
    for key, filename in [('legacy_positive', 'positive_features_test.npy'), ('legacy_negative', 'negative_features_test.npy'),
                          ('legacy_background', 'background_noise_features_test.npy')]:
        values = np.load(OLD / filename)
        arrays[key] = values[:1000] if key == 'legacy_positive' else values
    v = np.load(ROOT / 'data/features/validation_set_features.npy')
    arrays['acav_validation'] = v[:len(v) // 16 * 16].reshape(-1, 16, 96)[1::2]
    models = {'baseline': baseline, 'candidate': candidate}
    offline = {k: metrics_for(m, arrays) for k, m in models.items()}
    export_model = r.make_model(RUN / 'baseline_hi_neo.pt')
    export_model.load_state_dict(r.cpu_state_dict(candidate))
    export_model.eval()
    torch.backends.mha.set_fastpath_enabled(False)
    path = OUT / 'diagnostic.onnx'
    torch.onnx.export(export_model, torch.randn(2, 16, 96), str(path), input_names=['embeddings'], output_names=['score'],
                      dynamic_axes={'embeddings': {0: 'batch'}, 'score': {0: 'batch'}}, opset_version=18, dynamo=False)
    import onnx
    onnx.checker.check_model(onnx.load(path))
    validation = parity(candidate, path, [data['dev_clean'], data['dev_snr0'], data['dev_negative']])
    stream = r.stream_eval(models, manifest, output_prefix='retention_v1/')
    reasons = r.gate(offline['baseline'], offline['candidate'])
    # 本轮要求严格不退化，不接受旧门禁 0.5 个百分点的干净容差。
    for key in ['test_clean', 'legacy_positive']:
        if offline['candidate'][key]['hits'] < offline['baseline'][key]['hits'] and key + '_regression' not in reasons:
            reasons.append(key + '_regression')
    for key, base in stream['baseline'].items():
        cand = stream['candidate'][key]
        if cand['false_events'] > base['false_events']:
            reasons.append('stream_' + key + '_false_events_increased')
        if ('synthetic' in key or 'real_' in key) and cand['detected'] < base['detected']:
            reasons.append('stream_' + key + '_recall_regression')
    return {'status': 'previously_exposed_diagnostic_only', 'offline': offline, 'stream': stream,
            'onnx': validation, 'onnx_sha256': sha256(path), 'rejection_reasons': reasons,
            'stream_runtime': {'threshold': .25, 'confirm_frames': 2, 'hop_seconds': .08, 'window_seconds': 2}}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--steps', type=int, default=2500)
    train_trials(parser.parse_args().steps)
