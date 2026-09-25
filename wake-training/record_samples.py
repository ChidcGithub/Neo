# One-shot recorder for hi_neo training samples: mic in, features out.
#
# 用法（在 wake-training/ 目录下）：
#   .venv/Scripts/python.exe record_samples.py              # 录 16 句「嗨，Neo」
#   .venv/Scripts/python.exe record_samples.py -n 20        # 录 20 句
#   .venv/Scripts/python.exe record_samples.py --negative   # 录日常用语当负样本
#   .venv/Scripts/python.exe record_samples.py --train      # 录完直接重训
#
# 每句流程：Enter 准备 → 录 2 秒 → 立刻用现有模型打分 → Enter 保留 / r 重录。
# 正样本最后 2 句自动进验证集；原始 wav 存档在 data/recordings/ 以便将来复用。
# 录完若想立刻重训：train（分类头，分钟级）→ export → 替换 app 侧 hi_neo.onnx。

from __future__ import annotations

import argparse
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

from import_recordings import ASSETS, OUT_DIR, SPLITS, SAMPLE_RATE, align_to_end, extract, trim_silence

RECORD_SECONDS = 2.0
RECORDINGS_DIR = Path(__file__).resolve().parent / "data" / "recordings"

# 正样本得分低于它建议重录；负样本得分高于它反而是最有价值的对抗样本。
LOW_SCORE = 0.15
HOT_NEGATIVE = 0.20


def record_one(seconds: float = RECORD_SECONDS) -> np.ndarray:
    """录 seconds 秒 16kHz 单声道。"""
    import sounddevice as sd

    audio = sd.rec(
        int(seconds * SAMPLE_RATE),
        samplerate=SAMPLE_RATE,
        channels=1,
        dtype="float32",
        blocking=True,
    )
    return audio[:, 0]


def score_clip(clip: np.ndarray, session) -> float:
    """用现有 hi_neo.onnx 给一句录音打分（0~1，越高越像唤醒词）。"""
    feats = extract([clip])  # (1, 16, 96)
    input_name = session.get_inputs()[0].name
    out = session.run(None, {input_name: feats.astype(np.float32)})
    return float(np.asarray(out[0]).reshape(-1)[0])


def save_wav(path: Path, audio: np.ndarray) -> None:
    import soundfile as sf

    path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(path), audio, SAMPLE_RATE)


def main() -> int:
    ap = argparse.ArgumentParser(description="Record hi_neo samples straight into training")
    ap.add_argument("-n", "--count", type=int, default=16, help="正样本句数（默认 16，末 2 句进验证集）")
    ap.add_argument("--negative", action="store_true", help="负样本模式：录日常用语（别说唤醒词）")
    ap.add_argument("--train", action="store_true", help="录完直接重训并替换 app 模型")
    args = ap.parse_args()

    import onnxruntime as ort

    split_train = "negative_train" if args.negative else "positive_train"
    kind = "negative" if args.negative else "positive"
    prompt = "随便说一句日常的话（别说唤醒词）" if args.negative else "说：「嗨，Neo」"
    session = ort.InferenceSession(str(ASSETS / "hi_neo.onnx"), providers=["CPUExecutionProvider"])

    n_val = 0 if args.negative else min(2, max(0, args.count // 8))
    print(f"准备录 {args.count} 句（{'负样本' if args.negative else f'正样本，其中 {n_val} 句进验证集'}）")
    print("每句 2 秒。说得自然一点，远近响度可以换着来。\n")

    kept: list[tuple[np.ndarray, float, str]] = []  # (clip, score, split)
    i = 0
    while i < args.count:
        split = split_train if (args.negative or i < args.count - n_val) else "positive_test"
        input(f"[{i + 1}/{args.count}] Enter 后开始 —— {prompt}")
        print("  录音中…")
        clip = align_to_end(trim_silence(record_one()))
        peak = float(np.abs(clip).max())
        score = score_clip(clip, session)

        if peak < 0.01:
            print("  太轻了，几乎没收到声音，重来。\n")
            continue
        if args.negative:
            tag = "高分负样本，好东西" if score >= HOT_NEGATIVE else "ok"
            print(f"  得分 {score:.3f}（负样本越低越好；{tag}）")
        else:
            print(f"  得分 {score:.3f}（阈值 0.2，越高越稳）")
        if not args.negative and score < LOW_SCORE:
            choice = input("  这句得分偏低，Enter 保留 / r 重录：").strip().lower()
            if choice == "r":
                print()
                continue
        kept.append((clip, score, split))
        i += 1
        print()

    # 存档原始 wav + 追加特征
    stamp = time.strftime("%Y%m%d-%H%M%S")
    clips_by_split: dict[str, list[np.ndarray]] = {}
    for idx, (clip, score, split) in enumerate(kept):
        save_wav(RECORDINGS_DIR / stamp / f"{kind}_{idx:02d}_{split}_s{score:.2f}.wav", clip)
        clips_by_split.setdefault(split, []).append(clip)

    for split, clips in clips_by_split.items():
        target = OUT_DIR / SPLITS[split]
        backup = target.with_suffix(".npy.bak")
        if target.exists() and not backup.exists():
            import shutil

            shutil.copy2(target, backup)
        feats = extract(clips)
        existing = np.load(target) if target.exists() else np.zeros((0, 16, 96), np.float32)
        merged = np.concatenate([existing, feats], axis=0)
        np.save(target, merged.astype(np.float32))
        print(f"{split}: {existing.shape[0]} -> {merged.shape[0]}")

    scores = [s for _, s, _ in kept]
    print(f"\n录音存档：data/recordings/{stamp}/")
    print(f"本次得分：min {min(scores):.3f} / 均值 {sum(scores) / len(scores):.3f} / max {max(scores):.3f}")
    if not args.negative:
        print("如还有容易误触发的日常话，再跑一遍加 --negative 录几句。")

    go = args.train or input("\n现在重训分类头吗？[y/N] ").strip().lower() == "y"
    if not go:
        print("稍后可手动：train → export → 替换 assets（见 import_recordings.py 头部注释）")
        return 0

    exe = ROOT / ".venv" / "Scripts" / "livekit-wakeword.exe"
    # hi_neo.yaml 里的 data_dir/output_dir 是相对配置文件目录的，钉死 cwd。
    yaml = ROOT / "hi_neo.yaml"
    for step in ("train", "export"):
        print(f"\n== livekit-wakeword {step} ==")
        r = subprocess.run([str(exe), step, str(yaml)], cwd=ROOT)
        if r.returncode != 0:
            print(f"{step} 失败（{r.returncode}）")
            return r.returncode
    dst = ASSETS / "hi_neo.onnx"
    import shutil

    shutil.copy2(OUT_DIR / "hi_neo.onnx", dst)
    print(f"\n已替换 {dst}")
    print("重训后留意它打印的建议阈值，必要时同步 crates/neo-wake 的默认阈值。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
