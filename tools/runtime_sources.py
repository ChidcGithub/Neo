#!/usr/bin/env python3
"""Git Bash corresponding-source evidence, not a license approval gate.

inventory: snapshot every runtime file and every package-versions entry.
discover: query bounded official JSON metadata only; no archive downloads.
verify: check the snapshot, explicit package mappings and local pinned artifacts.
inspect: hash a local archive and its regular members without extracting/running it.

See docs-pri/licenses/gitbash-remediation.md for schema and review limitations.
Stdlib plus curl for opt-in downloads; never executes PKGBUILDs or approves releases.
Nested inspection copies regular archive bytes to generated temporary paths only.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile
from pathlib import Path
from urllib.parse import quote, urljoin, urlsplit

SCHEMA = 1
MAX_JSON = 2 * 1024 * 1024
MAX_MEMBERS = 100000
MAX_CONTENT = 4 * 1024 ** 3
CHUNK = 1024 * 1024
ROLES = {'source', 'license', 'patches', 'build-scripts'}
SHA256 = re.compile(r'[0-9a-f]{64}\Z')
NAME = re.compile(r'[a-zA-Z0-9@_+.~-]+\Z')
INDEX_ROOT = 'https://raw.githubusercontent.com/git-for-windows/pacman-repo/refs/heads/x86_64/'
API_ROOT = 'https://api.github.com/repos/git-for-windows/pacman-repo/releases/tags/'


def check_path(path: Path) -> None:
    # Include parents: a plain file beneath a junction can escape the evidence root.
    for p in (path.absolute(), *path.absolute().parents):
        try:
            info = p.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, 'st_file_attributes', 0) & 0x400:
            raise ValueError(f'reparse/symlink path is not allowed: {p}')


def safe_relative(value: str) -> str:
    if not isinstance(value, str) or not value or '\\' in value:
        raise ValueError(f'invalid relative path: {value!r}')
    reserved = {'CON', 'PRN', 'AUX', 'NUL', *(f'COM{i}' for i in '123456789¹²³'),
                *(f'LPT{i}' for i in '123456789¹²³')}
    for part in value.split('/'):
        if (not part or part in ('.', '..') or part.endswith((' ', '.'))
                or any(c in ':<>|?*"' or ord(c) < 32 for c in part)
                or part.split('.')[0].upper() in reserved):
            raise ValueError(f'unsafe relative path: {value!r}')
    return value


def hash_stream(stream, limit: int | None = None) -> tuple[str, int]:
    digest, size = hashlib.sha256(), 0
    while chunk := stream.read(CHUNK):
        size += len(chunk)
        if limit is not None and size > limit:
            raise ValueError('content exceeds inspection budget')
        digest.update(chunk)
    return digest.hexdigest(), size


def file_record(path: Path) -> dict:
    check_path(path)
    if not path.is_file():
        raise ValueError(f'not a regular file: {path}')
    with path.open('rb') as stream:
        sha, size = hash_stream(stream)
    return {'sha256': sha, 'size': size}


def pe_signing_content_record(raw: bytes) -> dict:
    """Hash every PE byte except narrowly bounded Authenticode signing fields.

    This is not signature verification or a general executable equivalence test.
    A certificate must be aligned, after all sections, and end exactly at EOF;
    arbitrary overlays, resources, timestamps and code remain in the digest.
    No assembly is loaded and no executable is changed on disk.
    """
    def u16(offset):
        if offset < 0 or offset + 2 > len(raw):
            raise ValueError('truncated PE')
        return struct.unpack_from('<H', raw, offset)[0]

    def u32(offset):
        if offset < 0 or offset + 4 > len(raw):
            raise ValueError('truncated PE')
        return struct.unpack_from('<I', raw, offset)[0]

    if len(raw) < 64 or raw[:2] != b'MZ':
        raise ValueError('not a PE file')
    pe = u32(0x3c)
    if raw[pe:pe + 4] != b'PE\0\0':
        raise ValueError('invalid PE signature')
    optional = pe + 24
    optional_size = u16(pe + 20)
    magic = u16(optional)
    directory = optional + (96 if magic == 0x10b else 112 if magic == 0x20b else 0)
    if directory == optional or directory + 40 > optional + optional_size:
        raise ValueError('unsupported PE optional header')
    if u32(directory - 4) < 5:
        raise ValueError('missing PE security directory')
    checksum = optional + 64
    security = directory + 32
    cert_offset, cert_size = u32(security), u32(security + 4)
    section_table = optional + optional_size
    sections = u16(pe + 6)
    if not 0 < sections <= 96 or section_table + sections * 40 > len(raw):
        raise ValueError('invalid PE section table')
    image_end = max(u32(optional + 60), section_table + sections * 40)
    for i in range(sections):
        section = section_table + i * 40
        size, offset = u32(section + 16), u32(section + 20)
        if offset + size > len(raw):
            raise ValueError('PE section outside file')
        image_end = max(image_end, offset + size)
    if bool(cert_offset) != bool(cert_size):
        raise ValueError('inconsistent PE certificate bounds')
    if cert_size:
        if cert_offset % 8 or cert_offset < image_end or cert_offset + cert_size != len(raw):
            raise ValueError('PE certificate must follow sections and end at EOF')
        cursor = cert_offset
        while cursor < len(raw):
            length = u32(cursor)
            if length < 8 or cursor + length > len(raw) or u16(cursor + 4) != 0x200 or u16(cursor + 6) != 2:
                raise ValueError('invalid WIN_CERTIFICATE')
            aligned = (cursor + length + 7) & ~7
            if aligned > len(raw) or any(raw[cursor + length:aligned]):
                raise ValueError('invalid PE certificate padding')
            cursor = aligned
        end = cert_offset
    else:
        end = len(raw)
    content = bytearray(raw[:end])
    content[checksum:checksum + 4] = b'\0' * 4
    content[security:security + 8] = b'\0' * 8
    return {'sha256': hashlib.sha256(content).hexdigest(), 'size': len(content),
            'certificate_size': cert_size, 'signature_verified': False,
            'method': 'All bytes except checksum, security-directory entry and bounded trailing WIN_CERTIFICATE'}


def verify_notice_delivery(manifest: dict, runtime: Path, notices: Path) -> dict:
    """Check a version-scoped notice review against every runtime byte and notice.

    The manifest is a review input, not trusted legal authority. Integrity success
    does not remove its explicit licensing blockers or grant release approval.
    """
    errors = []
    state, _ = snapshot(runtime)
    digest = hashlib.sha256(json.dumps(state, sort_keys=True,
                                      separators=(',', ':')).encode()).hexdigest()
    if digest != manifest.get('runtime_snapshot_sha256'):
        errors.append('runtime snapshot changed: repeat source and notice review')
    actual = {f['path']: f for f in state['files']}
    declared_notices = {}
    for item in manifest.get('notice_files', []):
        try:
            name = safe_relative(item['path'])
            require_sha(item['sha256'])
            if name.casefold() in declared_notices:
                raise ValueError('duplicate notice path')
            declared_notices[name.casefold()] = name
            if file_record(notices / name) != {k: item[k] for k in ('sha256', 'size')}:
                raise ValueError('notice bytes changed')
        except (ValueError, OSError, KeyError) as error:
            errors.append(f'notice: {error}')
    seen = set()
    for item in manifest.get('files', []):
        try:
            name = safe_relative(item['path'])
            if name.casefold() in seen:
                raise ValueError('duplicate runtime mapping')
            seen.add(name.casefold())
            if actual.get(name) != {k: item[k] for k in ('path', 'sha256', 'size')}:
                raise ValueError('mapped runtime bytes changed')
            refs = item['notice_files']
            if not refs or any(ref.casefold() not in declared_notices for ref in refs):
                raise ValueError('missing referenced notice')
            if not item.get('packages'):
                raise ValueError('missing exact package evidence')
        except (ValueError, KeyError, TypeError) as error:
            errors.append(f'runtime mapping: {error}')
    if not seen or not declared_notices or manifest.get('unmapped_executable_files') != []:
        errors.append('empty or incomplete notice coverage')
    blockers = manifest.get('blockers')
    if not isinstance(blockers, list):
        errors.append('explicit blocker list required')
    return {'integrity_ok': not errors, 'errors': errors, 'approval': False,
            'published': False, 'release_ready': False, 'blockers': blockers,
            'limitations': ['Hashes validate the declared review, not licensing permission.',
                            'Any runtime mutation requires a new snapshot/source/notice review.']}


def parse_versions(raw: bytes) -> dict[str, str]:
    packages = {}
    for number, line in enumerate(raw.decode('utf-8-sig').splitlines(), 1):
        if not line.strip():
            continue
        fields = line.split()
        if len(fields) != 2 or not NAME.fullmatch(fields[0]):
            raise ValueError(f'invalid package-versions line {number}')
        name, version = fields
        if not re.fullmatch(r'[a-zA-Z0-9@_+.~:\-]+', version):
            raise ValueError(f'invalid version on line {number}')
        if name in packages and packages[name] != version:
            raise ValueError(f'conflicting duplicate package: {name}')
        packages[name] = version  # Identical duplicates occur in actual MinGit.
    if not packages:
        raise ValueError('empty package-versions')
    return dict(sorted(packages.items()))


def snapshot(runtime: Path) -> tuple[dict, dict[str, str]]:
    check_path(runtime)
    versions_path = runtime / 'etc/package-versions.txt'
    check_path(versions_path)
    raw = versions_path.read_bytes()
    packages = parse_versions(raw)
    files, seen = [], set()
    def walk_error(error):
        raise error
    for directory, dirs, names in os.walk(runtime, onerror=walk_error, followlinks=False):
        for name in dirs + names:
            check_path(Path(directory) / name)
        for name in sorted(names):
            path = Path(directory) / name
            rel = safe_relative(path.relative_to(runtime).as_posix())
            if rel.casefold() in seen:
                raise ValueError(f'case-insensitive runtime collision: {rel}')
            seen.add(rel.casefold())
            files.append({'path': rel, **file_record(path)})
    return {'package_versions_sha256': hashlib.sha256(raw).hexdigest(),
            'files': sorted(files, key=lambda f: f['path']),
            'scope': 'All files; package list is conservative, not a file-to-package SBOM'}, packages


def inventory(runtime: Path) -> dict:
    state, packages = snapshot(runtime)
    return {'schema_version': SCHEMA, 'approval': False, 'runtime': state,
            'artifacts': [], 'packages': [
                {'name': n, 'version': v, 'artifact_ids': [], 'mapping_evidence': '',
                 'patches_not_required_reason': '', 'candidates': []}
                for n, v in packages.items()]}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f'duplicate JSON key: {key}')
        result[key] = value
    return result


def load_json(path: Path) -> dict:
    check_path(path)
    with path.open(encoding='utf-8') as stream:
        result = json.load(stream, object_pairs_hook=unique_object)
    if not isinstance(result, dict):
        raise TypeError('manifest must be an object')
    return result


def metadata_json(url: str) -> dict:
    class MetadataOnly(urllib.request.HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            raise ValueError('metadata redirects are not allowed')
    parsed = urlsplit(url)
    if not url.startswith((INDEX_ROOT, API_ROOT)) or parsed.query or parsed.fragment:
        raise ValueError('only official package metadata endpoints are allowed')
    request = urllib.request.Request(url, headers={'User-Agent': 'Neo-runtime-sources',
                                                  'Accept': 'application/json'})
    with urllib.request.build_opener(MetadataOnly).open(request, timeout=20) as response:
        raw = response.read(MAX_JSON + 1)
    if len(raw) > MAX_JSON:
        raise ValueError('metadata exceeds 2 MiB')
    data = json.loads(raw, object_pairs_hook=unique_object)
    if not isinstance(data, dict):
        raise TypeError('metadata is not an object')
    return {'url': url, 'response_sha256': hashlib.sha256(raw).hexdigest(), 'data': data}


def discover(manifest: dict, fetch=metadata_json) -> dict:
    """Candidates only: split-package/epoch mapping requires human evidence."""
    if manifest.get('schema_version') != SCHEMA:
        raise ValueError('unsupported manifest schema')
    releases = {}
    for package in manifest['packages']:
        name, version = package['name'], package['version']
        if not NAME.fullmatch(name):
            raise ValueError('invalid package name')
        package['candidates'] = []
        package['discovery'] = {'status': 'unresolved'}
        try:
            index = fetch(INDEX_ROOT + quote(name, safe='') + '.versions.json')
            package['discovery']['index'] = index
            tag = index['data'].get(version)
            if not isinstance(tag, str) or not tag:
                raise ValueError('exact version absent from official index; no guessed fallback')
            url = API_ROOT + quote(tag, safe='')
            if url not in releases:
                releases[url] = fetch(url)
            release = releases[url]
            data = release['data']
            if data.get('tag_name') != tag or data.get('draft') is not False:
                raise ValueError('release tag mismatch or draft')
            package['discovery']['release_url'] = url
            package['discovery']['release_response_sha256'] = release['response_sha256']
            # A release may contain several source packages. Do not assign any
            # automatically, even if its filename looks like this binary package.
            for asset in data['assets']:
                if '.src.tar.' in asset['name'] and not asset['name'].endswith('.sig'):
                    package['candidates'].append({
                        'name': asset['name'], 'url': asset['browser_download_url'],
                        'sha256': (asset.get('digest') or '').removeprefix('sha256:'),
                        'size': asset['size'], 'asset_id': asset['id'],
                        'evidence_url': url, 'verification': 'API metadata only; archive not inspected'})
            if not package['candidates']:
                raise ValueError('release contains no source archive asset')
            package['discovery']['status'] = 'candidates-only'
        except (OSError, ValueError, KeyError, TypeError) as error:
            package['discovery']['error'] = str(error)
    manifest['approval'] = False
    return manifest


def inspect_archive(path: Path, max_content: int = MAX_CONTENT, *,
                    record_zip_links: bool = False, shared_budget: dict | None = None) -> dict:
    """Hash raw archive + every regular member; no extraction or nested unpacking.

    Symlinks/hardlinks are recorded, never followed. Declared evidence members
    must be regular files. Nested tarballs remain opaque regular members.
    """
    record = file_record(path)
    members, seen, total = [], set(), 0
    def add(name, kind, size=0, stream=None, target=None):
        nonlocal total
        name = safe_relative(name.rstrip('/') if kind == 'directory' else name)
        if name.casefold() in seen:
            raise ValueError(f'duplicate/case-alias archive member: {name}')
        seen.add(name.casefold())
        if shared_budget is not None:
            shared_budget['members'] -= 1
            shared_budget['bytes'] -= size
            if shared_budget['members'] < 0 or shared_budget['bytes'] < 0:
                raise ValueError('shared nested inspection budget exceeded')
        if len(seen) > MAX_MEMBERS or size < 0 or total + size > max_content:
            raise ValueError('archive exceeds inspection budget')
        item = {'path': name, 'kind': kind}
        if stream is not None:
            sha, actual = hash_stream(stream, max_content - total)
            if actual != size:
                raise ValueError(f'archive member size mismatch: {name}')
            total += actual
            item.update(sha256=sha, size=actual)
        if target is not None:
            item['target'] = target
        members.append(item)
    if zipfile.is_zipfile(path):
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                mode = stat.S_IFMT(info.external_attr >> 16)
                if (info.orig_filename != info.filename or info.flag_bits & 1
                        or mode not in (0, stat.S_IFDIR, stat.S_IFREG, *([stat.S_IFLNK] if record_zip_links else []))):
                    raise ValueError(f'unsupported ZIP member: {info.filename}')
                if mode == stat.S_IFLNK:
                    if info.file_size > 65536:
                        raise ValueError('ZIP link target too long')
                    with archive.open(info) as stream:
                        target = stream.read(65537).decode('utf-8', errors='replace')
                    add(info.filename, 'symlink', info.file_size, target=target)
                elif info.is_dir():
                    add(info.filename, 'directory')
                else:
                    with archive.open(info) as stream:
                        add(info.filename, 'file', info.file_size, stream)
    else:
        with tarfile.open(path, 'r|*') as archive:
            for info in archive:
                if info.isdir():
                    add(info.name, 'directory')
                elif info.isfile():
                    stream = archive.extractfile(info)
                    if stream is None:
                        raise ValueError(f'unreadable TAR member: {info.name}')
                    with stream:
                        add(info.name, 'file', info.size, stream)
                elif info.issym() or info.islnk():
                    add(info.name, 'symlink' if info.issym() else 'hardlink', target=info.linkname)
                else:
                    raise ValueError(f'unsupported TAR member: {info.name}')
    return {**record, 'members': members, 'nested_archives_inspected': False}


ARCHIVE_SUFFIXES = ('.tar.gz', '.tar.bz2', '.tar.xz', '.tar.zst', '.tgz', '.tar', '.zip')
# Retain opaque containers as explicit gaps, never mistake them for indexed source.
OPAQUE_CONTAINER_SUFFIXES = ('.tar.lz', '.tar.lzo', '.7z', '.rar', '.pack')


def mingw_source_mapping(fields: dict, pkgbuild: str, package: str, version: str) -> dict:
    """Recognize a shared MINGW64/UCRT recipe, not a binary architecture alias.

    Only literal package-name templates are accepted; no shell evaluation. The
    caller must separately authenticate the archive and review its inputs/patches.
    A matching name alone (including a matching pkgbase) is never sufficient.
    """
    result = {'matched': False, 'kind': 'shared-source-recipe',
              'binary_equivalence_claimed': False, 'shell_executed': False}
    source_version = '-'.join(fields.get(k, [''])[0] for k in ('pkgver', 'pkgrel'))
    epoch = fields.get('epoch', ['0'])[0]
    if epoch != '0':
        source_version = epoch + '~' + source_version
    if version != source_version or not package.startswith('mingw-w64-x86_64-'):
        return result
    arch = re.search(r'^mingw_arch=\(([^\n)]*)\)', pkgbuild, re.M)
    if not arch or not {'mingw64', 'ucrt64'} <= set(re.findall(r'[a-z0-9]+', arch[1])):
        return result
    real = re.search(r'^_realname=[\"\']?([a-zA-Z0-9_.+-]+)[\"\']?\s*$', pkgbuild, re.M)
    # Inspect only literal tokens in the declaration. Conditional/command
    # substitutions are not evidence that a particular split package is built.
    names = re.search(r'^pkgname=(\([^\n]*(?:\n[ \t]+[^\n]*)*|[^\n]*)', pkgbuild, re.M)
    if not names:
        return result
    templates = []
    for line in names[1].splitlines():
        literal = line.split('$(', 1)[0].split('#', 1)[0].lstrip('(')
        templates.extend(re.findall(
            r'(?:^|[ \t])[\"\']?(\$\{MINGW_PACKAGE_PREFIX\}-[a-zA-Z0-9_${}.+-]+)[\"\']?(?=[ \t)]|$)',
            literal))
    suffix = package.removeprefix('mingw-w64-x86_64-')
    for template in templates:
        value = template.replace('${MINGW_PACKAGE_PREFIX}', 'mingw-w64-x86_64')
        if real:
            value = value.replace('${_realname}', real[1]).replace('$_realname', real[1])
        counterpart = 'mingw-w64-ucrt-x86_64-' + suffix
        if value == package and counterpart in fields.get('pkgname', []):
            result.update(matched=True, package=package, version=version,
                          srcinfo_counterpart=counterpart, pkgbase=fields.get('pkgbase', []),
                          recipe_template=template, mingw_arch_declaration=arch[0],
                          build_environment={'MSYSTEM': 'MINGW64', 'MINGW_PACKAGE_PREFIX':
                                             'mingw-w64-x86_64', 'MINGW_PREFIX': '/mingw64'},
                          condition='Same fixed source revision, downstream patches and recipe; '
                                    'build with MINGW64, not UCRT64. No bit-reproducibility claim.')
            break
    return result


def inspect_source_tree(path: Path, max_content: int = MAX_CONTENT, max_depth: int = 3) -> dict:
    """Read nested bytes into generated temp files, never extract member paths.

    Inventories and literal .SRCINFO checks are evidence, not shell evaluation or
    completeness approval. A single decompressed-byte/member budget spans recursion.
    """
    budget = {'bytes': max_content, 'members': MAX_MEMBERS}
    def visit(local, depth):
        result = inspect_archive(local, max(0, budget['bytes']), record_zip_links=True,
                                 shared_budget=budget)
        result.update(nested=[], metadata={}, issues=[], uninspected_nested=[],
                      corresponding_source_complete=False)
        def skipped(member, reason, purpose='unclassified'):
            result['issues'].append(reason)
            result['uninspected_nested'].append({
                'member_path': member['path'], 'sha256': member['sha256'],
                'size': member['size'], 'reason': reason,
                'bytes_retained_in_parent': True, 'contents_indexed': False,
                                'purpose': purpose, 'delivery_blocker': purpose != 'test-fixture'})
        for member in result['members']:
            if member['kind'] == 'file' and member['path'].endswith(OPAQUE_CONTAINER_SUFFIXES):
                skipped(member, f'opaque container requires separate inspection: {member["path"]}')
        wanted = {m['path']: m for m in result['members'] if m['kind'] == 'file' and
                  (m['path'].endswith(ARCHIVE_SUFFIXES) or m['path'].rsplit('/', 1)[-1] in ('.SRCINFO', 'PKGBUILD'))}
        def consume(name, stream):
            if name not in wanted:
                return
            member = wanted[name]
            if name.rsplit('/', 1)[-1] in ('.SRCINFO', 'PKGBUILD'):
                if member['size'] > MAX_JSON:
                    result['issues'].append(f'metadata too large: {name}')
                else:
                    result['metadata'][name] = stream.read(MAX_JSON + 1).decode('utf-8', errors='replace')
                return
            if depth >= max_depth:
                skipped(member, f'nested depth budget: {name}')
                return
            with tempfile.TemporaryDirectory() as temporary:
                nested = Path(temporary) / 'archive'
                with nested.open('xb') as output:
                    digest, size = hashlib.sha256(), 0
                    while chunk := stream.read(CHUNK):
                        size += len(chunk)
                        if size > member['size']:
                            raise ValueError('nested size changed')
                        digest.update(chunk)
                        output.write(chunk)
                if size != member['size'] or digest.hexdigest() != member['sha256']:
                    raise ValueError('nested bytes differ from inventory')
                try:
                    if re.search(r'(^|/)(t|test|tests|testdata)/', name) and not any(
                            m['path'].rsplit('/', 1)[-1] == '.SRCINFO' for m in result['members']):
                        skipped(member, f'test-fixture nested archive not expanded: {name}',
                                purpose='test-fixture')
                        return
                    child = visit(nested, depth + 1)
                    result['nested'].append({'member_path': name, 'inspection': child})
                except (ValueError, OSError, tarfile.TarError, zipfile.BadZipFile) as error:
                    skipped(member, f'nested inspection failed: {name}: {error}')
        if zipfile.is_zipfile(local):
            with zipfile.ZipFile(local) as archive:
                for info in archive.infolist():
                    if info.filename in wanted:
                        with archive.open(info) as stream:
                            consume(info.filename, stream)
        else:
            with tarfile.open(local, 'r|*') as archive:
                for info in archive:
                    if info.isfile() and info.name in wanted:
                        stream = archive.extractfile(info)
                        if stream is None:
                            raise ValueError(f'unreadable nested member: {info.name}')
                        with stream:
                            consume(info.name, stream)
        result['nested_archives_inspected'] = not result['issues'] and all(
            n['inspection']['nested_archives_inspected'] for n in result['nested'])
        result['srcinfo_checks'] = []
        by_name = {m['path']: m for m in result['members'] if m['kind'] == 'file'}
        for name, text in result['metadata'].items():
            if not name.endswith('/.SRCINFO') and name != '.SRCINFO':
                continue
            fields = {}
            for line in text.splitlines():
                match = re.fullmatch(r'\s*([\w]+)\s*=\s*(.*?)\s*', line)
                if match:
                    fields.setdefault(match[1], []).append(match[2])
            checks = []
            for key, inputs in fields.items():
                if key != 'source' and not key.startswith('source_'):
                    continue
                hashes = fields.get('sha256sums' + key.removeprefix('source'), [])
                for i, source in enumerate(inputs):
                    filename = source.split('::', 1)[0] if '::' in source else source.rsplit('/', 1)[-1]
                    prefix = name.rsplit('/', 1)[0] + '/' if '/' in name else ''
                    member = by_name.get(prefix + filename)
                    pin = hashes[i] if i < len(hashes) else None
                    checks.append({'input': source, 'member': prefix + filename,
                                   'present': member is not None, 'declared_sha256': pin,
                                   'sha256_matches': bool(member and pin and SHA256.fullmatch(pin) and member['sha256'] == pin)})
            result['srcinfo_checks'].append({'path': name, 'fields': fields, 'inputs': checks,
                                            'dynamic_pkgbuild_evaluated': False})
        return result
    result = visit(path, 0)
    def gaps(tree, chain):
        for item in tree['uninspected_nested']:
            yield {'archive_chain': chain, **item}
        for child in tree['nested']:
            yield from gaps(child['inspection'], chain + [child['member_path']])
    result['nested_archive_gaps'] = list(gaps(result, []))
    result['source_delivery_review_gaps'] = [g for g in result['nested_archive_gaps']
                                           if g['delivery_blocker']]
    result['inspection_bytes'] = max_content - budget['bytes']
    result['inspection_members'] = MAX_MEMBERS - budget['members']
    result['approval'] = False
    return result


def require_sha(value) -> None:
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise ValueError('a pinned lowercase SHA-256 is required')


def require_url(value) -> None:
    if not isinstance(value, str) or any(ord(c) <= 32 for c in value):
        raise ValueError('source/evidence URL must be HTTPS')
    parsed = urlsplit(value)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError('source/evidence URL must be HTTPS without credentials')


def verify(manifest: dict, runtime: Path, archives: Path) -> dict:
    errors = []
    def fail(message):
        errors.append(message)
    if manifest.get('schema_version') != SCHEMA:
        raise ValueError('unsupported manifest schema')
    state, actual_packages = snapshot(runtime)
    if manifest.get('runtime') != state:
        fail('runtime snapshot/package-versions hash differs (added/removed/changed files)')
    packages = manifest.get('packages', [])
    mapped = {}
    for p in packages:
        if p['name'] in mapped:
            fail(f'duplicate package mapping: {p["name"]}')
        mapped[p['name']] = p['version']
    if mapped != actual_packages:
        fail('package mappings do not exactly cover package-versions names and versions')
    artifact_roles, ids, paths = {}, set(), set()
    for a in manifest.get('artifacts', []):
        label = a.get('id', '<missing>')
        try:
            if not isinstance(label, str) or not NAME.fullmatch(label) or label in ids:
                raise ValueError('missing/duplicate/invalid artifact ID')
            ids.add(label)
            require_url(a['url'])
            require_url(a['hash_evidence_url'])
            require_sha(a['sha256'])
            rel = safe_relative(a['path'])
            if rel.casefold() in paths:
                raise ValueError('duplicate artifact path')
            paths.add(rel.casefold())
            if type(a['size']) is not int or a['size'] <= 0:
                raise ValueError('positive archive size required')
            path = archives / rel
            raw_record = file_record(path)
            if raw_record != {'sha256': a['sha256'], 'size': a['size']}:
                raise ValueError('whole-archive SHA-256/size mismatch')
            inspected = inspect_archive(path)
            members = {m['path']: m for m in inspected['members']}
            roles = set()
            if not a.get('members'):
                raise ValueError('explicit evidence members required')
            for member in a['members']:
                require_sha(member['sha256'])
                m = members.get(safe_relative(member['path']))
                if not m or m['kind'] != 'file' or m['sha256'] != member['sha256']:
                    raise ValueError('evidence member absent, not a regular file, or hash mismatch')
                if member['role'] not in ROLES:
                    raise ValueError('unknown evidence member role')
                roles.add(member['role'])
            artifact_roles[label] = roles
        except (OSError, ValueError, KeyError, TypeError, tarfile.TarError, zipfile.BadZipFile) as error:
            fail(f'artifact {label}: {error}')
    used = set()
    for p in packages:
        label = f'{p["name"]} {p["version"]}'
        try:
            require_url(p['mapping_evidence'])
            refs = p['artifact_ids']
            if not isinstance(refs, list) or not refs or len(refs) != len(set(refs)):
                raise ValueError('nonempty unique artifact_ids required')
            used.update(refs)
            if any(ref not in artifact_roles for ref in refs):
                raise ValueError('missing/invalid referenced source artifact')
            roles = set().union(*(artifact_roles[ref] for ref in refs))
            needed = ROLES - ({'patches'} if p.get('patches_not_required_reason', '').strip() else set())
            if not needed <= roles:
                raise ValueError(f'missing evidence roles: {sorted(needed - roles)}')
        except (ValueError, KeyError, TypeError, AttributeError) as error:
            fail(f'package {label}: {error}')
    if ids - used:
        fail(f'unreferenced artifacts: {sorted(ids - used)}')
    return {'schema_version': SCHEMA, 'integrity_ok': not errors, 'approval': False,
            'corresponding_source_complete': False, 'errors': errors,
            'limitations': [
                'Integrity/declared coverage only; mapping URLs and member roles require human review.',
                'No proof of binary provenance, file-to-package ownership, build reproducibility, or nested-source completeness.',
                'Publication, license obligations, availability and installation information require separate review.']}


OFFICIAL_HOSTS = {'github.com', 'api.github.com', 'raw.githubusercontent.com',
                  'release-assets.githubusercontent.com', 'objects.githubusercontent.com'}


def official_url(url: str) -> None:
    require_url(url)
    parsed = urlsplit(url)
    if parsed.hostname not in OFFICIAL_HOSTS or parsed.port not in (None, 443):
        raise ValueError('not an allowed official GitHub HTTPS host')


def bounded_get(url: str, destination: Path, limit: int, seconds: float = 120,
                byte_range: tuple[int, int, int] | None = None) -> dict:
    """No credentials/config/retries. Validate every redirect, hard wall-clock bound.

    curl is only a transport: no downloaded program is ever executed. The caller
    reserves `limit` bytes before starting; redirects share the same byte budget.
    """
    check_path(destination)
    if destination.exists() or limit <= 0 or not 0 < seconds <= 120:
        raise ValueError('new destination and positive bounded download budget required')
    if byte_range is not None and not (0 <= byte_range[0] <= byte_range[1] < byte_range[2]):
        raise ValueError('invalid byte range')
    deadline, consumed = time.monotonic() + seconds, 0
    original = url
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        body, headers = Path(temporary) / 'body', Path(temporary) / 'headers'
        for _ in range(6):
            official_url(url)
            remaining = deadline - time.monotonic()
            if remaining <= 0 or consumed >= limit:
                raise ValueError('download time/byte budget exhausted')
            command = ['curl', '-q', '--silent', '--show-error', '--proto', '=https',
                       '--connect-timeout', str(min(45, remaining)), '--max-time', str(remaining),
                       '--max-filesize', str(limit - consumed), '--dump-header', str(headers),
                       '--output', str(body), '--write-out', '%{http_code}',
                       '--user-agent', 'Neo-runtime-sources', url]
            if byte_range is not None:
                command[-1:-1] = ['--range', f'{byte_range[0]}-{byte_range[1]}']
            result = subprocess.run(command, capture_output=True, timeout=remaining + 0.2, check=False)
            consumed += body.stat().st_size if body.exists() else 0
            if result.returncode:
                raise OSError('curl: ' + result.stderr.decode('utf-8', errors='replace')[:500])
            status = int(result.stdout)
            if status in (301, 302, 303, 307, 308):
                locations = re.findall(r'^location:\s*(.+)$', headers.read_text(encoding='iso-8859-1'), re.MULTILINE | re.IGNORECASE)
                if not locations:
                    raise ValueError('redirect without Location')

                url = urljoin(url, locations[-1].strip())
                continue
            if status != (206 if byte_range else 200):
                raise OSError(f'HTTP {status}: {original}')
            if byte_range:
                expected = f'bytes {byte_range[0]}-{byte_range[1]}/{byte_range[2]}'
                ranges = re.findall(r'^content-range:\s*(.+)$', headers.read_text(encoding='iso-8859-1'), re.MULTILINE | re.IGNORECASE)
                if not ranges or ranges[-1].strip() != expected or body.stat().st_size != byte_range[1] - byte_range[0] + 1:
                    raise ValueError('incorrect range response')
            if consumed > limit:
                raise ValueError('download byte budget exceeded')
            # Exclusive destination; never replace user evidence.
            with destination.open('xb') as output, body.open('rb') as source:
                while chunk := source.read(CHUNK):
                    output.write(chunk)
            return {'url': original, 'bytes_received': consumed, **file_record(destination)}
    raise ValueError('too many redirects')


def ranged_get(asset: dict, destination: Path, seconds: float, parts: int = 8) -> dict:
    """One bounded parallel range attempt, whole-archive pin checked by acquire."""
    if type(parts) is not int or not 1 <= parts <= 8 or asset['size'] < parts:
        raise ValueError('invalid range count')
    check_path(destination)
    size = asset['size']
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        paths = [Path(temporary) / str(i) for i in range(parts)]
        def part(i):
            start, end = size * i // parts, size * (i + 1) // parts - 1
            return bounded_get(asset['url'], paths[i], end - start + 1 + 65536,
                               seconds, (start, end, size))
        with concurrent.futures.ThreadPoolExecutor(max_workers=parts) as pool:
            records = list(pool.map(part, range(parts)))
        with destination.open('xb') as output:
            for path in paths:
                with path.open('rb') as source:
                    while chunk := source.read(CHUNK):
                        output.write(chunk)
    return {**file_record(destination), 'bytes_received': sum(r['bytes_received'] for r in records)}


def acquire(candidates: list[dict], directory: Path, byte_budget: int,
            seconds: float = 120, workers: int = 3, range_parts: int = 1) -> dict:
    """Reserve full asset size before parallel downloads; failed reservations count.

    No automatic retry, no trust in filenames alone, no automatic package mapping.
    Cache hits are hashed again. Only exact size + pre-pinned SHA successes survive.
    """
    if not 0 < seconds <= 120 or not 1 <= workers <= 8 or not 1 <= range_parts <= 8 or byte_budget < 0:
        raise ValueError('invalid acquisition budget')
    check_path(directory)
    planned, results, reserved, seen = [], [], 0, set()
    for asset in candidates:
        require_sha(asset['sha256'])
        official_url(asset['url'])
        official_url(asset['evidence_url'])
        name = safe_relative(asset['name'])
        if '/' in name or name.casefold() in seen:
            raise ValueError('duplicate/non-flat asset name')
        seen.add(name.casefold())
        size = asset['size']
        if type(size) is not int or size <= 0:
            raise ValueError('positive pinned size required')
        path = directory / name
        partial = directory / (name + '.partial')
        check_path(partial)
        if partial.exists():
            raise ValueError(f'pre-existing partial file: {partial}')
        if path.exists():
            record = file_record(path)
            if record != {'sha256': asset['sha256'], 'size': size}:
                raise ValueError(f'existing archive differs: {name}')
            results.append({**asset, 'status': 'verified-cache', 'path': name})
        elif reserved + size + 65536 * range_parts > byte_budget:
            results.append({**asset, 'status': 'budget-skipped'})
        else:
            reserved += size + 65536 * range_parts  # also charged on failure
            planned.append(asset)
    def download(asset):
        path = directory / asset['name']
        partial = directory / (asset['name'] + '.partial')
        try:
            record = (ranged_get(asset, partial, seconds, range_parts) if range_parts > 1 else
                      bounded_get(asset['url'], partial, asset['size'] + 65536, seconds))
            if (record['sha256'], record['size']) != (asset['sha256'], asset['size']):
                raise ValueError('download SHA-256/size differs from official pin')
            # The directory is caller-owned; do not clobber an existing archive.
            with path.open('xb') as out, partial.open('rb') as inp:
                while chunk := inp.read(CHUNK):
                    out.write(chunk)
            return {**asset, 'status': 'verified-download', 'path': asset['name'],
                    'bytes_received': record['bytes_received']}
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            return {**asset, 'status': 'download-failed', 'error': str(error)}
        finally:
            if partial.exists():
                partial.unlink()
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        results.extend(pool.map(download, planned))
    return {'approval': False, 'corresponding_source_complete': False,
            'reserved_bytes': reserved, 'byte_budget': byte_budget,
            'assets': sorted(results, key=lambda a: a['name'])}


def bundle_sources(manifest_path: Path, archives: Path, output: Path) -> dict:
    """Deterministic local companion, never a binary-ZIP attachment or publication.

    The manifest intentionally describes acquisition, not verify-v1 role approval.
    Every artifact is rehashed immediately before packaging; no omitted bytes.
    """
    check_path(output)
    if output.exists():
        raise ValueError('bundle already exists')
    manifest = load_json(manifest_path)
    if manifest.get('approval') is not False:
        raise ValueError('source companion cannot grant legal approval')
    complete = manifest.get('corresponding_source_complete')
    if complete is not False:
        review = manifest.get('delivery_review', {})
        packages = manifest.get('packages', [])
        expected = parse_versions(review.get('package_versions_text', '').encode('utf-8'))
        actual = {p['name']: p['version'] for p in packages}
        artifact_ids = {a['id'] for a in manifest['artifacts']}
        evidence_paths = {e['path'] for e in manifest.get('evidence_files', [])}
        if (complete is not True or not expected or actual != expected
                or len(packages) != len(expected) or review.get('unresolved_required_inputs') != []
                or not review.get('scope') or not review.get('evidence_paths')
                or not set(review['evidence_paths']) <= evidence_paths
                or any(not p.get('artifact_ids') or not set(p['artifact_ids']) <= artifact_ids
                       or not p.get('mapping_basis') for p in packages)):
            raise ValueError('complete companion requires explicit scoped delivery review and exact coverage')
    entries = [('source-manifest.json', manifest_path)]
    for asset in manifest['artifacts']:
        name = safe_relative(asset['path'])
        require_sha(asset['sha256'])
        path = archives / name
        if file_record(path) != {'sha256': asset['sha256'], 'size': asset['size']}:
            raise ValueError(f'bundle input differs: {name}')
        entries.append(('archives/' + name, path))
    for item in manifest.get('evidence_files', []):
        name = safe_relative(item['path'])
        path = manifest_path.parent / name
        if file_record(path) != {'sha256': item['sha256'], 'size': item['size']}:
            raise ValueError(f'bundle evidence differs: {name}')
        entries.append(('evidence/' + name, path))
    names = [name.casefold() for name, _ in entries]
    if len(names) != len(set(names)):
        raise ValueError('duplicate bundle member')
    with zipfile.ZipFile(output, 'x', compression=zipfile.ZIP_STORED) as archive:
        for name, path in sorted(entries):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            with archive.open(info, 'w', force_zip64=True) as out, path.open('rb') as inp:
                while chunk := inp.read(CHUNK):
                    out.write(chunk)
    return {**file_record(output), 'approval': False, 'corresponding_source_complete': complete,
            'artifact_count': len(manifest['artifacts']), 'published': False,
            'release_ready': False,
            'note': 'Packaging validates integrity and declared review coverage, not legal conclusions or publication.'}


def write_json(path: Path, data: dict) -> None:
    check_path(path)
    # Exclusive creation protects manually reviewed evidence and prior reports.
    with path.open('x', encoding='utf-8', newline='\n') as stream:
        json.dump(data, stream, ensure_ascii=False, indent=2)
        stream.write('\n')


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    inv = sub.add_parser('inventory')
    inv.add_argument('--runtime', required=True, type=Path)
    dis = sub.add_parser('discover', help='network: bounded official JSON only, never archives')
    dis.add_argument('--manifest', required=True, type=Path)
    val = sub.add_parser('verify')
    val.add_argument('--runtime', required=True, type=Path)
    val.add_argument('--manifest', required=True, type=Path)
    val.add_argument('--archives', required=True, type=Path)
    ins = sub.add_parser('inspect')
    ins.add_argument('--archive', required=True, type=Path)
    ins.add_argument('--nested', action='store_true', help='inspect nested inputs without executing build scripts')
    acq = sub.add_parser('acquire', help='explicit bounded pinned official archive downloads')
    acq.add_argument('--candidates', required=True, type=Path, help='JSON object with assets list')
    acq.add_argument('--archives', required=True, type=Path)
    acq.add_argument('--byte-budget', required=True, type=int)
    acq.add_argument('--seconds', type=float, default=110)
    acq.add_argument('--workers', type=int, default=3)
    acq.add_argument('--range-parts', type=int, default=1)
    bun = sub.add_parser('bundle', help='offline deterministic, explicitly incomplete companion ZIP')
    bun.add_argument('--manifest', required=True, type=Path)
    bun.add_argument('--archives', required=True, type=Path)
    bun.add_argument('--bundle', required=True, type=Path)
    for command in (inv, dis, val, ins, acq, bun):
        command.add_argument('--output', required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        check_path(args.output)
        if args.output.exists():
            raise ValueError('output already exists; choose a new path')
        if args.command == 'inventory':
            result = inventory(args.runtime)
        elif args.command == 'discover':
            result = discover(load_json(args.manifest))
        elif args.command == 'inspect':
            result = inspect_source_tree(args.archive) if args.nested else inspect_archive(args.archive)
        elif args.command == 'acquire':
            result = acquire(load_json(args.candidates)['assets'], args.archives,
                             args.byte_budget, args.seconds, args.workers, args.range_parts)
        elif args.command == 'bundle':
            result = bundle_sources(args.manifest, args.archives, args.bundle)
        else:
            result = verify(load_json(args.manifest), args.runtime, args.archives)
        write_json(args.output, result)
        if args.command == 'verify':
            print(f'integrity_ok={result["integrity_ok"]}; approval=False; errors={len(result["errors"])}')
            return 0 if result['integrity_ok'] else 1
        if args.command == 'acquire' and any(not a['status'].startswith('verified-') for a in result['assets']):
            print(f'acquisition incomplete: {args.output}; not a compliance approval')
            return 1
        print(f'{args.command}: {args.output}; not a compliance approval')
        return 0
    except (OSError, ValueError, KeyError, TypeError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f'runtime sources: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
