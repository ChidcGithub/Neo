#!/usr/bin/env python3
"""Read-only final full-function package/ZIP integrity checks (Python 3.11+).

Before inventory:
  python -B tools/verify_release_variants.py package --package dist/neo-int8 --variant int8 --version 0.1.0
After inventory and ZIP creation:
  python -B tools/verify_release_variants.py archive --package dist/neo-int8 --variant int8 --version 0.1.0 --archive dist/neo-0.1.0-int8-portable-x64.zip

After installer creation (7z reads the EXE; never execute the installer):
  python -B tools/verify_release_variants.py installer --package dist/neo-int8 --variant int8 --version 0.1.0 --archive dist/neo-0.1.0-int8-installer-x64.exe --work-root target/verify-release-variants

Use fp32 and its corresponding paths for FP32. Package/ZIP checks write nothing.
Installer checks invoke only a trusted 7z extractor (--extractor, default PATH),
using a cleaned temporary directory under --work-root. No downloads, installation,
feature removal, or release/legal approval. Requires the current main build at
 target/x86_64-pc-windows-msvc/release/neo.exe. Byte equality to that build is
not a source/build attestation or final static-library identity verification.
Use trusted, quiescent directories; concurrent hostile mutation is unsupported.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import mmap
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zipfile

if __package__:
    from . import assemble_drawing_release as drawing
    from . import check_release
    from . import prepare_math_models as math_models
    from . import stage_source_companions as sources
    from . import verify_ort_sources as ort
    from . import verify_sensevoice_notices as sensevoice
    from .package_combined import PEImports, source_allowed, tree_digest, tree_files
else:
    import assemble_drawing_release as drawing
    import check_release
    import prepare_math_models as math_models
    import stage_source_companions as sources
    import verify_ort_sources as ort
    import verify_sensevoice_notices as sensevoice
    from package_combined import PEImports, source_allowed, tree_digest, tree_files

ROOT = Path(__file__).resolve().parents[1]
MAIN_BUILD = 'target/x86_64-pc-windows-msvc/release/neo.exe'
DRAWING_LEGAL = 'docs/licenses/NeoRuntime-drawing/'
DRAWING_SOURCE = 'sources/NeoRuntime-drawing/'
DRAWING_FILES = DRAWING_LEGAL + 'FILES.sha256.json'
MANIFEST = sources.PACKAGE_MANIFEST
CHUNK = 1024 * 1024
MAX_JSON = 32 * CHUNK
MAX_ENTRIES = 50000
NSIS_LIMIT = 2 * 1024**3
# Uncompressed payload is deliberately conservative: allow space for inventory,
# installer stub/art/plugins and compression overhead, not guessed compression.
NSIS_HEADROOM = 64 * CHUNK
NSIS_PAYLOAD_LIMIT = NSIS_LIMIT - NSIS_HEADROOM
INSTALLER_ICON = 'target/package/installer-art/neo.ico'
EXTRACT_SECONDS = 300
MAX_TOOL_OUTPUT = 16 * CHUNK
MAX_UNINSTALLER = 16 * CHUNK
MAX_PLUGINS = 32 * CHUNK
MAX_PLUGIN_FILES = 64
EXTRACTION_FAILURE = ('Installer extraction is incomplete/unsupported; no weaker fallback passes. '
                      'Use a trusted 7-Zip version capable of listing and extracting the complete NSIS '
                      'payload including uninstall.exe, then rerun; never run the installer as a fallback.')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def regular(path):
    path = math_models.checked(path)
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
            'Expected regular unlinked file: ' + str(path))
    return path


def read_json(path):
    path = regular(path)
    require(path.stat().st_size <= MAX_JSON, 'JSON size budget exceeded: ' + str(path))
    return json.loads(path.read_text(encoding='utf-8'), object_pairs_hook=math_models.unique_object)


def stream_record(stream, limit):
    digest, size = hashlib.sha256(), 0
    while block := stream.read(min(CHUNK, limit - size + 1)):
        size += len(block)
        require(size <= limit, 'Stream expanded-size budget exceeded')
        digest.update(block)
    return {'size': size, 'sha256': digest.hexdigest()}


def file_record(path):
    path = regular(path)
    before = path.stat()
    with path.open('rb') as stream:
        record = stream_record(stream, before.st_size)
    after = path.stat()
    require((before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns)
            and record['size'] == after.st_size, 'File changed while hashing: ' + str(path))
    return record


def budget(size):
    require(size < NSIS_PAYLOAD_LIMIT,
            f'Full payload is {size} bytes; NSIS requires < {NSIS_PAYLOAD_LIMIT} bytes '
            f'(2 GiB minus {NSIS_HEADROOM} bytes headroom). Release BLOCKED; '
            'do not remove features/models automatically. Review the packaging strategy.')


def verify_pe(path, *, executable=False):
    """Read-only x64 PE/import structure check, not execution or authenticity."""
    path = regular(path)
    require(path.stat().st_size >= 64, 'Truncated PE: ' + str(path))
    with path.open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
        image = PEImports(data)
        for _, size, offset in image.sections:
            require(offset + size <= len(data), 'Truncated PE section: ' + str(path))
        pe = struct.unpack_from('<I', data, 0x3c)[0]
        flags = struct.unpack_from('<H', data, pe + 22)[0]
        require(flags & 2 and (not executable or not flags & 0x2000),
                'Expected executable PE image: ' + str(path))
        return image.imports()


def bytes_pin(record, label):
    require(isinstance(record, dict) and type(record.get('bytes')) is int
            and record['bytes'] >= 0 and isinstance(record.get('sha256'), str)
            and re.fullmatch('[0-9a-f]{64}', record['sha256']), 'Invalid bytes/hash record: ' + label)
    return {'size': record['bytes'], 'sha256': record['sha256']}


def verify_drawing(package, records, root):
    lock_path = root / 'tools/drawing-runtime.lock.json'
    lock = drawing.load_lock(lock_path)
    source = read_json(package / (DRAWING_LEGAL + 'SOURCE.json'))
    require(source.get('repository') == lock['repository']
            and source.get('lock_sha256') == file_record(lock_path)['sha256'],
            'Drawing SOURCE repository/lock mismatch')
    for key, value in lock['source_binding'].items():
        require(source.get(key) == value, 'Drawing SOURCE binding mismatch: ' + key)
    require(source.get('crt_policy') == 'external-official-msvc-x64-prerequisite',
            'Drawing CRT policy mismatch')
    artifacts = source['artifacts']
    require(set(artifacts) == set(drawing.ARTIFACTS), 'Drawing artifact set mismatch')
    for kind in ('drawing', 'blackboard'):
        for name in ('neo-' + kind + '.exe', 'DirectML.dll'):
            relative = f'apps/{kind}/{name}'
            require(records.get(relative) == bytes_pin(artifacts[name], relative),
                    'Drawing artifact bytes/hash mismatch: ' + relative)
            imports = verify_pe(package / relative, executable=name.endswith('.exe'))
            require(artifacts[name].get('architecture') == 'x64'
                    and artifacts[name].get('imports') == imports, 'Drawing PE receipt mismatch: ' + relative)
            if name.endswith('.exe'):
                require('directml.dll' in imports, 'Drawing executable must import DirectML: ' + relative)
        prerequisite = records.get(f'apps/{kind}/MSVC-PREREQUISITE.txt')
        require(prerequisite and prerequisite['size'] > 0, 'Missing drawing MSVC prerequisite notice')
    require(source['directml_provenance']['sha256'] == artifacts['DirectML.dll']['sha256'],
            'Drawing DirectML provenance hash mismatch')
    for name, digest in source['legal_files'].items():
        math_models.safe_name(name)
        record = records.get(DRAWING_LEGAL + name)
        require(record and record['size'] > 0 and record['sha256'] == digest,
                'Drawing legal hash mismatch: ' + name)
    files = source['files']
    require(isinstance(files, dict) and {'Cargo.toml', 'Cargo.lock', 'LICENSE'} <= files.keys(),
            'Incomplete drawing source file manifest')
    require(tree_digest(files) == source['source_files_sha256'], 'Drawing source inventory digest mismatch')
    for name, pin in files.items():
        math_models.safe_name(name)
        require(source_allowed(name, lock), 'Unexpected drawing source path: ' + name)
        require(records.get(DRAWING_SOURCE + name) == bytes_pin(pin, name),
                'Drawing source bytes/hash mismatch: ' + name)
    require(files['Cargo.lock']['sha256'] == source['cargo_lock_sha256'], 'Drawing Cargo.lock mismatch')
    expected = {DRAWING_LEGAL + name for name in source['legal_files']}
    expected |= {DRAWING_SOURCE + name for name in files}
    expected.add(DRAWING_LEGAL + 'SOURCE.json')
    expected |= {f'apps/{kind}/{name}' for kind in ('drawing', 'blackboard')
                 for name in ('neo-' + kind + '.exe', 'DirectML.dll', 'MSVC-PREREQUISITE.txt')}
    # The assembler inventory predates model preparation. Only this one model
    # subtree is excluded; its exact contents are independently lock-verified.
    actual = {name for name in records if name.startswith((DRAWING_LEGAL, DRAWING_SOURCE,
              'apps/drawing/', 'apps/blackboard/'))
              and not name.startswith('apps/blackboard/models/') and name != DRAWING_FILES}
    require(actual == expected, 'Drawing child file set mismatch')
    inventory = read_json(package / DRAWING_FILES)
    require(isinstance(inventory, dict) and set(inventory) == expected, 'Drawing FILES map set mismatch')
    for name, pin in inventory.items():
        math_models.safe_name(name)
        require(records[name] == bytes_pin(pin, name), 'Drawing FILES bytes/hash mismatch: ' + name)


def verify_package(package, variant, version, *, root=ROOT, require_manifest=False):
    require(variant in math_models.DIRECTORIES, 'Unknown variant')
    sources.check_version(version)
    package = math_models.checked(package)
    require(package.is_dir(), 'Missing package directory: ' + str(package))
    paths = tree_files(package)
    # Reject hardlinks and over-budget payloads before hashing large model files.
    budget(sum(regular(path).stat().st_size for path in paths.values()))
    records = {name: file_record(path) for name, path in paths.items()}
    check_release.validate_payload(package)
    verify_pe(package / 'neo.exe', executable=True)
    require(records['neo.exe'] == file_record(root / MAIN_BUILD),
            'neo.exe differs from current release build')
    sensevoice.verify(root, package)
    source_lock = sources.read_lock(root / 'tools/source-companions.lock.json')
    ort.verify(source_lock, root=root, package=package, required=True)
    notice = read_json(package / 'docs/licenses/SOURCE-ACCESS.json')
    # The existing schema carries the version in tag and source asset names/URLs,
    # not in a top-level version field. Compare the entire generated notice.
    require(notice == sources.source_access(source_lock, notice['repository'], 'v' + version, version),
            'SOURCE-ACCESS release version/source identity mismatch')
    verify_drawing(package, records, root)
    model_root = package / 'apps/blackboard/models'
    require(model_root.is_dir() and {p.name for p in model_root.iterdir()} == {math_models.DIRECTORIES[variant]},
            'Wrong/missing model tree or opposite sibling model present')
    require(not (package / 'apps/drawing/models').exists(), 'Drawing must not contain a duplicate model tree')
    model_lock = math_models.load_lock(root / 'tools/math-models.lock.json')
    math_models.verify_tree(model_root / math_models.DIRECTORIES[variant], model_lock[variant]['files'], variant)
    manifest = {'schema': 1, 'version': version, 'variant': variant,
                'files': {name: record for name, record in sorted(records.items()) if name != MANIFEST}}
    if MANIFEST in records:
        require(read_json(package / MANIFEST) == manifest, 'PACKAGE-MANIFEST differs from package/version/variant')
    else:
        require(not require_manifest, 'Archive verification requires PACKAGE-MANIFEST.json; run inventory first')
    total = sum(record['size'] for record in records.values())
    if MANIFEST not in records:
        total += len((json.dumps(manifest, indent=2) + '\n').encode('utf-8'))
    budget(total)
    return records, total


def verify_zip(archive, records, variant):
    archive = regular(archive)
    require(archive.stat().st_size < NSIS_LIMIT, 'ZIP on-disk size budget exceeded')
    prefix = 'neo-' + variant
    expected = {prefix + '/' + name: record for name, record in records.items()}
    directories = {prefix}
    for name in expected:
        directories.update(str(parent) for parent in Path(name).parents if str(parent) != '.')
    directories = {name.replace('\\', '/') for name in directories}
    with zipfile.ZipFile(archive) as zipped:
        infos = zipped.infolist()
        require(len(infos) <= min(MAX_ENTRIES, len(expected) + len(directories)), 'ZIP member count budget exceeded')
        budget(sum(info.file_size for info in infos))
        seen, members = set(), {}
        for info in infos:
            raw = info.orig_filename
            require(raw == info.filename, 'Normalized/unsafe ZIP member alias')
            name = math_models.safe_name(raw[:-1] if info.is_dir() else raw)
            require(name.casefold() not in seen, 'Duplicate/case-colliding ZIP member: ' + name)
            seen.add(name.casefold())
            kind = stat.S_IFMT(info.external_attr >> 16)
            require(not info.flag_bits & (1 | 64) and not info.external_attr & 0x400
                    and kind in (0, stat.S_IFREG, stat.S_IFDIR), 'Encrypted/link/special ZIP member: ' + name)
            if info.is_dir():
                require(name in directories and not info.file_size and kind != stat.S_IFREG,
                        'Unexpected ZIP directory: ' + name)
                continue
            require(kind != stat.S_IFDIR and not info.external_attr & 0x10, 'ZIP file/directory type mismatch')
            require(name in expected, 'Unexpected ZIP file/root: ' + name)
            require(info.file_size == expected[name]['size'], 'ZIP member size mismatch: ' + name)
            members[name] = info
        require(members.keys() == expected.keys(), 'ZIP missing package files or PACKAGE-MANIFEST')
        for name, info in members.items():
            with zipped.open(info) as stream:
                require(stream_record(stream, expected[name]['size']) == expected[name],
                        'ZIP bytes/hash mismatch: ' + name)


def run_extractor(command, *, cwd, monitor=None):
    """Bound time/output and monitor extracted bytes; never invoke via a shell.

    This is resource supervision, not an OS sandbox: a trusted extractor can
    briefly overshoot the disk limit between polls. Inputs/workspace must remain
    quiescent, and a maintained extractor is required for untrusted archives.
    """
    output = bytearray()
    overflow = threading.Event()
    with subprocess.Popen(command, cwd=cwd, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as process:
        def drain():
            while block := process.stdout.read(65536):
                if len(output) + len(block) > MAX_TOOL_OUTPUT:
                    overflow.set()
                    break
                output.extend(block)
        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        deadline = time.monotonic() + EXTRACT_SECONDS
        try:
            while process.poll() is None:
                require(time.monotonic() < deadline, 'Extractor time budget exceeded')
                require(not overflow.is_set(), 'Extractor output budget exceeded')
                if monitor is not None:
                    monitor()
                time.sleep(0.05)
            reader.join(timeout=5)
            require(not reader.is_alive() and not overflow.is_set(), 'Extractor output budget exceeded')
            require(process.returncode == 0, '7z returned nonzero: ' + str(process.returncode))
            if monitor is not None:
                monitor()
        finally:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
            reader.join(timeout=5)
    return output.decode('utf-8', errors='strict')


def parse_nsis_listing(text, records, icon):
    """7z -slt uses $_N_ for user variables, not their NSIS source names.

    Observed with NSIS 3 / 7-Zip 26: even solid archives list payload sizes,
    but synthesized uninstall.exe has a blank Size and must be bounded later.
    """
    header, separator, body = text.replace('\r\n', '\n').partition('\n----------\n')
    require(separator and '\nType = Nsis\n' in header + '\n', 'Expected 7z NSIS technical listing')
    entries = []
    for block in body.strip('\n').split('\n\n'):
        if not block.strip():
            continue
        fields = {}
        for line in block.splitlines():
            key, delimiter, value = line.partition(' = ')
            require(delimiter and key not in fields, 'Malformed/duplicate NSIS listing field')
            fields[key] = value
        entries.append(fields)
    require(0 < len(entries) <= MAX_ENTRIES, 'NSIS member count budget exceeded')
    seen, files, directories, roots = set(), {}, set(), set()
    for fields in entries:
        name = math_models.safe_name(fields['Path'].replace('\\', '/'))
        require(name.casefold() not in seen, 'Duplicate/case-colliding NSIS member: ' + name)
        seen.add(name.casefold())
        require(fields.get('Encrypted', '-') == '-' and fields.get('Anti', '-') == '-',
                'Encrypted/anti NSIS member')
        require(not any(key in fields for key in ('Symbolic Link', 'Hard Link', 'Reparse', 'Reparse Point', 'Mode')),
                'Linked/reparse NSIS member')
        attributes = fields.get('Attributes', '')
        require(re.fullmatch('[ADRHSCNI _]*', attributes) is not None, 'Unsafe NSIS attributes: ' + name)
        is_dir = fields.get('Folder') == '+' or 'D' in attributes
        require(fields.get('Folder', '-') in ('+', '-'), 'Invalid NSIS folder flag')
        size_text = fields.get('Size', '')
        require(not size_text or re.fullmatch('[0-9]+', size_text), 'Invalid NSIS member size')
        size = int(size_text) if size_text else None
        if is_dir:
            require(size in (None, 0), 'Nonempty NSIS directory')
            directories.add(name)
        else:
            files[name] = size
        top = name.split('/')[0]
        if top != '$PLUGINSDIR':
            require(top == '$INSTDIR' or re.fullmatch(r'\$_[0-9]+_', top), 'Unknown NSIS install root: ' + top)
            roots.add(top)
    require(len(roots) == 1, 'NSIS must have exactly one installation root')
    prefix = roots.pop() + '/'
    require(not any(name in records for name in ('neo.ico', 'uninstall.exe'))
            and not any(name.startswith('$PLUGINSDIR/') for name in records),
            'Package collides with NSIS generated files')
    expected = {prefix + name: pin for name, pin in records.items()}
    expected[prefix + 'neo.ico'] = icon
    uninstall = prefix + 'uninstall.exe'
    require(uninstall in files, 'NSIS listing missing uninstall.exe')
    plugins = {name for name in files if name.startswith('$PLUGINSDIR/')}
    require(len(plugins) <= MAX_PLUGIN_FILES, 'NSIS plugin count budget exceeded')
    for name in plugins:
        require(re.fullmatch(r'\$PLUGINSDIR/[A-Za-z0-9_.-]+\.(?:dll|bmp)', name),
                'Unexpected NSIS plugin/support file: ' + name)
        require(files[name] is not None and 0 < files[name] <= MAX_PLUGINS, 'NSIS plugin size budget exceeded')
    require(sum(files[name] for name in plugins) <= MAX_PLUGINS, 'NSIS aggregate plugin budget exceeded')
    require(set(files) == set(expected) | plugins | {uninstall}, 'NSIS missing/extra payload files (variant/removal mismatch)')
    for name, pin in expected.items():
        require(files[name] == pin['size'], 'NSIS payload size mismatch: ' + name)
    require(files[uninstall] is None or 0 < files[uninstall] <= MAX_UNINSTALLER,
            'NSIS uninstaller size budget exceeded')
    limits = {name: size if size is not None else MAX_UNINSTALLER for name, size in files.items()}
    parents = {parent.as_posix() for name in files for parent in Path(name).parents if parent.as_posix() != '.'}
    require(directories <= parents, 'Unexpected NSIS directory')
    require(sum(limits.values()) < NSIS_LIMIT, 'NSIS expanded-size budget exceeded')
    return expected, files, limits, parents, uninstall


def extraction_files(directory, limits, parents):
    paths = {}
    pending = [directory]
    count, total = 0, 0
    while pending:
        for path in pending.pop().iterdir():
            math_models.checked(path)
            name = math_models.safe_name(path.relative_to(directory).as_posix())
            count += 1
            require(count <= MAX_ENTRIES, 'Extraction entry count budget exceeded')
            if path.is_dir():
                require(name in parents, 'Unexpected extracted directory: ' + name)
                pending.append(path)
            else:
                regular(path)
                require(name in limits, 'Unexpected extracted file: ' + name)
                size = path.stat().st_size
                require(size <= limits[name], 'Extracted member size budget exceeded: ' + name)
                total += size
                require(total < NSIS_LIMIT, 'Extraction disk budget exceeded')
                paths[name] = path
    return paths


def verify_installer(archive, package, records, *, extractor=None, work_root=None, root=ROOT):
    archive = regular(archive)
    require(0 < archive.stat().st_size < NSIS_LIMIT, 'Installer on-disk size budget exceeded')
    executable = shutil.which(str(extractor) if extractor is not None else '7z')
    require(executable is not None, '7z extractor unavailable; supply --extractor. ' + EXTRACTION_FAILURE)
    executable = regular(Path(executable))
    require(executable != archive, 'Extractor must not be the installer')
    target = math_models.checked(root / 'target')
    work = math_models.checked(work_root if work_root is not None else target / 'verify-release-variants')
    require(work.is_relative_to(target) and work != target, '--work-root must be a dedicated directory under target/')
    package = math_models.checked(package)
    require(not work.is_relative_to(package) and not package.is_relative_to(work)
            and not archive.is_relative_to(work), 'Installer workspace must not overlap inputs')
    icon = file_record(root / INSTALLER_ICON)
    require(0 < icon['size'] <= MAX_PLUGINS, 'Installer icon size budget exceeded')
    before = file_record(archive)
    work.mkdir(parents=True, exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix='nsis-', dir=work) as temporary:
            stage = Path(temporary)
            listing = run_extractor([str(executable), 'l', '-slt', '-sccUTF-8', '-tNsis', '--', str(archive)], cwd=stage)
            expected, files, limits, parents, uninstall = parse_nsis_listing(listing, records, icon)
            output = stage / 'payload'
            output.mkdir()
            run_extractor([str(executable), 'x', '-y', '-bd', '-bb0', '-sccUTF-8', '-tNsis',
                           '-o' + str(output), '--', str(archive)], cwd=stage,
                          monitor=lambda: extraction_files(output, limits, parents))
            extracted = extraction_files(output, limits, parents)
            require(extracted.keys() == files.keys(), '7z did not extract the complete NSIS file set')
            for name, path in extracted.items():
                actual = file_record(path)
                if name in expected:
                    require(actual == expected[name], 'Installer payload bytes/hash mismatch: ' + name)
                elif files[name] is not None:
                    require(actual['size'] == files[name], 'Extracted NSIS support file size mismatch: ' + name)
            # NSIS generates a 32-bit uninstaller; do not use the x64 payload PE check.
            with extracted[uninstall].open('rb') as stream:
                dos = stream.read(64)
                require(len(dos) == 64 and dos[:2] == b'MZ', 'Missing/invalid extracted uninstaller PE')
                offset = struct.unpack_from('<I', dos, 0x3c)[0]
                require(64 <= offset <= MAX_UNINSTALLER - 4, 'Invalid uninstaller PE offset')
                stream.seek(offset)
                require(stream.read(4) == b'PE\0\0', 'Invalid uninstaller PE signature')
            require(file_record(archive) == before, 'Installer changed during verification')
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        raise ValueError(str(error) + '. ' + EXTRACTION_FAILURE) from error


def verify(command, package, variant, version, *, archive=None, root=ROOT, extractor=None, work_root=None):
    require(command in ('package', 'archive', 'installer'), 'Unknown command')
    require((archive is not None) == (command != 'package'), '--archive is required only for archive/installer modes')
    require(command == 'installer' or (extractor is None and work_root is None),
            '--extractor and --work-root are only valid for installer mode')
    records, total = verify_package(package, variant, version, root=root, require_manifest=command != 'package')
    if command == 'archive':
        verify_zip(archive, records, variant)
    elif command == 'installer':
        verify_installer(archive, package, records, extractor=extractor, work_root=work_root, root=root)
    return {'status': 'integrity-verified', 'mode': command, 'variant': variant, 'version': version,
            'payload_bytes_including_inventory': total, 'nsis_payload_limit_exclusive': NSIS_PAYLOAD_LIMIT,
            'main_build_bytes_verified': True, 'final_binary_identity_verified': False,
            'release_clearance': False, 'installer_executed': False,
            'limitations': 'No source/build attestation, final static-library identity, legal approval, '
                           'GUI/model execution, complete dependency closure or clean-install acceptance. '
                           'Installer mode checks extracted bytes, not installation control flow; '
                           'trusted 7z required, extraction supervision is not an OS sandbox.'}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('package', 'archive', 'installer'))
    parser.add_argument('--package', required=True, type=Path)
    parser.add_argument('--variant', required=True, choices=sorted(math_models.DIRECTORIES))
    parser.add_argument('--version', required=True)
    parser.add_argument('--archive', type=Path)
    parser.add_argument('--extractor', help='Trusted 7z executable; default: shutil.which("7z")')
    parser.add_argument('--work-root', type=Path, help='Installer-only workspace under target/; default: target/verify-release-variants')
    args = parser.parse_args(argv)
    try:
        options = {'extractor': args.extractor, 'work_root': args.work_root} if args.extractor or args.work_root else {}
        report = verify(args.command, args.package, args.variant, args.version, archive=args.archive, **options)
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError,
            NotImplementedError, struct.error, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        print('Release variant verification BLOCKED: ' + str(error), file=sys.stderr)
        return 1
    print(json.dumps(report, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
