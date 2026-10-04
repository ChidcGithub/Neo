#!/usr/bin/env python3
"""Build a new, pinned GCM-free MinGit distribution; never edit the developer cache.

Run on a trusted, quiescent local filesystem. No network, credential commands,
GUI, user configuration writes, or in-place output replacement are supported.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import tempfile
import zipfile

if __package__:
    from .runtime_sources import check_path, file_record, load_json, safe_relative, snapshot
else:
    from runtime_sources import check_path, file_record, load_json, safe_relative, snapshot

POLICY = Path(__file__).with_name('gitbash-distribution-policy.json')
FILTERED_FILES_SHA256 = 'ea7f28118cd09f9be62f40f6637a5444c10b2a950f162b615e491275916e9bdc'
FETCH_METADATA = {
    '.neo-version': 'v2.55.0.windows.5',
    '.neo-archive-sha256': '56d7b226b7693196cfc71fef26568f536c4a021ab6c37ff2db4287bed908e96e',
}
DISTRIBUTION_DOCUMENTS = ('MANIFEST.json', 'MODIFICATIONS.md', 'gitbash-distribution-policy.json')
GCM_PACKAGE = 'mingw-w64-x86_64-git-credential-manager'
GCM_ARTIFACT = 'mingw-w64-git-credential-manager'
MANAGED_HELPERS = {'manager', 'manager-core', 'selector', 'helper-selector'}
MACHINE_INCLUDES = {'c:/program files/git/etc/gitconfig',
                    'c:/program files (x86)/git/etc/gitconfig'}


def digest_json(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                     ensure_ascii=False).encode()).hexdigest()


def write_json(path: Path, value: object) -> None:
    with path.open('x', encoding='utf-8', newline='\n') as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write('\n')


def records(files: list[dict]) -> dict[str, dict]:
    result, seen = {}, set()
    for item in files:
        name = safe_relative(item['path'])
        if name.casefold() in seen:
            raise ValueError(f'duplicate/case-colliding path: {name}')
        seen.add(name.casefold())
        if not re.fullmatch('[0-9a-f]{64}', item['sha256']) or item['size'] < 0:
            raise ValueError(f'invalid file record: {name}')
        result[name] = {k: item[k] for k in ('sha256', 'size')}
    return result


def config_value(raw: str) -> str:
    """Decode a single Git config value, preserving unrelated lines verbatim."""
    out, quoted, escaped = [], False, False
    for char in raw.strip():
        if escaped:
            if char not in 'n tb\\"'.replace(' ', ''):
                raise ValueError('unsupported Git config escape')
            out.append({'n': '\n', 't': '\t', 'b': '\b'}.get(char, char))
            escaped = False
        elif char == '\\':
            escaped = True
        elif char == '"':
            quoted = not quoted
        elif char in '#;' and not quoted:
            break
        else:
            out.append(char)
    if escaped or quoted:
        raise ValueError('multiline/unterminated config requires a new reviewed policy')
    return ''.join(out).strip()


def sanitize_config(raw: bytes) -> tuple[bytes, list[dict]]:
    section, output, removed = '', [], []
    for number, line in enumerate(raw.decode('utf-8').splitlines(keepends=True), 1):
        stripped = line.strip()
        header = re.fullmatch(r'\[([\w-]+)(?:\s+"(?:[^"\\]|\\.)*"|\.[^\]]+)?\]\s*(?:[#;].*)?', stripped)
        if header:
            section = header[1].lower()
        elif stripped.startswith('['):
            raise ValueError('unsupported Git config section')
        assignment = re.match(r'^\s*([\w-]+)\s*=\s*(.*?)(?:\r?\n)?$', line)
        reason = None
        if assignment:
            key = assignment[1].lower()
            if section == 'credential' and key == 'helper':
                if config_value(assignment[2]).lower() in MANAGED_HELPERS:
                    reason = 'removed managed GCM/selector helper only'
            elif section == 'include' and key == 'path':
                if config_value(assignment[2]).replace('\\', '/').lower() in MACHINE_INCLUDES:
                    reason = 'do not inherit the machine Git installation configuration'
        if reason:
            removed.append({'line': number, 'text': line.rstrip('\r\n'), 'reason': reason})
        else:
            output.append(line)
    return ''.join(output).encode('utf-8'), removed


def pe_imports(data: bytes) -> list[str]:
    """Read normal and delay-load PE imports without loading/executing a DLL."""
    def unpack(fmt, offset):
        size = struct.calcsize(fmt)
        if offset < 0 or offset + size > len(data):
            raise ValueError('truncated PE')
        return struct.unpack_from(fmt, data, offset)

    if data[:2] != b'MZ':
        raise ValueError('EXE/DLL is not PE')
    pe, = unpack('<I', 0x3c)
    if data[pe:pe + 4] != b'PE\0\0':
        raise ValueError('invalid PE signature')
    sections, = unpack('<H', pe + 6)
    optional_size, = unpack('<H', pe + 20)
    opt = pe + 24
    magic, = unpack('<H', opt)
    if magic == 0x20b:
        directories, count_offset = opt + 112, opt + 108
        image_base, = unpack('<Q', opt + 24)
    elif magic == 0x10b:
        directories, count_offset = opt + 96, opt + 92
        image_base, = unpack('<I', opt + 28)
    else:
        raise ValueError('unsupported PE optional header')
    count, = unpack('<I', count_offset)
    if directories + count * 8 > opt + optional_size:
        raise ValueError('PE directories outside optional header')
    table = []
    for i in range(sections):
        virtual_size, rva, raw_size, raw_offset = unpack('<IIII', opt + optional_size + i * 40 + 8)
        table.append((rva, raw_size, raw_offset))

    def offset(rva):
        for start, size, raw in table:
            if start <= rva < start + size:
                return raw + rva - start
        raise ValueError(f'PE RVA outside file-backed sections: {rva:x}')

    def name(rva):
        start = offset(rva)
        end = data.find(b'\0', start, start + 4096)
        if end < 0:
            raise ValueError('unterminated PE import name')
        value = data[start:end].decode('ascii').lower()
        if not value or '/' in value or '\\' in value:
            raise ValueError('unsafe PE import name')
        return value

    result = set()
    for index, width in ((1, 20), (13, 32)):
        if count <= index:
            continue
        rva, size = unpack('<II', directories + index * 8)
        if not rva:
            continue
        start = offset(rva)
        for delta in range(0, size, width):
            values = unpack('<' + 'I' * (width // 4), start + delta)
            if not any(values):
                break
            if index == 1:
                name_rva = values[3]
            else:
                name_rva = values[1] if values[0] & 1 else values[1] - image_base
            result.add(name(name_rva))
        else:
            raise ValueError('unterminated PE import directory')
    return sorted(result)


def dependency_review(runtime: Path, before: dict, removed: set[str]) -> dict:
    original_dlls = {Path(p).name.lower() for p in before if p.lower().endswith('.dll')}
    retained_dlls = {Path(p).name.lower() for p in before if p not in removed and p.lower().endswith('.dll')}
    missing, imports = [], {}
    for path in sorted(before):
        if path in removed or not path.lower().endswith(('.exe', '.dll')):
            continue
        dependencies = pe_imports((runtime / path).read_bytes())
        imports[path] = dependencies
        for dll in dependencies:
            if dll in original_dlls and dll not in retained_dlls:
                missing.append({'path': path, 'removed_import': dll})
    if missing:
        raise ValueError(f'retained PE imports a removed local DLL: {missing}')
    return {'method': 'PE normal + delay imports, case-insensitive original/retained local DLL map',
            'scope_limit': 'Not full Windows loader emulation or a proof about dynamic LoadLibrary calls.',
            'pe_file_count': len(imports), 'imports': imports, 'missing_removed_local_imports': missing}


def smoke(runtime: Path, temporary_parent: Path) -> dict:
    if os.name != 'nt':
        raise ValueError('native smoke validation requires Windows; use --skip-smoke for static-only output')
    with tempfile.TemporaryDirectory(prefix='smoke-', dir=temporary_parent) as td:
        home = Path(td)
        # Deliberately do not inherit GIT_*, BASH_ENV, proxies, credentials, or PATH.
        env = {k: os.environ[k] for k in ('SystemRoot', 'WINDIR', 'COMSPEC') if k in os.environ}
        env.update({'HOME': td, 'USERPROFILE': td, 'XDG_CONFIG_HOME': td,
                    'APPDATA': td, 'LOCALAPPDATA': td, 'TEMP': td, 'TMP': td,
                    'PATH': os.pathsep.join(str(runtime / p) for p in ('cmd', 'mingw64/bin', 'usr/bin')),
                    'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_SYSTEM': os.devnull,
                    'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_TERMINAL_PROMPT': '0',
                    'GCM_INTERACTIVE': 'Never', 'GIT_CEILING_DIRECTORIES': str(home.parent)})
        git = str(runtime / 'cmd/git.exe')
        commands = [[str(runtime / 'usr/bin/bash.exe'), '--noprofile', '--norc', '-c', 'echo neo-runtime-ok'],
                    [git, '--version'],
                    [git, 'config', '--file', str(runtime / 'etc/gitconfig'), '--no-includes', '--list'],
                    [git, 'config', '--list', '--show-origin']]
        results = []
        for command in commands:
            run = subprocess.run(command, cwd=home, env=env, capture_output=True,
                                 text=True, encoding='utf-8', errors='replace', timeout=30,
                                 creationflags=subprocess.CREATE_NO_WINDOW)
            if run.returncode:
                raise ValueError(f'smoke failed: {command}: {run.stderr}')
            results.append({'arguments': [arg.replace(str(runtime), '<runtime>') for arg in command[1:]],
                                        'stdout': run.stdout, 'stderr': run.stderr,
                            'returncode': run.returncode})
        if results[0]['stdout'].strip() != 'neo-runtime-ok' or results[-1]['stdout'].strip():
            raise ValueError(f'smoke environment not isolated: {results}')
        if 'credential.helper=' in results[2]['stdout'] or 'include.path=' in results[2]['stdout']:
            raise ValueError('distribution still configures a helper or machine include')
        return {'status': 'passed', 'network_or_credentials_requested': False, 'commands': results}


def load_policy(path: Path) -> dict:
    policy = load_json(path)
    if policy.get('schema') != 'neo-gitbash-distribution-v1':
        raise ValueError('unknown distribution policy')
    original = records(policy['source_files'])
    removed = records(policy['remove_files'])
    if len(original) != 366 or len(removed) != 56:
        raise ValueError('unreviewed input/removal count')
    if policy['source_files_sha256'] != digest_json(policy['source_files']):
        raise ValueError('source manifest digest mismatch')
    if any(original.get(p) != r for p, r in removed.items()):
        raise ValueError('removal not bound to original snapshot')
    if len([p for p in removed if p.lower().endswith(('.exe', '.dll'))]) != 51:
        raise ValueError('incomplete GCM plus selector executable removal')
    for path in ('mingw64/bin/Microsoft.Identity.Client.NativeInterop.dll', 'mingw64/bin/msalruntime.dll'):
        if path not in removed or removed[path]['sha256'] not in policy['blocked_sha256']:
            raise ValueError('NativeInterop removal/hash mapping missing')
    return policy


def verify_snapshot(source: Path, policy: dict) -> dict:
    state, _ = snapshot(source)
    metadata = {f['path'] for f in state['files']} & FETCH_METADATA.keys()
    if metadata and metadata != FETCH_METADATA.keys():
        raise ValueError('incomplete Fetch metadata')
    for name in metadata:
        if (source / name).read_bytes() != FETCH_METADATA[name].encode():
            raise ValueError('unknown/tampered Fetch metadata: ' + name)
    state['files'] = [f for f in state['files'] if f['path'] not in metadata]
    if state['files'] != policy['source_files']:
        raise ValueError('unknown/tampered runtime: full manifest does not match approved 366 files')
    return state


def verify_distribution(directory: Path, runtime: Path | None = None) -> dict:
    """Validate reviewed output bytes, not a caller-supplied inventory alone."""
    check_path(directory)
    for name in DISTRIBUTION_DOCUMENTS:
        check_path(directory / name)
        if not (directory / name).is_file() or not (directory / name).read_bytes().strip():
            raise ValueError('missing distribution marker/document: ' + name)
    policy = load_policy(POLICY)
    if file_record(directory / 'gitbash-distribution-policy.json') != file_record(POLICY):
        raise ValueError('unknown distribution policy bytes')
    report = load_json(directory / 'MANIFEST.json')
    state, _ = snapshot(runtime if runtime is not None else directory / 'runtime/gitbash')
    if (report.get('schema') != 'neo-gitbash-distribution-manifest-v1'
            or report.get('policy') != file_record(POLICY)
            or report.get('original_runtime', {}).get('files') != policy['source_files']
            or report.get('removed') != policy['remove_files']
            or report.get('runtime') != state
            or len(state['files']) != 310
            or digest_json(state['files']) != FILTERED_FILES_SHA256
            or report.get('runtime_files_sha256') != FILTERED_FILES_SHA256):
        raise ValueError('unknown/tampered filtered runtime: expected exact reviewed 310 files')
    before, after = records(policy['source_files']), records(state['files'])
    changes = [{'path': n, 'before': before[n], 'after': r}
               for n, r in after.items() if r != before[n]]
    if (report.get('modifications') != changes
            or {r['path'] for r in changes} != {'etc/gitconfig', 'etc/package-versions.txt'}):
        raise ValueError('distribution modifications marker mismatch')
    assert_absent(state['files'], policy)
    return report


def verify_companion(directory: Path, distribution: dict) -> dict:
    """Reuse the selected immutable companion; never rebuild/download source archives."""
    record = load_json(directory / 'source-companion-record.json')
    archive = directory / safe_relative(record['path'])
    if file_record(archive) != {k: record[k] for k in ('sha256', 'size')}:
        raise ValueError('source companion hash mismatch')
    source = load_json(directory / 'source-manifest.json')
    if (source.get('schema') != 'neo-gitbash-no-gcm-source-v1'
            or source.get('runtime') != distribution['runtime']
            or source.get('corresponding_source_complete') is not True
            or len(source.get('packages', [])) != 57
            or any(p['name'] == GCM_PACKAGE for p in source['packages'])
            or any(a['id'] == GCM_ARTIFACT for a in source['artifacts'])):
        raise ValueError('source companion/runtime binding mismatch')
    with zipfile.ZipFile(archive) as bundle:
        members = records(record['members'])
        if (len(bundle.namelist()) != len(members) or set(bundle.namelist()) != set(members)
                or set(load_policy(POLICY)['source_companion']['exclude_members']) & set(members)):
            raise ValueError('source companion members mismatch')
        for member, local in {
            'source-manifest.json': 'source-manifest.json',
            'MODIFICATIONS.md': 'MODIFICATIONS.md',
            'distribution/MANIFEST.json': 'MANIFEST.json',
            'distribution/gitbash-distribution-policy.json': 'gitbash-distribution-policy.json',
        }.items():
            if bundle.read(member) != (directory / local).read_bytes():
                raise ValueError('source companion binding mismatch: ' + member)
    return record


def assert_absent(files: list[dict], policy: dict) -> None:
    removed = {f['path'].casefold() for f in policy['remove_files']}
    blocked = set(policy['blocked_sha256'])
    for item in files:
        if item['path'].casefold() in removed or item['sha256'] in blocked:
            raise ValueError(f'removed component bytes/path survived: {item["path"]}')


def build_companion(old_zip: Path, stage: Path, policy: dict, distribution: dict,
                    policy_path: Path) -> dict:
    """Repackage only the pinned historical companion, excluding the entire GCM archive."""
    expected = policy['source_companion']
    if file_record(old_zip) != expected['record']:
        raise ValueError('unknown/tampered historical source companion')
    destination = stage / 'gitbash-source-companion-no-gcm.zip'
    excluded = expected['exclude_members']
    with zipfile.ZipFile(old_zip) as source:
        names = source.namelist()
        if len(names) != len(set(n.casefold() for n in names)):
            raise ValueError('duplicate companion members')
        for member in source.infolist():
            safe_relative(member.filename)
            if stat.S_ISLNK(member.external_attr >> 16):
                raise ValueError('symlink in source companion')
        if not set(excluded) <= set(names):
            raise ValueError('GCM archive absent from pinned historical companion')
        original = json.loads(source.read('source-manifest.json'))
        if original['runtime']['files'] != policy['source_files']:
            raise ValueError('companion/runtime mismatch')
        artifacts = [a for a in original['artifacts'] if a['id'] != GCM_ARTIFACT]
        packages = [p for p in original['packages'] if p['name'] != GCM_PACKAGE]
        if len(packages) != 57 or len(artifacts) != len(original['artifacts']) - 1:
            raise ValueError('unexpected companion package coverage')
        manifest = {
            'schema': 'neo-gitbash-no-gcm-source-v1', 'approval': False,
            'published': False, 'release_ready': False, 'corresponding_source_complete': True,
            'runtime': distribution['runtime'], 'packages': packages, 'artifacts': artifacts,
            'original_companion': expected['record'], 'removed_members': excluded,
            'evidence_files': original['evidence_files'],
            'delivery_review': copy.deepcopy(original['delivery_review']),
            'scope': '57 conservative source packages; only GCM source/payload removed. '
                     'Other SDK packages retained even without a shipped file. Historical evidence '
                     'and acquisition recipes describe the original 58-package input, not this output.',
            'changes': 'See distribution/MANIFEST.json and distribution/gitbash-distribution-policy.json',
            'publication_conditions': 'Local only. Publish matching binary/source with equivalent access '
                                      'only after parent release review; no URL or legal approval granted.'}
        manifest['delivery_review']['scope'] = manifest['scope']
        manifest['delivery_review']['package_versions_text'] = (
            stage / 'runtime/gitbash/etc/package-versions.txt').read_text(encoding='utf-8')
        write_json(stage / 'source-manifest.json', manifest)
        modifications = (
            '# MinGit 2.55.0.5: GCM-free distribution\n\n'
            'The full GCM component and git-extra helper-selector executable were removed.\n'
            'Git, Bash, wincred, other shared DLLs and retained original licenses are unchanged.\n'
            'etc/gitconfig removes only managed helpers and the two machine Git includes.\n'
            'etc/package-versions.txt drops only the GCM package; other source packages remain conservative.\n'
            'Personal Git configuration is never modified. If it names manager/manager-core/selector,\n'
            'the owner must adjust it or select an independently installed, permitted helper.\n'
            'A system config cannot override later global helpers: isolated callers must set\n'
            'GIT_CONFIG_GLOBAL to a private empty file (and control GIT_CONFIG_SYSTEM), or use\n'
            'git -c credential.helper= for an individual command. Neo is not changed here.\n\n'
            'Reproduce with the included preparation script/policy and the pinned original runtime.\n'
            'Retained source archives, patches, package recipes and fixed builder are byte unchanged.\n'
            'The entire GCM source archive, including its nested proprietary binary payload, is absent.\n'
            'Historical evidence/notices may still mention removed components; no GCM binaries ship.\n'
            'Historical acquisition scripts are evidence only: do not run them to build this distribution.\n'
            'PE import checks are static, not complete dynamic-loader or legal approval.\n'
            'This companion has not been published and does not constitute a written source offer.\n')
        (stage / 'MODIFICATIONS.md').write_text(modifications, encoding='utf-8', newline='\n')
        additions = {'source-manifest.json': stage / 'source-manifest.json',
                     'MODIFICATIONS.md': stage / 'MODIFICATIONS.md',
                     'distribution/MANIFEST.json': stage / 'MANIFEST.json',
                     'distribution/gitbash-distribution-policy.json': policy_path,
                     'distribution/prepare_runtime_distribution.py': Path(__file__),
                     'distribution/test_prepare_runtime_distribution.py': Path(__file__).with_name('test_prepare_runtime_distribution.py'),
                     'distribution/runtime_sources.py': Path(__file__).with_name('runtime_sources.py'),
                     'distribution/etc/gitconfig': stage / 'runtime/gitbash/etc/gitconfig',
                     'distribution/etc/package-versions.txt': stage / 'runtime/gitbash/etc/package-versions.txt'}
        members = []
        with zipfile.ZipFile(destination, 'x', compression=zipfile.ZIP_STORED) as output:
            for name in sorted(set(names) - set(excluded) - {'source-manifest.json'}):
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system, info.external_attr = 3, 0o100644 << 16
                digest, size = hashlib.sha256(), 0
                with source.open(name) as inp, output.open(info, 'w', force_zip64=True) as out:
                    while chunk := inp.read(1024 * 1024):
                        digest.update(chunk)
                        size += len(chunk)
                        out.write(chunk)
                record = {'path': name, 'sha256': digest.hexdigest(), 'size': size}
                if record['sha256'] in policy['blocked_sha256']:
                    raise ValueError(f'blocked payload hash in companion: {name}')
                members.append(record)
            for name, path in sorted(additions.items()):
                check_path(path)
                info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system, info.external_attr = 3, 0o100644 << 16
                output.writestr(info, path.read_bytes())
                members.append({'path': name, **file_record(path)})
        by_name = {r['path']: r for r in members}
        for artifact in artifacts:
            record = by_name['archives/' + safe_relative(artifact['path'])]
            if any(record[k] != artifact[k] for k in ('sha256', 'size')):
                raise ValueError('retained source artifact differs')
        # Re-hashing the immutable old ZIP also detects ordinary concurrent changes.
        if file_record(old_zip) != expected['record']:
            raise ValueError('historical companion changed while reading')
    return {'path': destination.name, **file_record(destination), 'package_count': len(packages),
            'artifact_count': len(artifacts), 'members': sorted(members, key=lambda r: r['path']),
            'excluded_members': excluded, 'approval': False, 'published': False,
            'blocked_payload_absence_basis': 'Pinned historical ZIP; entire GCM source archive omitted '
                '(including nested gcm-win-x64 payload); retained archive hashes unchanged. '
                'No recursive execution/extraction of unrelated test fixtures.'}


def prepare(source: Path, output: Path, trusted_root: Path, policy_path: Path = POLICY,
            old_companion: Path | None = None, run_smoke: bool = True) -> dict:
    for path in (source, output, trusted_root, policy_path):
        if '..' in path.parts:
            raise ValueError('parent traversal is not allowed')
        check_path(path)
    source, output, trusted_root = (p.resolve() for p in (source, output, trusted_root))
    if not trusted_root.is_dir() or not output.is_relative_to(trusted_root) or output == trusted_root:
        raise ValueError('output must be a new descendant of explicit existing trusted root')
    if source.is_relative_to(output) or output.is_relative_to(source):
        raise ValueError('source/output overlap')
    if output.exists() or not output.parent.is_dir():
        raise ValueError('output must not exist and its parent must already exist')
    policy = load_policy(policy_path)
    state = verify_snapshot(source, policy)
    before = records(state['files'])
    removed = set(records(policy['remove_files']))
    lock = output.parent / ('.' + output.name + '.lock')
    check_path(lock)
    fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    try:
        with tempfile.TemporaryDirectory(prefix='.' + output.name + '-stage-', dir=output.parent) as td:
            stage = Path(td)
            runtime = stage / 'runtime/gitbash'
            runtime.mkdir(parents=True)
            for name, expected in before.items():
                if name in removed:
                    continue
                src, dst = source / name, runtime / name
                check_path(src)
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(src, dst)
                if file_record(dst) != expected:
                    raise ValueError(f'input changed while copying: {name}')
            config = runtime / 'etc/gitconfig'
            new_config, config_edits = sanitize_config(config.read_bytes())
            config.write_bytes(new_config)
            versions = runtime / 'etc/package-versions.txt'
            lines = versions.read_bytes().splitlines(keepends=True)
            keep = [line for line in lines if line.split()[:1] != [GCM_PACKAGE.encode()]]
            if len(keep) != len(lines) - 1:
                raise ValueError('GCM package inventory differs')
            versions.write_bytes(b''.join(keep))
            after, packages = snapshot(runtime)
            assert_absent(after['files'], policy)
            after_map = records(after['files'])
            if set(after_map) != set(before) - removed:
                raise ValueError('unexpected distribution file set')
            modifications = []
            for name, record in after_map.items():
                if record != before[name]:
                    if name not in ('etc/gitconfig', 'etc/package-versions.txt'):
                        raise ValueError(f'unexpected modified retained file: {name}')
                    modifications.append({'path': name, 'before': before[name], 'after': record})
            dependencies = dependency_review(runtime, before, removed)
            smoke_result = smoke(runtime, stage) if run_smoke else {'status': 'not-run'}
            if snapshot(runtime)[0] != after:
                raise ValueError('smoke modified distribution runtime')
            verify_snapshot(source, policy)
            report = {'schema': 'neo-gitbash-distribution-manifest-v1', 'approval': False,
                      'published': False, 'release_ready': False, 'policy': file_record(policy_path),
                                            'preparation_script': file_record(Path(__file__)),
                                            'runtime_files_sha256': digest_json(after['files']),
                      'original_runtime': state, 'runtime': after, 'removed': policy['remove_files'],
                      'modifications': modifications, 'gitconfig_removed_lines': config_edits,
                      'dependency_review': dependencies, 'smoke': smoke_result,
                      'counts': {'original': len(before), 'removed': len(removed), 'retained': len(after_map),
                                 'modified': len(modifications), 'source_packages': len(packages)},
                      'original_source_unchanged': True,
                      'source_companion_required': True,
                      'notes': ['No user/global config changed; personal helpers may require owner adjustment.',
                                'Original licenses for retained files copied unchanged; wincred is not GCM.',
                                'Build on a trusted quiescent filesystem; not a defense against hostile concurrent writers.']}
            write_json(stage / 'MANIFEST.json', report)
            shutil.copyfile(policy_path, stage / 'gitbash-distribution-policy.json')
            if old_companion is None:
                (stage / 'MODIFICATIONS.md').write_text(
                    '# MinGit 2.55.0.5: GCM-free distribution\n\n'
                    'Removed GCM and helper-selector; retained Git/Bash/wincred and original licenses.\n'
                    'etc/gitconfig removes managed helpers and machine Git includes;\n'
                    'etc/package-versions.txt removes the GCM package. See MANIFEST.json for exact hashes.\n'
                    'Personal/global configuration is unchanged and can still name removed helpers;\n'
                    'the owner must adjust it or isolate Git configuration per process.\n'
                    'Runtime-only preparation: corresponding source NOT supplied or verified.\n'
                    'Publication remains BLOCKED pending matching source delivery and release review.\n',
                    encoding='utf-8', newline='\n')
            if old_companion is not None:
                companion = build_companion(old_companion, stage, policy, report, policy_path)
                write_json(stage / 'source-companion-record.json', companion)
            verify_snapshot(source, policy)
            check_path(output)
            if output.exists():
                raise ValueError('output appeared during staging')
            # Same-parent rename exposes the complete tree only after every check succeeds.
            os.rename(stage, output)
        return report
    finally:
        lock.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--trusted-output-root', type=Path, required=True)
    parser.add_argument('--policy', type=Path, default=POLICY)
    parser.add_argument('--source-companion', type=Path)
    parser.add_argument('--skip-smoke', action='store_true', help='static-only; not validated for native use')
    args = parser.parse_args()
    report = prepare(args.source, args.output, args.trusted_output_root, args.policy,
                     args.source_companion, not args.skip_smoke)
    print(json.dumps({'output': str(args.output.absolute()), 'counts': report['counts'],
                      'smoke': report['smoke']['status']}, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())