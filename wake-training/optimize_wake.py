"""Local-only reproducible wake-head optimization; never accesses audio devices."""
from __future__ import annotations

import argparse
import hashlib
import json
import platform
from pathlib import Path

import numpy as np
import soundfile as sf
import torch

ROOT = Path(__file__).resolve().parent
ASSETS = ROOT.parent / 'crates' / 'neo-wake' / 'assets'
OLD = ROOT / 'output' / 'hi_neo'
RUN = ROOT / 'output' / 'robust_20260928'
SEED = 20260928


def sha256(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda: f.read(8 * 1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def save_json(name, data):
    path = RUN / name
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(data, indent=2, ensure_ascii=False, allow_nan=False), encoding='utf-8')
    temporary.replace(path)


def inspect():
    import importlib.metadata
    import onnxruntime as ort
    models = list(ASSETS.glob('*.onnx')) + list(OLD.glob('*.pt')) + list(OLD.glob('*.onnx'))
    models += list((ROOT / '.venv/Lib/site-packages/livekit/wakeword/resources').rglob('*.onnx'))
    arrays = list(OLD.glob('*.npy')) + list((ROOT / 'data/features').glob('*.npy'))
    data = {'python': platform.python_version(), 'executable': __import__('sys').executable,
            'torch': torch.__version__, 'cuda': torch.version.cuda,
            'cuda_available': torch.cuda.is_available(), 'gpu': torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,
            'packages': {n: importlib.metadata.version(n) for n in ['numpy', 'scipy', 'soundfile', 'onnx', 'onnxruntime', 'livekit-wakeword']},
            'models': {str(p.relative_to(ROOT.parent)): {'sha256': sha256(p), 'bytes': p.stat().st_size} for p in models},
            'arrays': {}, 'audio': {}, 'data_children': [str(p.relative_to(ROOT)) for p in (ROOT / 'data').iterdir()]}
    for p in arrays:
        a = np.load(p, mmap_mode='r')
        data['arrays'][str(p.relative_to(ROOT))] = {'shape': list(a.shape), 'dtype': str(a.dtype), 'bytes': p.stat().st_size}
    for directory in [OLD / n for n in ['positive_train', 'positive_test', 'negative_train', 'negative_test']] + [ROOT / 'data' / n for n in ['backgrounds', 'rirs', 'recordings']]:
        files = sorted(directory.rglob('*.wav'))
        data['audio'][str(directory.relative_to(ROOT))] = {'count': len(files), 'examples': []}
        for p in files[:3]:
            info = sf.info(p)
            data['audio'][str(directory.relative_to(ROOT))]['examples'].append({'path': str(p.relative_to(ROOT)), 'sr': info.samplerate, 'channels': info.channels, 'seconds': info.duration})
    state = torch.load(OLD / 'hi_neo.pt', map_location='cpu', weights_only=True)
    data['checkpoint'] = {k: list(v.shape) for k, v in state.items()}
    opt = ort.SessionOptions()
    opt.intra_op_num_threads = 2
    session = ort.InferenceSession(str(ASSETS / 'hi_neo.onnx'), opt, providers=['CPUExecutionProvider'])
    data['onnx_io'] = {'inputs': [(x.name, x.shape, x.type) for x in session.get_inputs()], 'outputs': [(x.name, x.shape, x.type) for x in session.get_outputs()]}
    if torch.cuda.is_available():
        x = torch.randn(512, 512, device='cuda')
        data['cuda_smoke_finite'] = bool(torch.isfinite(x @ x).all())
    save_json('inspection.json', data)
    print(json.dumps(data, indent=2, ensure_ascii=True), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('stage', choices=['inspect'])
    args = parser.parse_args()
    inspect()
