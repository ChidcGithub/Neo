# Import your own recordings into the hi_neo training features.
#
# 用法（在 wake-training/ 目录下）：
#   .venv/Scripts/python.exe import_recordings.py 录音.wav 更多录音.wav
#   .venv/Scripts/python.exe import_recordings.py recordings/ --split positive_test
#   .venv/Scripts/python.exe import_recordings.py 误触发词.wav --split negative_train
#
# 做法：不碰 TTS 合成与增强（那几个钟头的活全省），直接把录音过一遍
# 冻结的 mel → embedding 管线，特征追加进 output/hi_neo/*_features_*.npy，
# 然后只需重训分类头：
#   .venv/Scripts/livekit-wakeword.exe train hi_neo.yaml
#   .venv/Scripts/livekit-wakeword.exe export hi_neo.yaml
#   copy /Y output\hi_neo\hi_neo.onnx ..\crates\neo-wake\assets\hi_neo.onnx
#
# 录音建议（正样本）：
#   - 10~20 句「嗨，Neo / Hi, Neo」，不同语速、远近、响度；
#   - 留 2~3 句 --split positive_test 当验证（别全进训练）；
#   - 容易误触发的日常话录几句 --split negative_train 当负样本。
# 录音格式：wav/flac 均可（m4a 请先用别的工具转 wav）。

from __future__ import annotations

import argparse
import random
import shutil
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent
# 冻结管线用的两个 ONNX 直接借 app 侧 assets 里的那份（同一模型，别复制两份）。
ASSETS = ROOT.parent / "crates" / "neo-wake" / "assets"
OUT_DIR = ROOT / "output" / "hi_neo"

SAMPLE_RATE = 16000
CLIP_SECONDS = 2.0
CLIP_SAMPLES = int(SAMPLE_RATE * CLIP_SECONDS)
# 与 augment.py 的约定一致：唤醒词贴尾 + 0~200ms 随机抖动。
JITTER_SAMPLES = 3200

SPLITS = {
    "positive_train": "positive_features_train.npy",
    "positive_test": "positive_features_test.npy",
    "negative_train": "negative_features_train.npy",
}


def load_audio(path: Path) -> np.ndarray:
    """读 wav/flac → 单声道 float32 → 重采样到 16kHz。"""
    import soundfile as sf
    from scipy.signal import resample_poly

    audio, sr = sf.read(str(path), always_2d=False)
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    audio = audio.astype(np.float32)
    if sr != SAMPLE_RATE:
        from math import gcd

        g = gcd(SAMPLE_RATE, sr)
        audio = resample_poly(audio, SAMPLE_RATE // g, sr // g).astype(np.float32)
    return audio


def trim_silence(audio: np.ndarray) -> np.ndarray:
    """去头尾静音：包络低于峰值 8% 的段剪掉。"""
    if audio.size == 0:
        return audio
    peak = float(np.abs(audio).max())
    if peak < 1e-4:
        return audio
    mask = np.abs(audio) > peak * 0.08
    idx = np.flatnonzero(mask)
    if idx.size == 0:
        return audio
    # 首尾各留 100ms 余量，别把起音/尾音切秃。
    pad = int(0.1 * SAMPLE_RATE)
    lo = max(0, int(idx[0]) - pad)
    hi = min(audio.size, int(idx[-1]) + pad)
    return audio[lo:hi]


def align_to_end(audio: np.ndarray) -> np.ndarray:
    """对齐到 2s 窗口末尾（augment.py 的约定：正样本贴尾 + 随机抖动）。"""
    out = np.zeros(CLIP_SAMPLES, dtype=np.float32)
    jitter = random.randint(0, JITTER_SAMPLES)
    end = CLIP_SAMPLES - jitter
    start = max(0, end - audio.size)
    src = max(0, audio.size - (end - start))
    out[start:end] = audio[src : src + (end - start)]
    return out


def extract(clips: list[np.ndarray]) -> np.ndarray:
    """音频 → (N, 16, 96) 特征（与 features.py 同一条冻结管线）。"""
    from livekit.wakeword.data.features import _pad_or_truncate
    from livekit.wakeword.models.feature_extractor import (
        MelSpectrogramFrontend,
        SpeechEmbedding,
    )

    mel_frontend = MelSpectrogramFrontend(ASSETS / "melspectrogram.onnx")
    speech_embedding = SpeechEmbedding(ASSETS / "embedding_model.onnx")

    feats = []
    for clip in clips:
        mel = mel_frontend(clip)
        emb = speech_embedding.extract_embeddings(mel)
        feats.append(_pad_or_truncate(emb[0]))
    return np.stack(feats, axis=0)


def main() -> int:
    ap = argparse.ArgumentParser(description="Import own recordings into hi_neo features")
    ap.add_argument("inputs", nargs="+", help="wav/flac files or directories")
    ap.add_argument(
        "--split",
        choices=sorted(SPLITS),
        default="positive_train",
        help="target feature file (default: positive_train)",
    )
    ap.add_argument("--dry-run", action="store_true", help="extract only, don't write")
    args = ap.parse_args()

    files: list[Path] = []
    for raw in args.inputs:
        p = Path(raw)
        if p.is_dir():
            files.extend(sorted(p.glob("*.wav")) + sorted(p.glob("*.flac")))
        elif p.is_file():
            files.append(p)
        else:
            print(f"!! not found: {raw}")
    if not files:
        print("no wav/flac files given")
        return 1

    clips = []
    for f in files:
        audio = align_to_end(trim_silence(load_audio(f)))
        clips.append(audio)
        voiced = float(np.abs(audio).max())
        print(f"  {f.name}: peak={voiced:.3f}")
        if voiced < 0.01:
            print("    ^^ 太轻了，基本是静音，建议重录")

    feats = extract(clips)
    print(f"extracted {feats.shape[0]} clips -> {feats.shape[1]}x{feats.shape[2]} features")
    if args.dry_run:
        print("dry-run, nothing written")
        return 0

    target = OUT_DIR / SPLITS[args.split]
    backup = target.with_suffix(".npy.bak")
    if target.exists() and not backup.exists():
        shutil.copy2(target, backup)
        print(f"backup: {backup.name}")
    existing = np.load(target) if target.exists() else np.zeros((0, 16, 96), np.float32)
    merged = np.concatenate([existing, feats], axis=0)
    np.save(target, merged.astype(np.float32))
    print(f"{args.split}: {existing.shape[0]} -> {merged.shape[0]} clips")
    print()
    print("下一步（重训分类头，不用再跑 TTS 合成）：")
    print("  .venv/Scripts/livekit-wakeword.exe train hi_neo.yaml")
    print("  .venv/Scripts/livekit-wakeword.exe export hi_neo.yaml")
    print("  copy /Y output\\hi_neo\\hi_neo.onnx ..\\crates\\neo-wake\\assets\\hi_neo.onnx")
    return 0


if __name__ == "__main__":
    sys.exit(main())
