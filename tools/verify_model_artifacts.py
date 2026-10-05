"""Pinned CI model evidence; no extraction, microphone, training or license approval.

Network is opt-in. Each curl invocation has one 180-second total deadline and no
retry. Existing source files are verified in place, never replaced. Reports are
private by default. Standard library only; synthetic smoke is a separate step.
"""
from __future__ import annotations

import argparse
import bz2
import hashlib
import io
import json
import re
import subprocess
import tarfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/'
PREFIX = 'sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17'
ARTIFACTS = (
    ('sv.tar.bz2', PREFIX + '.tar.bz2', 163002883,
     '7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e'),
    ('silero_vad.onnx', 'silero_vad.onnx', 643854,
     '9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6'),
)
NETWORK_BUDGET = 200 * 1024 * 1024
MAX_EXPANDED = 512 * 1024 * 1024
MAX_MEMBER = 256 * 1024 * 1024
MAX_MEMBERS = 128


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def save_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=True, indent=2) + '\n', encoding='utf-8')
    temporary.replace(path)


def safe_name(name):
    # Reject Windows aliases/ADS as well as POSIX traversal, even though we never extract.
    if not name or '\\' in name or name.startswith('/'):
        raise ValueError('unsafe tar path: ' + repr(name))
    name = name.removeprefix('./')
    name = name.rstrip('/')
    parts = name.split('/')
    reserved = {'CON', 'PRN', 'AUX', 'NUL'} | {f'{x}{i}' for x in ('COM', 'LPT') for i in range(1, 10)}
    for part in parts:
        if (part in ('', '.', '..') or part[-1:] in (' ', '.')
                or any(ord(c) < 32 or c in ':<>"|?*' for c in part)
                or part.split('.')[0].upper() in reserved):
            raise ValueError('unsafe tar path: ' + repr(name))
    return '/'.join(parts)


class BoundedReader(io.BufferedIOBase):
    """Count decompressed bytes, including metadata tarfile consumes internally."""
    def __init__(self, stream, limit):
        super().__init__()
        self.stream, self.remaining = stream, limit

    def read(self, size: int | None = -1) -> bytes:
        size = min(size if size is not None and size >= 0 else self.remaining + 1, self.remaining + 1)
        data = self.stream.read(size)
        self.remaining -= len(data)
        if self.remaining < 0:
            raise ValueError('tar expanded-byte limit exceeded')
        return data


def archive_members(path, *, max_expanded=MAX_EXPANDED, max_member=MAX_MEMBER, max_members=MAX_MEMBERS):
    rows, seen, total = [], set(), 0
    with bz2.open(path, 'rb') as compressed:
        bounded = BoundedReader(compressed, max_expanded)
        with tarfile.open(fileobj=bounded, mode='r|') as archive:
            for member in archive:
                if len(rows) >= max_members:
                    raise ValueError('tar member-count limit exceeded')
                name = safe_name(member.name)
                if name.casefold() in seen:
                    raise ValueError('duplicate tar path: ' + name)
                seen.add(name.casefold())
                if not (member.isfile() or member.isdir()) or member.issparse():
                    raise ValueError('tar links, devices and sparse members forbidden: ' + name)
                if member.size < 0 or member.size > max_member or (member.isdir() and member.size):
                    raise ValueError('tar member-size limit/type violation: ' + name)
                total += member.size
                if total > max_expanded:
                    raise ValueError('tar declared-size limit exceeded')
                row = {'path': name, 'bytes': member.size, 'type': 'file' if member.isfile() else 'directory'}
                if member.isfile():
                    h, count = hashlib.sha256(), 0
                    source = archive.extractfile(member)
                    if source is None:
                        raise ValueError('missing regular tar member stream: ' + name)
                    with source:
                        while True:
                            block = source.read(1024 * 1024)
                            if not block:
                                break
                            count += len(block)
                            h.update(block)
                    if count != member.size:
                        raise ValueError('truncated tar member: ' + name)
                    row['sha256'] = h.hexdigest()
                rows.append(row)
        # Account for trailing compressed data too; do not ignore a trailing decompression bomb.
        while bounded.read(1024 * 1024):
            pass
    return rows


def verify_file(path, size, expected):
    if path.stat().st_size != size or digest(path) != expected:
        raise ValueError('artifact size/SHA-256 mismatch: ' + path.name)


def download(cache, artifact, ledger_path):
    filename, remote, size, expected = artifact
    target = cache / filename
    if target.exists():
        verify_file(target, size, expected)
        return {'path': filename, 'status': 'verified-existing', 'bytes': size, 'sha256': expected}
    ledger = json.loads(ledger_path.read_text(encoding='utf-8')) if ledger_path.exists() else []
    # Reserve full size even on failure: repeated invocations cannot silently reset the budget.
    if sum(x['reserved_bytes'] for x in ledger) + size > NETWORK_BUDGET:
        raise ValueError('cumulative model download budget exceeded')
    partial = target.with_suffix(target.suffix + '.partial')
    if partial.exists():
        raise ValueError('existing partial preserved; inspect it before another attempt')
    row = {'url': BASE + remote, 'path': filename, 'reserved_bytes': size,
           'timeout_seconds': 180, 'retries': 0, 'status': 'started', 'started_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}
    ledger.append(row)
    save_json(ledger_path, ledger)
    start = time.monotonic()
    try:
        result = subprocess.run(
            ['curl', '--fail', '--location', '--silent', '--show-error', '--proto', '=https',
             '--proto-redir', '=https', '--max-time', '180', '--connect-timeout', '30',
             '--max-filesize', str(size), '--output', str(partial), BASE + remote],
            capture_output=True, timeout=185, check=False)
        row['returncode'] = result.returncode
        row['stderr'] = result.stderr.decode('utf-8', errors='replace')[-2000:]
        if result.returncode:
            raise RuntimeError('curl download failed: ' + str(result.returncode))
        verify_file(partial, size, expected)
        partial.rename(target)
        row.update(status='verified', sha256=expected)
    except Exception as error:
        row.update(status='failed', error=str(error))
        raise
    finally:
        row['elapsed_seconds'] = round(time.monotonic() - start, 3)
        row['stored_bytes'] = target.stat().st_size if target.exists() else partial.stat().st_size if partial.exists() else 0
        save_json(ledger_path, ledger)
    return row


def verify(root, cache):
    for filename, _, size, expected in ARTIFACTS:
        verify_file(cache / filename, size, expected)
    workflow = (root / '.github/workflows/build.yml').read_text(encoding='utf-8')
    for variable, artifact in zip(('svExpected', 'vadExpected'), ARTIFACTS):
        match = re.search(r'\$' + variable + r"\s*=\s*'([0-9a-f]{64})'", workflow)
        if not match or match.group(1) != artifact[3]:
            raise ValueError('workflow pin differs from audited artifact')
    members = archive_members(cache / 'sv.tar.bz2')
    by_name = {row['path']: row for row in members}
    comparisons = []
    for member, local in [('model.int8.onnx', 'sense-voice/model.int8.onnx'), ('tokens.txt', 'sense-voice/tokens.txt')]:
        row = by_name[PREFIX + '/' + member]
        path = root / 'crates/neo-stt/assets' / local
        comparisons.append({'member': row['path'], 'local': path.relative_to(root).as_posix(),
                            'local_sha256': digest(path), 'matches': digest(path) == row['sha256'] and path.stat().st_size == row['bytes']})
    vad = root / 'crates/neo-stt/assets/vad/silero_vad.onnx'
    comparisons.append({'member': 'silero_vad.onnx', 'local': vad.relative_to(root).as_posix(),
                        'local_sha256': digest(vad), 'matches': digest(vad) == ARTIFACTS[1][3]})
    return {'schema': 1, 'release_clearance': False, 'workflow_pins_verified': True,
            'artifacts': [{'path': x[0], 'source_url': BASE + x[1], 'bytes': x[2], 'sha256': x[3]} for x in ARTIFACTS],
            'tar_policy': {'extraction': False, 'max_expanded_bytes': MAX_EXPANDED, 'max_member_bytes': MAX_MEMBER, 'max_members': MAX_MEMBERS},
            'members': members, 'local_comparisons': comparisons}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--download', action='store_true')
    args = parser.parse_args()
    cache = ROOT / '.cache/stt'
    cache.mkdir(parents=True, exist_ok=True)
    evidence = ROOT / 'docs-pri/licenses/models-evidence'
    errors = []
    if args.download:
        for artifact in ARTIFACTS:
            try:
                download(cache, artifact, evidence / 'artifact-download-log.json')
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                errors.append({'artifact': artifact[0], 'error': str(error)})
    try:
        if errors:
            raise ValueError('one or more pinned downloads failed; no substitution allowed')
        report = verify(ROOT, cache)
        report['status'] = 'verified'
    except (OSError, ValueError, KeyError, EOFError, tarfile.TarError) as error:
        report = {'schema': 1, 'status': 'blocked', 'release_clearance': False,
                  'error': str(error), 'download_errors': errors,
                  'members_verified': False, 'local_ci_equivalence_claimed': False}
    save_json(evidence / 'ci-artifact-verification.json', report)
    print(json.dumps(report, indent=2))
    return 0 if report['status'] == 'verified' else 1


if __name__ == '__main__':
    raise SystemExit(main())
