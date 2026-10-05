#!/usr/bin/env python3
"""Prepare pinned STT files offline using Python bz2/tar streams, never inference.

Defaults match build.yml: .cache/stt/{models,member-sha256.json}. Raw cache files
are read-only. Every regular archive member is hashed for workflow-compatible
metadata; only model.int8.onnx, tokens.txt and Silero VAD are written to models.
Integrity is not license/release approval. No downloads or external programs.

Use trusted, quiescent directories: links/reparse points and hardlinked files
are rejected, but concurrent hostile filesystem mutation is not supported.
Existing outputs/reports are never replaced. Staging and same-volume renames
prevent partial models on handled failure (two separate paths cannot be
published as one crash-atomic transaction). The monotonic deadline is checked
between bounded reads/writes; it cannot interrupt a blocked filesystem call.
"""
from __future__ import annotations

import argparse
import bz2
import hashlib
import json
import math
import stat
import tarfile
import tempfile
import time
from contextlib import nullcontext
from pathlib import Path

if __package__:
    from . import verify_model_artifacts as artifacts
    from . import verify_sensevoice_notices as notices
else:
    import verify_model_artifacts as artifacts
    import verify_sensevoice_notices as notices

CHUNK = 1024 * 1024
MAX_SECONDS = 180
MODEL_NAMES = ('model.int8.onnx', 'tokens.txt')


def progress(message):
    print(message, flush=True)


def checked(path):
    """Inspect ancestors without resolving away links, including dangling ones."""
    path = Path(path)
    if '..' in path.parts:
        raise ValueError('Parent traversal forbidden in filesystem path')
    path = path.absolute()
    for component in (*reversed(path.parents), path):
        try:
            info = component.lstat()
        except FileNotFoundError:
            continue
        if (stat.S_ISLNK(info.st_mode)
                or getattr(info, 'st_file_attributes', 0) & 0x400
                or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode))
                or (stat.S_ISREG(info.st_mode) and info.st_nlink != 1)):
            raise ValueError('Link/reparse/hardlink or special path forbidden: ' + str(component))
        if component != path and not stat.S_ISDIR(info.st_mode):
            raise ValueError('Non-directory ancestor: ' + str(component))
    return path


def require_absent(path):
    path = checked(path)
    if path.exists():
        raise ValueError('Existing destination will not be overwritten: ' + str(path))
    return path


class Deadline:
    def __init__(self, seconds):
        if not math.isfinite(seconds) or not 0 < seconds <= MAX_SECONDS:
            raise ValueError('Timeout must be greater than zero and at most 180 seconds')
        self.start = time.monotonic()
        self.end = self.start + seconds

    def check(self):
        if time.monotonic() >= self.end:
            raise TimeoutError('STT preparation time budget exceeded')

    def elapsed(self):
        return time.monotonic() - self.start


class TimedReader(artifacts.BoundedReader):
    def __init__(self, stream, limit, deadline):
        super().__init__(stream, limit)
        self.deadline = deadline

    def read(self, size=-1):
        self.deadline.check()
        # Also bound individual bz2 reads, including tarfile's internal reads.
        data = super().read(min(CHUNK, size) if size is not None and size >= 0 else CHUNK)
        self.deadline.check()
        return data


class PlainTarInfo(tarfile.TarInfo):
    @classmethod
    def fromtarfile(cls, tarfile):
        # Python 3.14's fromtarfile bypasses overridden frombuf methods.
        return cls.read_plain_header(tarfile)

    @classmethod
    def read_plain_header(cls, archive):
        buf = archive.fileobj.read(tarfile.BLOCKSIZE)
        member = cls.frombuf(buf, archive.encoding, archive.errors)
        # Reject extension headers before tarfile can hide them, rewrite names,
        # or consume their payload internally. The pinned archive needs none.
        if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
            raise ValueError('Tar links, devices, sparse and extension headers forbidden')
        member.offset = archive.fileobj.tell() - tarfile.BLOCKSIZE
        # Follow tarfile's own header processing only after the type gate.
        return member._proc_member(archive)  # pyright: ignore[reportAttributeAccessIssue]


def hash_stream(source, size, deadline, destination=None):
    digest, count = hashlib.sha256(), 0
    while True:
        deadline.check()
        block = source.read(min(CHUNK, size - count + 1))
        deadline.check()
        if not block:
            break
        count += len(block)
        if count > size:
            raise ValueError('Stream exceeds declared/pinned size')
        digest.update(block)
        if destination is not None:
            destination.write(block)
        deadline.check()
    if count != size:
        raise ValueError('Truncated stream / size mismatch')
    return digest.hexdigest()


def verify_raw(path, size, expected, deadline, destination=None):
    path = checked(path)
    if not path.is_file() or path.stat().st_size != size:
        raise ValueError('Artifact size mismatch: ' + path.name)
    with path.open('rb') as source:
        actual = hash_stream(source, size, deadline, destination)
    if actual != expected:
        raise ValueError('Artifact SHA-256 mismatch: ' + path.name)
    return actual


def scan_archive(path, stage, deadline):
    rows, seen, found, declared = [], set(), set(), 0
    pins = {artifacts.PREFIX + '/' + name:
            notices.MODEL_PINS['resources/models/stt/sense-voice/' + name]
            for name in MODEL_NAMES}
    progress('Archive start: sv.tar.bz2 (full scan)')
    with bz2.open(path, 'rb') as compressed:
        bounded = TimedReader(compressed, artifacts.MAX_EXPANDED, deadline)
        with tarfile.open(fileobj=bounded, mode='r|', tarinfo=PlainTarInfo) as archive:
            for member in archive:
                deadline.check()
                if len(seen) >= artifacts.MAX_MEMBERS:
                    raise ValueError('Tar member-count limit exceeded')
                name = artifacts.safe_name(member.name)
                if not (name == artifacts.PREFIX or name.startswith(artifacts.PREFIX + '/')):
                    raise ValueError('Unexpected tar prefix: ' + repr(name)[:200])
                if name.casefold() in seen:
                    raise ValueError('Duplicate tar path: ' + repr(name)[:200])
                seen.add(name.casefold())
                if not (member.isfile() or member.isdir()) or member.issparse():
                    raise ValueError('Tar links, devices and sparse members forbidden')
                if (member.size < 0 or member.size > artifacts.MAX_MEMBER
                        or (member.isdir() and member.size)
                        or (name == artifacts.PREFIX and not member.isdir())):
                    raise ValueError('Tar member-size limit/type violation')
                declared += member.size
                if declared > artifacts.MAX_EXPANDED:
                    raise ValueError('Tar declared-size limit exceeded')
                if name in pins and not member.isfile():
                    raise ValueError('Required model member is not a regular file')
                progress(f'Archive member {len(seen)}: {name!r:.200} size={member.size} '
                         + ('directory' if member.isdir() else 'file'))
                if member.isdir():
                    continue
                source = archive.extractfile(member)
                if source is None:
                    raise ValueError('Missing regular tar member stream')
                target = checked(stage / 'sense-voice' / name.rsplit('/', 1)[1]) if name in pins else None
                with source, (target.open('xb') if target is not None else nullcontext()) as output:
                    actual = hash_stream(source, member.size, deadline, output)
                if name in pins:
                    if actual != pins[name]:
                        raise ValueError('Model/tokens SHA-256 mismatch: ' + name)
                    found.add(name)
                rows.append({'archive': 'sv.tar.bz2', 'path': name,
                             'size': member.size, 'sha256': actual})
                progress(f'Archive member {len(seen)} end: bytes={member.size} sha256={actual}')
        # tarfile stops at its end marker and may have read ahead. All of those
        # bytes already passed through BoundedReader; drain the rest, including
        # concatenated bz2 streams, to check CRC/truncation and trailing bombs.
        while bounded.read(CHUNK):
            pass
        expanded = artifacts.MAX_EXPANDED - bounded.remaining
    if found != set(pins):
        raise ValueError('Missing required model/tokens members')
    progress(f'Archive end: members={len(seen)} files={len(rows)} '
             f'declared_bytes={declared} expanded_bytes={expanded}')
    return sorted(rows, key=lambda row: row['path'])


def prepare(cache=Path('.cache/stt'), output=None, report=None, *, timeout=MAX_SECONDS):
    deadline = Deadline(timeout)
    progress(f'STT preparation starting: timeout={timeout:g}s (offline, integrity only)')
    cache = checked(cache)
    output = require_absent(output if output is not None else cache / 'models')
    report = require_absent(report if report is not None else cache / 'member-sha256.json')
    if report.is_relative_to(output) or output.is_relative_to(report):
        raise ValueError('Model output and report paths must not overlap')
    raw_pins = {name: (size, sha) for name, _, size, sha in artifacts.ARTIFACTS}
    for name, (size, sha) in raw_pins.items():
        progress(f'Hash start: {name} expected_bytes={size}')
        verify_raw(cache / name, size, sha, deadline)
        progress(f'Hash end: {name} bytes={size} sha256={sha}')

    for parent in (output.parent, report.parent):
        checked(parent).mkdir(parents=True, exist_ok=True)
        checked(parent)
    # TemporaryDirectory keeps every staged file out of the final model path.
    with tempfile.TemporaryDirectory(prefix='.stt-models-', dir=output.parent) as temporary:
        stage = checked(Path(temporary) / 'models')
        (stage / 'sense-voice').mkdir(parents=True)
        (stage / 'vad').mkdir()
        rows = scan_archive(checked(cache / 'sv.tar.bz2'), stage, deadline)
        size, sha = raw_pins['silero_vad.onnx']
        progress(f'VAD copy start: bytes={size}')
        with checked(stage / 'vad/silero_vad.onnx').open('xb') as target:
            verify_raw(cache / 'silero_vad.onnx', size, sha, deadline, target)
        progress(f'VAD copy end: bytes={size} sha256={sha}')
        rows.append({'archive': None, 'path': 'silero_vad.onnx', 'size': size, 'sha256': sha})
        # Verify the actual staged files too, not only bytes read from sources.
        output_bytes = 0
        for row in rows:
            if row['archive'] is None:
                target = stage / 'vad/silero_vad.onnx'
            elif row['path'] in {artifacts.PREFIX + '/' + name for name in MODEL_NAMES}:
                target = stage / 'sense-voice' / row['path'].rsplit('/', 1)[1]
            else:
                continue
            verify_raw(target, row['size'], row['sha256'], deadline)
            output_bytes += row['size']
        evidence = {'sense_voice_archive_sha256': raw_pins['sv.tar.bz2'][1],
                    'silero_sha256': sha, 'members': rows}
        # The report gets its own same-volume staging directory when --report
        # and --output reside on different volumes. Publish models last.
        with tempfile.TemporaryDirectory(prefix='.stt-report-', dir=report.parent) as report_temp:
            staged_report = checked(Path(report_temp) / 'member-sha256.json')
            with staged_report.open('x', encoding='utf-8') as stream:
                json.dump(evidence, stream, ensure_ascii=True, indent=2)
                stream.write('\n')
            published_report = False
            try:
                require_absent(output)
                require_absent(report)
                checked(stage)
                deadline.check()
                staged_report.rename(report)
                published_report = True
                require_absent(output)
                deadline.check()
                stage.rename(output)
            except BaseException:
                if published_report:
                    report.unlink()
                raise
    progress(f'STT preparation end: files=3 output_bytes={output_bytes} '
             f'elapsed_seconds={deadline.elapsed():.3f}')
    return evidence


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache', type=Path, default=Path('.cache/stt'), help='Existing raw cache (read-only)')
    parser.add_argument('--output', type=Path, help='New model directory (default: CACHE/models)')
    parser.add_argument('--report', type=Path, help='New metadata file (default: CACHE/member-sha256.json)')
    parser.add_argument('--timeout', type=float, default=MAX_SECONDS, help='Total seconds, greater than 0 and <=180')
    args = parser.parse_args(argv)
    try:
        prepare(args.cache, args.output, args.report, timeout=args.timeout)
    except (OSError, ValueError, EOFError, tarfile.TarError) as error:
        progress('STT preparation failed: ' + str(error)[:500])
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
