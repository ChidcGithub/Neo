#!/usr/bin/env python3
"""Install pinned TexTeller assets into a copied base package (Python 3.11+).

  python -B tools/prepare_math_models.py --variant int8 --package dist/neo-int8 \
      --cache .cache/math-models

FP32 uses --variant fp32 and apps/blackboard/models/texteller; INT8 uses
texteller-int8. Only those model directories are installed, never drawing copies.
An optional --int8-archive reads an existing release ZIP without uploading it.
No sibling checkout, ignored development assets, quantization, model execution,
GUI, quality approval, or package-manifest edits. Run before final package file
inventories/signing. Existing complete installations are reverified; conflicting
or incomplete ones fail closed rather than being overwritten.

Cache entries are content-addressed and rehashed on every use. Downloads have
bounded sizes, HTTPS-only redirects, a socket timeout and an overall deadline;
failed partials are removed. Use trusted, quiescent package/cache directories:
links/reparse points are rejected, but concurrent hostile filesystem edits are
not supported. A complete staged tree is verified before a same-volume rename.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOCK = Path(__file__).with_name('math-models.lock.json')
REVISION = '7b96df06b9d81cdb129c3bef68b7250bc3e2b0ea'
HF = f'https://huggingface.co/OleehyO/TexTeller/resolve/{REVISION}/'
INT8_URL = ('https://github.com/ChidcGithub/Neo/releases/download/'
            'models-texteller-int8-v1/texteller-int8-v1.zip')
LICENSE_SOURCE = ('docs/licenses/supplemental/'
                  'cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30.txt')
MODEL_FILES = {'encoder_model.onnx', 'decoder_model.onnx', 'config.json',
               'generation_config.json', 'tokenizer.json'}
LEGAL_FILES = {'LICENSE-APACHE-2.0.txt', 'UPSTREAM-MODEL-CARD.md'}
INT8_FILES = MODEL_FILES | LEGAL_FILES | {
    'optimization.json', 'ATTRIBUTION.txt', 'FILES.sha256.json',
    'encoder_model.onnx.MODIFICATIONS.txt', 'decoder_model.onnx.MODIFICATIONS.txt'}
DIRECTORIES = {'int8': 'texteller-int8', 'fp32': 'texteller'}
CHUNK = 1024 * 1024
DOWNLOAD_SECONDS = 1800


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate JSON key: ' + key)
        result[key] = value
    return result


def read_json(path):
    return json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=unique_object)


def safe_name(name):
    if not isinstance(name, str) or not name or '\\' in name:
        raise ValueError('Unsafe path: ' + repr(name))
    for part in name.split('/'):
        base = part.split('.')[0].upper()
        if (part in ('', '.', '..') or part.endswith((' ', '.'))
                or any(ord(c) < 32 or c in ':<>"|?*' for c in part)
                or base in {'CON', 'PRN', 'AUX', 'NUL', 'CONIN$', 'CONOUT$'}
                or re.fullmatch(r'(COM|LPT)[0-9¹²³]', base)):
            raise ValueError('Unsafe path: ' + repr(name))
    return name


def checked(path):
    """Reject links, junctions and special files in every existing component."""
    path = Path(os.path.abspath(path))
    for item in (*reversed(path.parents), path):
        try:
            info = item.lstat()
        except FileNotFoundError:
            continue
        if (stat.S_ISLNK(info.st_mode)
                or getattr(info, 'st_file_attributes', 0) & 0x400
                or not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode))):
            raise ValueError('Link/reparse point or special file forbidden: ' + str(item))
    return path


def verify_file(path, pin):
    path = checked(path)
    if not path.is_file() or path.stat().st_size != pin['size']:
        raise ValueError('Size mismatch: ' + path.name)
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    if digest != pin['sha256']:
        raise ValueError('SHA-256 mismatch: ' + path.name)


def check_pin(pin):
    if (not isinstance(pin, dict) or type(pin.get('size')) is not int
            or not 0 < pin['size'] <= 2 * 1024**3
            or not isinstance(pin.get('sha256'), str)
            or not re.fullmatch('[0-9a-f]{64}', pin['sha256'])):
        raise ValueError('Invalid size/SHA-256 pin')


def load_lock(path=LOCK):
    lock = read_json(path)
    if lock.get('schema') != 1 or lock.get('revision') != REVISION:
        raise ValueError('Unsupported model lock/revision')
    for variant, names in (('int8', INT8_FILES), ('fp32', MODEL_FILES | LEGAL_FILES)):
        files = lock[variant]['files']
        if set(files) != names:
            raise ValueError('Incomplete or unexpected locked file set: ' + variant)
        for name, pin in files.items():
            safe_name(name)
            check_pin(pin)
            if variant == 'fp32':
                expected = HF + ('README.md' if name == 'UPSTREAM-MODEL-CARD.md' else name) + '?download=true'
                if name == 'LICENSE-APACHE-2.0.txt':
                    expected = 'https://www.apache.org/licenses/LICENSE-2.0.txt'
                    if pin.get('source') != LICENSE_SOURCE:
                        raise ValueError('Unexpected public license source')
                elif 'source' in pin:
                    raise ValueError('Unexpected local model source')
                if pin.get('url') != expected:
                    raise ValueError('Unexpected FP32 URL: ' + name)
    check_pin(lock['int8']['archive'])
    if lock['int8']['archive'].get('url') != INT8_URL:
        raise ValueError('Unexpected INT8 URL')
    return lock


def require_https(url):
    parsed = urllib.parse.urlsplit(url)
    if (parsed.scheme != 'https' or not parsed.hostname or parsed.username
            or parsed.password or parsed.fragment):
        raise ValueError('Only credential-free HTTPS URLs are allowed')


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        require_https(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def download(url, destination, size):
    require_https(url)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), HTTPSRedirect())
    request = urllib.request.Request(url, headers={
        'User-Agent': 'Neo-Math-Models/1', 'Accept-Encoding': 'identity'})
    deadline = time.monotonic() + DOWNLOAD_SECONDS
    try:
        with opener.open(request, timeout=60) as response:
            require_https(response.geturl())
            if response.status != 200:
                raise ValueError('Unexpected HTTP status')
            length = response.headers.get('Content-Length')
            if length is not None and int(length) != size:
                raise ValueError('HTTP size mismatch')
            if response.headers.get('Content-Encoding', 'identity') != 'identity':
                raise ValueError('Unexpected HTTP content encoding')
            with destination.open('wb') as output:
                count = 0
                while True:
                    if time.monotonic() > deadline:
                        raise TimeoutError('Model download deadline exceeded')
                    block = response.read(min(CHUNK, size - count + 1))
                    if not block:
                        break
                    count += len(block)
                    if count > size:
                        raise ValueError('Download exceeds pinned size')
                    output.write(block)
                if count != size:
                    raise ValueError('Truncated download')
    except (OSError, urllib.error.URLError) as error:
        # Do not leak signed redirect URLs, credentials or proxy configuration.
        raise RuntimeError('Model download failed (' + type(error).__name__ + ')') from None


def cached_file(cache, pin, local=None):
    cache = checked(cache)
    cache.mkdir(parents=True, exist_ok=True)
    target = checked(cache / (pin['sha256'] + '.blob'))
    if local is not None:
        local = checked(local)
        verify_file(local, pin)
    if target.exists():
        verify_file(target, pin)
        return target
    fd, name = tempfile.mkstemp(prefix='.math-', suffix='.partial', dir=cache)
    os.close(fd)
    partial = Path(name)
    try:
        if local is not None:
            shutil.copyfile(local, partial)
        else:
            download(pin['url'], partial, pin['size'])
        verify_file(partial, pin)
        partial.replace(target)
    finally:
        partial.unlink(missing_ok=True)
    return target


def extract_int8(archive_path, destination, files):
    """Accept either a single texteller-int8 wrapper or a flat tree, never mixed."""
    with zipfile.ZipFile(archive_path) as archive:
        members = archive.infolist()
        if len(members) > len(files) + 1:
            raise ValueError('ZIP member count exceeds locked file set')
        seen, payload, layouts = set(), {}, set()
        for member in members:
            raw = member.orig_filename
            name = safe_name(raw.removesuffix('/'))
            if name.casefold() in seen:
                raise ValueError('Duplicate/case-colliding ZIP path: ' + name)
            seen.add(name.casefold())
            mode = member.external_attr >> 16
            kind = stat.S_IFMT(mode)
            if (member.flag_bits & 1 or member.external_attr & 0x400
                    or kind not in (0, stat.S_IFREG, stat.S_IFDIR)):
                raise ValueError('Encrypted/link/special ZIP member: ' + name)
            if member.is_dir():
                if name != 'texteller-int8' or member.file_size or kind == stat.S_IFREG:
                    raise ValueError('Unexpected ZIP directory: ' + name)
                layouts.add('wrapped')
                continue
            if kind == stat.S_IFDIR:
                raise ValueError('ZIP file/directory type mismatch')
            wrapped = name.startswith('texteller-int8/')
            relative = name.removeprefix('texteller-int8/') if wrapped else name
            layouts.add('wrapped' if wrapped else 'flat')
            if relative not in files or member.file_size != files[relative]['size']:
                raise ValueError('Unexpected ZIP file/size: ' + name)
            payload[relative] = member
        if len(layouts) != 1 or set(payload) != set(files):
            raise ValueError('Incomplete or mixed-layout ZIP')
        # Inspect every header before writing anything; no extract/extractall.
        for name, member in payload.items():
            with archive.open(member) as source, (destination / name).open('xb') as output:
                remaining = files[name]['size']
                while block := source.read(min(CHUNK, remaining + 1)):
                    remaining -= len(block)
                    if remaining < 0:
                        raise ValueError('ZIP expanded-size limit exceeded')
                    output.write(block)
                if remaining:
                    raise ValueError('Truncated ZIP member: ' + name)


def verify_tree(directory, files, variant):
    checked(directory)
    actual = set()
    for child in directory.iterdir():
        checked(child)
        if not child.is_file():
            raise ValueError('Unexpected model subdirectory: ' + child.name)
        actual.add(child.name)
    if actual != set(files):
        raise ValueError('Installed model file set differs from lock')
    for name, pin in files.items():
        verify_file(directory / name, pin)
    if variant == 'int8':
        expected = {'texteller-int8/' + name: {'bytes': pin['size'], 'sha256': pin['sha256']}
                    for name, pin in files.items() if name != 'FILES.sha256.json'}
        if read_json(directory / 'FILES.sha256.json') != expected:
            raise ValueError('INT8 FILES.sha256.json differs from lock')


def prepare(variant, package, cache, *, int8_archive=None, lock_path=LOCK, root=ROOT):
    if variant not in DIRECTORIES:
        raise ValueError('Unknown model variant')
    if int8_archive is not None and variant != 'int8':
        raise ValueError('--int8-archive is only valid with --variant int8')
    lock = load_lock(lock_path)
    package, cache = checked(package), checked(cache)
    if not package.is_dir() or not checked(package / 'apps/blackboard').is_dir():
        raise ValueError('Package must contain apps/blackboard from the base package')
    if cache.is_relative_to(package) or package.is_relative_to(cache):
        raise ValueError('Package and cache must not overlap')
    models = checked(package / 'apps/blackboard/models')
    destination = checked(models / DIRECTORIES[variant])
    conflicts = [models / DIRECTORIES['fp32' if variant == 'int8' else 'int8']]
    conflicts += [package / 'apps/drawing/models' / name for name in DIRECTORIES.values()]
    for path in conflicts:
        if checked(path).exists():
            raise ValueError('Conflicting model tree; use a clean base package: ' + str(path))
    files = lock[variant]['files']
    if destination.exists():
        verify_tree(destination, files, variant)
        return destination
    with tempfile.TemporaryDirectory(prefix='.math-models-', dir=package) as temporary:
        stage = Path(temporary) / DIRECTORIES[variant]
        stage.mkdir()
        if variant == 'int8':
            archive = cached_file(cache, lock['int8']['archive'], int8_archive)
            extract_int8(archive, stage, files)
        else:
            for name, pin in files.items():
                local = checked(root / pin['source']) if 'source' in pin else None
                if local is not None and not local.exists():
                    local = None
                source = cached_file(cache, pin, local)
                shutil.copyfile(source, stage / name)
        verify_tree(stage, files, variant)
        models.mkdir(parents=True, exist_ok=True)
        if destination.exists():
            raise ValueError('Model destination appeared during preparation')
        stage.rename(destination)
    return destination


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--variant', required=True, choices=sorted(DIRECTORIES))
    parser.add_argument('--package', required=True, type=Path)
    parser.add_argument('--cache', required=True, type=Path)
    parser.add_argument('--int8-archive', type=Path,
                        help='Optional existing pinned release ZIP; read/verify only')
    args = parser.parse_args(argv)
    try:
        destination = prepare(args.variant, args.package, args.cache, int8_archive=args.int8_archive)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, zipfile.BadZipFile,
            NotImplementedError) as error:
        print('Math model preparation failed: ' + str(error), file=sys.stderr)
        return 1
    print(f'Verified {args.variant} models: {destination}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
