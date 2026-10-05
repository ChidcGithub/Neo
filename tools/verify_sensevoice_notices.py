"""Offline SenseVoice integrity-only checks, never license/release approval.

Default: root docs only. --package: also require byte-identical delivered docs
and pinned model/tokens bytes. Official licensing conflicts remain unresolved;
success does not change distribution gates. No downloads or model execution.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import stat

ROOT = Path(__file__).resolve().parents[1]
DOCS = 'docs/licenses/models/'
NOTICE = DOCS + 'SENSEVOICE-NOTICE.txt'
LICENSE_PINS = {
    DOCS + 'FunASR-MODEL_LICENSE':
        '7dba975a2069691db4992b0592d70828b330d2f8a30a71450f4e152a554e84f8',
    DOCS + 'FunASR-MODEL_LICENSE-v1.0':
        '80f5bff3bc3f1b4ba7128e07a7bf94ac10ca260b64059dfdc66e83202bcae50e',
}
DOCUMENTS = (
    NOTICE, *LICENSE_PINS,
    DOCS + 'ATTRIBUTIONS.txt',
    DOCS + 'sherpa-sense-LICENSE',
    DOCS + 'sherpa-sense-README.md',
    DOCS + 'sensevoice-20240731-card.md',
)
# SHA-256 of actual crates/neo-stt/assets/sense-voice files, read locally.
# verify_model_artifacts.py identifies these members of its pinned SenseVoice
# archive; the archive's SHA-256 is NOT the extracted ONNX member's hash.
MODEL_PINS = {
    'resources/models/stt/sense-voice/model.int8.onnx':
        'c71f0ce00bec95b07744e116345e33d8cbbe08cef896382cf907bf4b51a2cd51',
    'resources/models/stt/sense-voice/tokens.txt':
        'f449eb28dc567533d7fa59be34e2abca8784f771850c78a47fb731a31429a1dc',
}


def file_record(root, name):
    path = Path(root).absolute() / name
    # Inspect before resolving: even a regular file below a junction is a link.
    for component in (*reversed(path.parents), path):
        info = component.lstat()
        if (stat.S_ISLNK(info.st_mode)
                or getattr(info, 'st_file_attributes', 0) & 0x400):
            raise ValueError('Linked/reparse path is not allowed: ' + str(component))
    if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError('Linked/non-regular file is not allowed: ' + str(path))
    h, size = hashlib.sha256(), 0
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            h.update(block)
            size += len(block)
    if not size:
        raise ValueError('Empty required file: ' + str(path))
    return {'size': size, 'sha256': h.hexdigest()}


def verify(root, package=None):
    documents = {}
    for name in DOCUMENTS:
        record = file_record(root, name)
        if name in LICENSE_PINS and record['sha256'] != LICENSE_PINS[name]:
            raise ValueError('Root license SHA-256 mismatch: ' + name)
        if package is not None and file_record(package, name) != record:
            raise ValueError('Package/root document bytes differ: ' + name)
        documents[name] = record
    models = {}
    if package is not None:
        for name, expected in MODEL_PINS.items():
            record = file_record(package, name)
            if record['sha256'] != expected:
                raise ValueError('Package model/tokens SHA-256 mismatch: ' + name)
            models[name] = record
    return {'mode': 'integrity-only', 'status': 'verified',
            'release_clearance': False,
            'scope': 'root-docs' if package is None else 'root-docs-and-package',
            'documents': documents, 'models': models}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, help='Unpacked package directory to verify read-only')
    args = parser.parse_args(argv)
    try:
        report = verify(ROOT, args.package)
    except (OSError, ValueError) as error:
        report = {'mode': 'integrity-only', 'status': 'failed',
                  'release_clearance': False, 'error': str(error)}
    print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'verified' else 1


if __name__ == '__main__':
    raise SystemExit(main())
