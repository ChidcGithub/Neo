"""Inert payload tests; optional tiny NSIS compilation, never installation/network."""
from contextlib import ExitStack, chdir, redirect_stderr, redirect_stdout
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

if __package__:
    from . import verify_release_variants as v
else:
    import verify_release_variants as v


def pe_fixture(label=b'neo'):
    data = bytearray(2048)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 0x80)
    data[0x80:0x84] = b'PE\0\0'
    struct.pack_into('<HHIIIHH', data, 0x84, 0x8664, 1, 0, 0, 0, 240, 2)
    opt = 0x98
    struct.pack_into('<H', data, opt, 0x20b)
    struct.pack_into('<Q', data, opt + 24, 0x140000000)
    struct.pack_into('<I', data, opt + 108, 16)
    struct.pack_into('<II', data, opt + 120, 0x1000, 40)
    struct.pack_into('<IIII', data, opt + 240 + 8, 1024, 0x1000, 1024, 512)
    struct.pack_into('<IIIII', data, 512, 0, 0, 0, 0x1100, 0)
    data[768:781] = b'DirectML.dll\0'
    data[1500:1500 + len(label)] = label
    return bytes(data)


def record(data, key='size'):
    return {key: len(data), 'sha256': hashlib.sha256(data).hexdigest()}


class VariantTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.dict(os.environ, {'CARGO_TARGET_DIR': ''}))
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / 'dist/neo-int8'
        self.archive = self.root / 'portable.zip'
        self.variant = 'int8'
        self.version = '0.1.0'
        self.put('neo.exe', pe_fixture())
        build = self.root / v.MAIN_BUILD
        build.parent.mkdir(parents=True)
        build.write_bytes(pe_fixture())
        self.source_lock = {'sources': [
            {'kind': kind, 'sha256': 'a' * 64, 'size': 9,
             'expected_runtime_files_sha256': 'b' * 64} for kind in ('gitbash', 'native')]}
        self.put_json('docs/licenses/SOURCE-ACCESS.json',
                      v.sources.source_access(self.source_lock, 'owner/Neo', 'v0.1.0', '0.1.0'))
        self.lock = {'repository': 'https://example.invalid/drawing', 'commit': 'c' * 40,
                     'source_root_files': ['Cargo.toml', 'Cargo.lock', 'LICENSE'], 'workspace_packages': []}
        legal = b'Original license body\r\n'
        source_files = {'Cargo.toml': b'[workspace]\n', 'Cargo.lock': b'lock', 'LICENSE': legal}
        binding = {'source_commit': self.lock['commit'], 'source_tree_sha256': 'd' * 64,
                   'cargo_lock_sha256': record(source_files['Cargo.lock'])['sha256'],
                   'version': '0.0.1', 'legal_files': {'LICENSE': record(legal)['sha256']}}
        self.lock['source_binding'] = binding
        lock_path = self.root / 'tools/drawing-runtime.lock.json'
        lock_path.parent.mkdir(parents=True)
        lock_path.write_text(json.dumps(self.lock), encoding='utf-8')
        self.put(v.DRAWING_LEGAL + 'LICENSE', legal)
        files = {}
        for name, body in source_files.items():
            self.put(v.DRAWING_SOURCE + name, body)
            files[name] = record(body, 'bytes')
        artifacts = {}
        for name in v.drawing.ARTIFACTS:
            body = pe_fixture(name.encode())
            artifacts[name] = {**record(body, 'bytes'), 'architecture': 'x64', 'imports': ['directml.dll']}
            for kind in ('drawing', 'blackboard'):
                if name in ('neo-' + kind + '.exe', 'DirectML.dll'):
                    self.put(f'apps/{kind}/{name}', body)
        for kind in ('drawing', 'blackboard'):
            self.put(f'apps/{kind}/MSVC-PREREQUISITE.txt', b'official Microsoft Visual C++ x64 prerequisite')
        self.source = {**binding, 'repository': self.lock['repository'],
                       'lock_sha256': v.file_record(lock_path)['sha256'], 'files': files,
                       'source_files_sha256': v.tree_digest(files), 'artifacts': artifacts,
                       'directml_provenance': {'sha256': artifacts['DirectML.dll']['sha256']},
                       'crt_policy': 'external-official-msvc-x64-prerequisite'}
        self.put_json(v.DRAWING_LEGAL + 'SOURCE.json', self.source)
        self.child_map = {p.relative_to(self.package).as_posix(): record(p.read_bytes(), 'bytes')
                          for p in self.package.rglob('*') if p.is_file()
                          and p.relative_to(self.package).as_posix().startswith(
                              ('apps/', v.DRAWING_LEGAL, v.DRAWING_SOURCE))}
        self.put_json(v.DRAWING_FILES, self.child_map)
        self.model_lock = {}
        self.install_models('int8')
        stack = self.enterContext(ExitStack())
        self.mocks = {}
        for name, value in (('check_release.validate_payload', None), ('sensevoice.verify', {}),
                            ('ort.verify', {}), ('sources.read_lock', self.source_lock),
                            ('drawing.load_lock', self.lock), ('math_models.load_lock', self.model_lock)):
            module, attribute = name.split('.')
            self.mocks[name] = stack.enter_context(patch.object(getattr(v, module), attribute, return_value=value))
        self.tree_check = stack.enter_context(patch.object(v.math_models, 'verify_tree', wraps=v.math_models.verify_tree))
        # Any accidental side-effectful delegate is a regression, not a test stub.
        for target in ('subprocess.run', 'subprocess.check_output', 'urllib.request.urlopen',
                       'urllib.request.OpenerDirector.open', 'tools.prepare_math_models.download'):
            # Direct-file unittest execution uses unqualified module imports.
            if target.startswith('tools.'):
                stack.enter_context(patch.object(v.math_models, 'download', side_effect=AssertionError('network forbidden')))
            else:
                stack.enter_context(patch(target, side_effect=AssertionError('execution/network forbidden')))

    def put(self, name, body):
        path = self.package / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)
        return path

    def put_json(self, name, value):
        return self.put(name, (json.dumps(value, indent=2) + '\n').encode())

    def install_models(self, variant):
        directory = v.math_models.DIRECTORIES[variant]
        files = {}
        for name in sorted(v.math_models.MODEL_FILES | v.math_models.LEGAL_FILES):
            body = (variant + ':' + name).encode()
            self.put('apps/blackboard/models/' + directory + '/' + name, body)
            files[name] = record(body)
        if variant == 'int8':
            for name in sorted(v.math_models.INT8_FILES - files.keys() - {'FILES.sha256.json'}):
                body = name.encode()
                self.put('apps/blackboard/models/' + directory + '/' + name, body)
                files[name] = record(body)
            manifest = {directory + '/' + name: {'bytes': pin['size'], 'sha256': pin['sha256']}
                        for name, pin in files.items()}
            path = self.put_json('apps/blackboard/models/' + directory + '/FILES.sha256.json', manifest)
            files['FILES.sha256.json'] = record(path.read_bytes())
        self.model_lock[variant] = {'files': files}

    def verify(self, mode='package'):
        return v.verify(mode, self.package, self.variant, self.version, root=self.root,
                        archive=self.archive if mode == 'archive' else None)

    def inventory(self):
        return v.sources.inventory(self.package, self.variant, self.version)

    def zip_entries(self):
        return [('neo-' + self.variant + '/' + p.relative_to(self.package).as_posix(), p.read_bytes())
                for p in sorted(self.package.rglob('*')) if p.is_file()]

    def make_zip(self, entries=None):
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(self.archive, 'w', compression=zipfile.ZIP_DEFLATED) as zipped:
                for name, data in self.zip_entries() if entries is None else entries:
                    zipped.writestr(name, data)

    def snapshot(self):
        return {p.relative_to(self.root).as_posix(): (p.read_bytes(), p.stat().st_mtime_ns)
                for p in self.root.rglob('*') if p.is_file()}

    def test_full_package_wiring_and_no_writes_or_approval(self):
        before = self.snapshot()
        report = self.verify()
        self.assertEqual(self.snapshot(), before)
        self.assertFalse((self.package / v.MANIFEST).exists())
        self.assertEqual(report['status'], 'integrity-verified')
        self.assertFalse(report['release_clearance'])
        self.assertFalse(report['final_binary_identity_verified'])
        self.mocks['check_release.validate_payload'].assert_called_once_with(self.package)
        self.mocks['sensevoice.verify'].assert_called_once_with(self.root, self.package)
        self.mocks['sources.read_lock'].assert_called_once_with(self.root / 'tools/source-companions.lock.json')
        self.mocks['ort.verify'].assert_called_once_with(self.source_lock, root=self.root, package=self.package, required=True)
        self.mocks['math_models.load_lock'].assert_called_once_with(self.root / 'tools/math-models.lock.json')
        self.tree_check.assert_called_once_with(self.package / 'apps/blackboard/models/texteller-int8',
                                               self.model_lock['int8']['files'], 'int8')

    def test_fp32_and_archive_exact_identity_read_only(self):
        for path in (self.package / 'apps/blackboard/models/texteller-int8').iterdir():
            path.unlink()
        (self.package / 'apps/blackboard/models/texteller-int8').rmdir()
        self.variant = 'fp32'
        self.install_models('fp32')
        self.verify()
        self.inventory()
        self.make_zip()
        before = self.snapshot()
        self.assertEqual(self.verify('archive')['mode'], 'archive')
        self.assertEqual(self.snapshot(), before)

    def test_delegated_validator_failures_are_not_ignored(self):
        for name in ('check_release.validate_payload', 'sensevoice.verify', 'ort.verify', 'math_models.load_lock'):
            with self.subTest(name=name):
                self.mocks[name].side_effect = ValueError('delegate failure')
                with self.assertRaisesRegex(ValueError, 'delegate failure'):
                    self.verify()
                self.mocks[name].side_effect = None

    def test_missing_main_build_missing_resources_and_invalid_pe(self):
        build = self.root / v.MAIN_BUILD
        build.unlink()
        with self.assertRaises(OSError):
            self.verify()
        build.write_bytes(pe_fixture(b'other'))
        with self.assertRaisesRegex(ValueError, 'current release build'):
            self.verify()
        self.put('neo.exe', b'not a PE')
        with self.assertRaisesRegex(ValueError, 'Truncated PE'):
            self.verify()
        self.put('neo.exe', pe_fixture())
        build.write_bytes(pe_fixture())
        self.mocks['check_release.validate_payload'].side_effect = ValueError('missing required runtime resource')
        with self.assertRaisesRegex(ValueError, 'required runtime resource'):
            self.verify()

    def test_main_build_unset_and_empty_use_default(self):
        for value in (None, ''):
            with self.subTest(value=value), patch.dict(os.environ):
                if value is None:
                    os.environ.pop('CARGO_TARGET_DIR', None)
                else:
                    os.environ['CARGO_TARGET_DIR'] = value
                with chdir(self.package):
                    self.assertEqual(v.main_build(self.root), self.root / v.MAIN_BUILD)
                    self.verify()

    def test_main_build_relative_and_external_absolute_ignore_cwd(self):
        outside = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.inventory()
        self.make_zip()
        for target in ('target/ci-rust', str(outside / 'cargo-target')):
            with self.subTest(target=target), patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                build = self.root / target / 'x86_64-pc-windows-msvc/release/neo.exe'
                build.parent.mkdir(parents=True)
                build.write_bytes(pe_fixture())
                (self.root / v.MAIN_BUILD).write_bytes(pe_fixture(b'stale'))
                with chdir(self.package):
                    self.assertEqual(v.main_build(self.root), build)
                    self.assertEqual(self.verify()['status'], 'integrity-verified')
                    self.verify('archive')
                    # A matching old-path build must not hide a mismatched override.
                    (self.root / v.MAIN_BUILD).write_bytes(pe_fixture())
                    build.write_bytes(pe_fixture(b'other'))
                    for mode in ('package', 'archive'):
                        with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, 'current release build'):
                            self.verify(mode)

    def test_main_build_missing_override_never_falls_back(self):
        outside = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.inventory()
        self.make_zip()
        for target in ('target/ci-rust', str(outside / 'missing')):
            with self.subTest(target=target), patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                self.assertTrue((self.root / v.MAIN_BUILD).is_file())
                with self.assertRaises(FileNotFoundError):
                    v.main_build(self.root)
                for mode in ('package', 'archive'):
                    with self.subTest(mode=mode), self.assertRaises(FileNotFoundError):
                        self.verify(mode)

    def test_main_build_directory_is_not_regular(self):
        build = self.root / 'target/ci-rust/x86_64-pc-windows-msvc/release/neo.exe'
        build.mkdir(parents=True)
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': 'target/ci-rust'}):
            with self.assertRaisesRegex(ValueError, 'Expected regular file:'):
                v.main_build(self.root)
            with self.assertRaisesRegex(ValueError, 'Expected regular file:'):
                self.verify()

    def test_cargo_hardlinked_build_input_matches_independent_package(self):
        self.inventory()
        self.make_zip()
        for target in ('target', 'target/ci-rust'):
            with self.subTest(target=target), patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                build = self.root / target / 'x86_64-pc-windows-msvc/release/neo.exe'
                build.parent.mkdir(parents=True, exist_ok=True)
                build.write_bytes(pe_fixture())
                dependency = build.parent / 'deps/neo-hashed.exe'
                dependency.parent.mkdir()
                try:
                    os.link(build, dependency)
                except OSError as error:
                    self.skipTest('Hardlinks unavailable: ' + str(error))
                self.assertGreater(build.stat().st_nlink, 1)
                self.assertEqual((self.package / 'neo.exe').stat().st_nlink, 1)
                self.assertEqual(v.main_build(self.root), build)
                self.verify()
                self.verify('archive')
                dependency.write_bytes(pe_fixture(b'changed through Cargo alias'))
                with self.assertRaisesRegex(ValueError, 'current release build'):
                    self.verify()
                dependency.unlink()

    def test_main_build_symlink_escape_is_rejected(self):
        outside = Path(self.enterContext(tempfile.TemporaryDirectory()))
        external_build = outside / 'x86_64-pc-windows-msvc/release/neo.exe'
        external_build.parent.mkdir(parents=True)
        external_build.write_bytes(pe_fixture())
        target_link = self.root / 'target/linked'
        build_link = self.root / 'target/ci-rust/x86_64-pc-windows-msvc/release/neo.exe'
        build_link.parent.mkdir(parents=True)
        try:
            target_link.symlink_to(outside, target_is_directory=True)
            build_link.symlink_to(external_build)
        except OSError as error:
            self.skipTest('Symbolic links unavailable: ' + str(error))
        for target in ('target/linked', str(target_link), 'target/ci-rust'):
            with self.subTest(target=target), patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                with self.assertRaisesRegex(ValueError, 'Link/reparse point or special file forbidden:'):
                    v.main_build(self.root)
                with self.assertRaisesRegex(ValueError, 'Link/reparse point or special file forbidden:'):
                    self.verify()

    def test_main_build_reparse_rejected_without_symlink_privilege(self):
        from types import SimpleNamespace

        target = self.root / 'target/ci-rust'
        original_lstat = Path.lstat

        def lstat(path, *args, **kwargs):
            if path == target:
                return SimpleNamespace(st_mode=stat.S_IFDIR, st_file_attributes=0x400)
            return original_lstat(path, *args, **kwargs)

        for value in ('target/ci-rust', str(target)):
            with self.subTest(target=value), patch.dict(os.environ, {'CARGO_TARGET_DIR': value}), \
                    patch.object(Path, 'lstat', lstat):
                with self.assertRaisesRegex(ValueError, 'Link/reparse point or special file forbidden:'):
                    v.main_build(self.root)
                with self.assertRaisesRegex(ValueError, 'Link/reparse point or special file forbidden:'):
                    self.verify()

    def test_drawing_swap_missing_corrupt_and_source_inventory(self):
        paths = ['apps/drawing/neo-drawing.exe', 'apps/blackboard/DirectML.dll',
                 'apps/blackboard/MSVC-PREREQUISITE.txt', v.DRAWING_LEGAL + 'LICENSE',
                 v.DRAWING_SOURCE + 'Cargo.toml']
        for name in paths:
            path = self.package / name
            original = path.read_bytes()
            for change in ('missing', 'corrupt'):
                with self.subTest(name=name, change=change):
                    if change == 'missing':
                        path.unlink()
                    else:
                        path.write_bytes(b'x' * len(original))
                    with self.assertRaises((ValueError, OSError)):
                        self.verify()
                    path.write_bytes(original)
        drawing = self.package / 'apps/drawing/neo-drawing.exe'
        blackboard = self.package / 'apps/blackboard/neo-blackboard.exe'
        a, b = drawing.read_bytes(), blackboard.read_bytes()
        drawing.write_bytes(b)
        blackboard.write_bytes(a)
        with self.assertRaisesRegex(ValueError, 'artifact bytes/hash'):
            self.verify()

    def test_child_map_checks_every_entry_and_rejects_extra_missing_unsafe(self):
        for name in self.child_map:
            with self.subTest(name=name):
                wrong = copy.deepcopy(self.child_map)
                wrong[name]['sha256'] = '0' * 64
                self.put_json(v.DRAWING_FILES, wrong)
                with self.assertRaisesRegex(ValueError, 'FILES bytes/hash'):
                    self.verify()
        for name in ('../escape', 'apps/blackboard/models/texteller-int8/config.json'):
            wrong = {**self.child_map, name: record(b'x', 'bytes')}
            self.put_json(v.DRAWING_FILES, wrong)
            with self.assertRaisesRegex(ValueError, 'FILES map set'):
                self.verify()
        wrong = dict(self.child_map)
        wrong.pop(next(iter(wrong)))
        self.put_json(v.DRAWING_FILES, wrong)
        with self.assertRaisesRegex(ValueError, 'FILES map set'):
            self.verify()

    def test_drawing_extra_payload_rejected(self):
        self.put('apps/drawing/extra.dll', b'uninventoried')
        with self.assertRaisesRegex(ValueError, 'child file set'):
            self.verify()

    def test_math_corrupt_missing_extra_and_opposite_sibling(self):
        folder = self.package / 'apps/blackboard/models/texteller-int8'
        path = folder / 'encoder_model.onnx'
        body = path.read_bytes()
        path.write_bytes(b'x' * len(body))
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.verify()
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'file set'):
            self.verify()
        path.write_bytes(body)
        extra = folder / 'extra'
        extra.write_bytes(b'x')
        with self.assertRaisesRegex(ValueError, 'file set'):
            self.verify()
        extra.unlink()
        opposite = folder.parent / 'texteller'
        opposite.mkdir()
        with self.assertRaisesRegex(ValueError, 'opposite sibling'):
            self.verify()
        opposite.rmdir()
        self.put('apps/drawing/models/texteller-int8/config.json', b'x')
        with self.assertRaises(ValueError):
            self.verify()

    def test_math_variant_swap_and_forged_internal_map(self):
        folder = self.package / 'apps/blackboard/models/texteller-int8'
        path = folder / 'encoder_model.onnx'
        original = path.read_bytes()
        path.write_bytes(b'fp32:' + original[5:])
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            self.verify()
        path.write_bytes(original)
        internal = folder / 'FILES.sha256.json'
        manifest = json.loads(internal.read_text())
        manifest['texteller-int8/config.json']['sha256'] = '0' * 64
        internal.write_text(json.dumps(manifest), encoding='utf-8')
        # Even a lock pin matching the changed inventory cannot bless a map
        # that contradicts the individually verified model pins.
        self.model_lock['int8']['files']['FILES.sha256.json'] = record(internal.read_bytes())
        with self.assertRaisesRegex(ValueError, 'differs from lock'):
            self.verify()

    def test_pe_structure_and_receipt_fields_fail_closed(self):
        path = self.package / 'neo.exe'
        original = path.read_bytes()
        wrong = bytearray(original)
        struct.pack_into('<H', wrong, 0x84, 0x14c)
        path.write_bytes(wrong)
        with self.assertRaisesRegex(ValueError, 'x64'):
            self.verify()
        path.write_bytes(original)
        source = copy.deepcopy(self.source)
        source['artifacts']['neo-drawing.exe']['imports'] = []
        self.put_json(v.DRAWING_LEGAL + 'SOURCE.json', source)
        with self.assertRaisesRegex(ValueError, 'PE receipt mismatch'):
            self.verify()

    def test_source_access_version_tag_url_hash_mismatch(self):
        path = self.package / 'docs/licenses/SOURCE-ACCESS.json'
        original = json.loads(path.read_text())
        for field in ('tag', 'name', 'url', 'sha256'):
            notice = copy.deepcopy(original)
            if field == 'tag':
                notice[field] = 'v0.2.0'
            else:
                notice['sources'][0][field] = 'wrong'
            self.put_json('docs/licenses/SOURCE-ACCESS.json', notice)
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'SOURCE-ACCESS'):
                self.verify()

    def test_stale_inventory_wrong_variant_version_and_tree(self):
        manifest = self.inventory()
        for key in ('version', 'variant', 'files'):
            wrong = copy.deepcopy(manifest)
            wrong[key] = {} if key == 'files' else 'wrong'
            self.put_json(v.MANIFEST, wrong)
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'PACKAGE-MANIFEST'):
                self.verify()
        self.put_json(v.MANIFEST, manifest)
        self.put('new.txt', b'x')
        with self.assertRaisesRegex(ValueError, 'PACKAGE-MANIFEST'):
            self.verify()

    def test_archive_requires_manifest(self):
        self.make_zip()
        with self.assertRaisesRegex(ValueError, 'run inventory first'):
            self.verify('archive')

    def test_zip_exact_top_root_files_and_manifest_content(self):
        self.inventory()
        self.make_zip()
        self.verify('archive')
        entries = self.zip_entries()
        for change in ('root', 'missing', 'extra', 'corrupt', 'manifest', 'size'):
            modified = list(entries)
            if change == 'root':
                modified = [(n.replace('neo-int8/', 'neo-fp32/', 1), b) for n, b in entries]
            elif change == 'missing':
                modified.pop()
            elif change == 'extra':
                modified.append(('neo-int8/unexpected', b'x'))
            else:
                index = next(i for i, (name, _) in enumerate(entries)
                             if name.endswith(v.MANIFEST if change == 'manifest' else 'neo.exe'))
                name, body = modified[index]
                modified[index] = (name, b'x' * (len(body) + (change == 'size')))
            self.make_zip(modified)
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.verify('archive')

    def test_zip_unsafe_duplicate_alias_link_directory_and_encryption(self):
        self.inventory()
        records, _ = v.verify_package(self.package, self.variant, self.version, root=self.root)
        entries = self.zip_entries()
        for name in ('../escape', '/absolute', 'neo-int8/../escape', 'neo-int8\\evil',
                     'neo-int8/C:evil', 'neo-int8/NUL', 'neo-int8/a.', 'neo-int8//a',
                     entries[0][0], entries[0][0].upper()):
            self.make_zip(entries + [(name, b'x')])
            with self.subTest(name=name), self.assertRaises(ValueError):
                v.verify_zip(self.archive, records, self.variant)
        for mode in (stat.S_IFLNK, stat.S_IFIFO, stat.S_IFDIR):
            info = zipfile.ZipInfo(entries[0][0])
            info.create_system = 3
            info.external_attr = (mode | 0o644) << 16
            self.make_zip([(info, entries[0][1]), *entries[1:]])
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                v.verify_zip(self.archive, records, self.variant)
        self.make_zip(entries)
        raw = bytearray(self.archive.read_bytes())
        for signature, offset in ((b'PK\x03\x04', 6), (b'PK\x01\x02', 8)):
            start = raw.index(signature)
            flags = struct.unpack_from('<H', raw, start + offset)[0]
            struct.pack_into('<H', raw, start + offset, flags | 1)
        self.archive.write_bytes(raw)
        with self.assertRaisesRegex(ValueError, 'Encrypted'):
            v.verify_zip(self.archive, records, self.variant)

    def test_zip_nul_alias_inflated_headers_and_member_budget(self):
        self.inventory()
        records, _ = v.verify_package(self.package, self.variant, self.version, root=self.root)
        self.make_zip()
        original = self.archive.read_bytes()
        raw = bytearray(original)
        central = raw.index(b'PK\x01\x02')
        struct.pack_into('<I', raw, central + 24, v.NSIS_PAYLOAD_LIMIT)
        self.archive.write_bytes(raw)
        with self.assertRaisesRegex(ValueError, 'NSIS'):
            v.verify_zip(self.archive, records, self.variant)
        raw = bytearray(original)
        # Keep the central name length intact, but inject a NUL alias that
        # ZipInfo.filename truncates while orig_filename preserves it.
        raw[central + 46 + 3] = 0
        self.archive.write_bytes(raw)
        with self.assertRaisesRegex(ValueError, 'unsafe ZIP member alias'):
            v.verify_zip(self.archive, records, self.variant)
        self.archive.write_bytes(original)
        with patch.object(v, 'MAX_ENTRIES', 1):
            with self.assertRaisesRegex(ValueError, 'member count budget'):
                v.verify_zip(self.archive, records, self.variant)

    def test_zip_directories_optional_but_only_expected_parents(self):
        self.inventory()
        entries = self.zip_entries()
        self.make_zip([('neo-int8/', b''), ('neo-int8/apps/', b''), *entries])
        self.verify('archive')
        self.make_zip([('neo-int8/not-a-parent/', b''), *entries])
        with self.assertRaisesRegex(ValueError, 'Unexpected ZIP directory'):
            self.verify('archive')

    def test_budget_strict_boundary_pre_hash_and_inventory_allowance(self):
        self.assertLess(v.NSIS_PAYLOAD_LIMIT, 2 * 1024**3)
        v.budget(v.NSIS_PAYLOAD_LIMIT - 1)
        with self.assertRaisesRegex(ValueError, 'do not remove features/models automatically'):
            v.budget(v.NSIS_PAYLOAD_LIMIT)
        size = sum(p.stat().st_size for p in self.package.rglob('*') if p.is_file())
        with patch.object(v, 'NSIS_PAYLOAD_LIMIT', size), patch.object(v, 'file_record') as hashed:
            with self.assertRaisesRegex(ValueError, 'NSIS'):
                self.verify()
            hashed.assert_not_called()
        with patch.object(v, 'NSIS_PAYLOAD_LIMIT', size + 1):
            with self.assertRaisesRegex(ValueError, 'NSIS'):
                self.verify()
        self.inventory()
        self.make_zip()
        records = {name: record(body) for name, body in
                   ((p.relative_to(self.package).as_posix(), p.read_bytes())
                    for p in self.package.rglob('*') if p.is_file())}
        with patch.object(v, 'NSIS_PAYLOAD_LIMIT', 1):
            with self.assertRaisesRegex(ValueError, 'NSIS'):
                v.verify_zip(self.archive, records, self.variant)
        with patch.object(v, 'NSIS_LIMIT', 1):
            with self.assertRaisesRegex(ValueError, 'on-disk size'):
                v.verify_zip(self.archive, records, self.variant)

    def test_links_rejected_without_writes(self):
        source = self.package / 'neo.exe'
        link = self.package / 'hardlink.exe'
        try:
            os.link(source, link)
        except OSError as error:
            self.skipTest('Hardlinks unavailable: ' + str(error))
        with self.assertRaisesRegex(ValueError, 'unlinked file'):
            self.verify()
        link.unlink()
        try:
            link.symlink_to(source)
        except OSError:
            return  # Windows may not grant symbolic link privileges.
        with self.assertRaisesRegex(ValueError, 'Symlink/reparse input or output is forbidden'):
            self.verify()

    def test_duplicate_json_keys_rejected(self):
        self.put(v.DRAWING_FILES, b'{"duplicate": 1, "duplicate": 2}')
        with self.assertRaisesRegex(ValueError, 'Duplicate JSON key'):
            self.verify()

    def test_bounded_stream_hashing(self):
        class Bounded(io.BytesIO):
            def read(self, size=-1):
                if size < 0 or size > v.CHUNK:
                    raise AssertionError('unbounded read')
                return super().read(size)
        data = b'x' * (v.CHUNK + 17)
        self.assertEqual(v.stream_record(Bounded(data), len(data)), record(data))
        with self.assertRaisesRegex(ValueError, 'expanded-size'):
            v.stream_record(Bounded(data), len(data) - 1)


def nsis_listing(entries):
    return '7-Zip fixture\n--\nType = Nsis\n\n----------\n' + '\n\n'.join(
        '\n'.join(key + ' = ' + str(value) for key, value in entry.items()) for entry in entries) + '\n'


class InstallerTests(unittest.TestCase):
    def setUp(self):
        temporary = self.enterContext(tempfile.TemporaryDirectory())
        self.root = Path(temporary)
        self.package = self.root / 'package'
        self.package.mkdir()
        self.records = {'neo.exe': record(b'int8'), v.MANIFEST: record(b'manifest-int8')}
        self.payload = {'$_23_/neo.exe': b'int8', '$_23_/' + v.MANIFEST: b'manifest-int8',
                        '$_23_/neo.ico': b'icon', '$_23_/uninstall.exe': pe_fixture(),
                        '$PLUGINSDIR/System.dll': b'plugin', '$PLUGINSDIR/modern-wizard.bmp': b'bitmap'}
        self.entries = [{'Path': name.replace('/', '\\'), 'Size': '' if name.endswith('/uninstall.exe') else len(data),
                         'Attributes': ''} for name, data in self.payload.items()]
        self.archive = self.root / 'installer.exe'
        self.archive.write_bytes(b'compiled NSIS stand-in, never run')
        self.extractor = self.root / '7z.exe'
        self.extractor.write_bytes(b'trusted extractor stand-in')
        self.work = self.root / 'target/work'
        art = self.root / v.INSTALLER_ICON
        art.parent.mkdir(parents=True)
        art.write_bytes(b'icon')
        self.commands = []
        self.enterContext(patch.object(v.shutil, 'which', return_value=str(self.extractor)))
        self.runner = self.enterContext(patch.object(v, 'run_extractor', side_effect=self.run_tool))

    def run_tool(self, command, *, cwd, monitor=None):
        self.commands.append(command)
        self.assertEqual(command[0], str(self.extractor))
        self.assertNotEqual(command[0], str(self.archive))
        if command[1] == 'l':
            self.assertIn('-slt', command)
            return nsis_listing(self.entries)
        self.assertEqual(command[1], 'x')
        output = Path(next(arg[2:] for arg in command if arg.startswith('-o')))
        for name, data in self.payload.items():
            path = output / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        monitor()
        return 'Everything is Ok'

    def verify(self):
        v.verify_installer(self.archive, self.package, self.records, root=self.root,
                           extractor=str(self.extractor), work_root=self.work)

    def test_complete_extraction_exact_hashes_and_cleanup(self):
        before = self.archive.read_bytes()
        self.verify()
        self.assertEqual([c[1] for c in self.commands], ['l', 'x'])
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual(self.archive.read_bytes(), before)

    def test_variant_swapped_content_and_icon_hash(self):
        for name in ('$_23_/neo.exe', '$_23_/' + v.MANIFEST, '$_23_/neo.ico'):
            original = self.payload[name]
            self.payload[name] = b'x' * len(original)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'bytes/hash mismatch'):
                self.verify()
            self.payload[name] = original
            self.assertEqual(list(self.work.iterdir()), [])

    def test_missing_payload_or_uninstaller_never_falls_back(self):
        for name in ('$_23_/neo.exe', '$_23_/uninstall.exe'):
            original = self.payload.pop(name)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'no weaker fallback passes'):
                self.verify()
            self.payload[name] = original
        self.entries = [entry for entry in self.entries if not entry['Path'].endswith('neo.exe')]
        self.commands.clear()
        with self.assertRaisesRegex(ValueError, 'missing/extra payload'):
            self.verify()
        self.assertEqual([c[1] for c in self.commands], ['l'])

    def test_unsafe_duplicate_link_unknown_root_and_extras_before_extract(self):
        original = copy.deepcopy(self.entries)
        for name in ('../evil', 'C:\\evil', '$_23_/../evil', '$_23_/NUL', '$_23_/x:stream',
                     '$_24_/new', '$_23_/extra.dll', '$PLUGINSDIR/script.exe', '$PLUGINSDIR/sub/evil.dll',
                     '$_23_/neo.exe', '$_23_/NEO.EXE'):
            self.entries = original + [{'Path': name, 'Size': '1', 'Attributes': ''}]
            self.commands.clear()
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.verify()
            self.assertEqual([c[1] for c in self.commands], ['l'])
        for key, value in (('Symbolic Link', 'elsewhere'), ('Hard Link', 'elsewhere'),
                           ('Attributes', 'lrwxrwxrwx'), ('Attributes', 'L'), ('Encrypted', '+')):
            self.entries = copy.deepcopy(original)
            self.entries[0][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify()

    def test_sizes_plugin_and_unknown_uninstaller_bounds(self):
        for key, value in (('Size', '-1'), ('Size', ''), ('Size', str(v.NSIS_LIMIT))):
            entries = copy.deepcopy(self.entries)
            self.entries[0][key] = value
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.verify()
            self.entries = entries
        with patch.object(v, 'MAX_PLUGIN_FILES', 1):
            with self.assertRaisesRegex(ValueError, 'plugin count'):
                self.verify()
        with patch.object(v, 'MAX_UNINSTALLER', 32):
            with self.assertRaisesRegex(ValueError, 'member size budget'):
                self.verify()
        with patch.object(v, 'NSIS_LIMIT', 32):
            with self.assertRaisesRegex(ValueError, 'size budget'):
                self.verify()
        with patch.object(v, 'MAX_ENTRIES', 1):
            with self.assertRaisesRegex(ValueError, 'member count'):
                self.verify()

    def test_extracted_links_and_extra_files_are_rejected(self):
        output = self.root / 'extraction'
        output.mkdir()
        (output / 'expected').write_bytes(b'x')
        os.link(output / 'expected', output / 'linked')
        with self.assertRaisesRegex(ValueError, 'unlinked file'):
            v.extraction_files(output, {'expected': 1, 'linked': 1}, set())
        (output / 'linked').unlink()
        (output / 'unexpected').write_bytes(b'x')
        with self.assertRaisesRegex(ValueError, 'Unexpected extracted file'):
            v.extraction_files(output, {'expected': 1}, set())

    def test_missing_tool_work_escape_and_nonzero_fail_closed(self):
        with patch.object(v.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'extractor unavailable'):
                self.verify()
        work = self.work
        self.work = self.root / 'outside'
        with self.assertRaisesRegex(ValueError, 'under target'):
            self.verify()
        self.work = work
        self.runner.side_effect = subprocess.CalledProcessError(2, ['7z'])
        with self.assertRaisesRegex(ValueError, 'no weaker fallback passes'):
            self.verify()
        self.assertEqual(list(self.work.iterdir()), [])

    def test_installer_full_validator_wiring(self):
        with patch.object(v, 'verify_package', return_value=(self.records, 123)) as package:
            report = v.verify('installer', self.package, 'fp32', '0.1.0', archive=self.archive,
                              root=self.root, extractor=str(self.extractor), work_root=self.work)
        package.assert_called_once_with(self.package, 'fp32', '0.1.0', root=self.root, require_manifest=True)
        self.assertFalse(report['final_binary_identity_verified'])
        self.assertEqual(report['mode'], 'installer')

    def test_bounded_runner_output_timeout_and_nonzero(self):
        # Run the real supervisor against an inert mocked process, never any EXE.
        class Process:
            def __init__(self, output=b'', code=0):
                self.stdout = io.BytesIO(output)
                self.returncode = code
                self.killed = False
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def poll(self):
                return self.returncode
            def kill(self):
                self.killed = True
                self.returncode = -9
            def wait(self, timeout):
                return self.returncode

        for process, message in ((Process(b'x' * 17), 'output budget'), (Process(code=2), 'nonzero'),
                                 (Process(code=None), 'time budget')):
            with patch.object(v.subprocess, 'Popen', return_value=process), patch.object(v, 'MAX_TOOL_OUTPUT', 16), \
                    patch.object(v, 'EXTRACT_SECONDS', -1):
                with self.assertRaisesRegex(ValueError, message):
                    REAL_RUN_EXTRACTOR(['trusted-7z', 'l'], cwd=self.root)
            if message == 'time budget':
                self.assertTrue(process.killed)


REAL_RUN_EXTRACTOR = v.run_extractor


def local_tool(name, candidates):
    override = os.environ.get('NEO_TEST_' + name.upper())
    if override:
        return shutil.which(override)
    return shutil.which(name) or next((str(p) for p in map(Path, candidates) if p.is_file()), None)


class RealNsisExtractionTests(unittest.TestCase):
    def test_tiny_solid_nsis_extracts_uninstaller_without_execution(self):
        compiler = local_tool('makensis', ['C:/Program Files (x86)/NSIS/makensis.exe'])
        extractor = local_tool('7z', ['C:/Program Files/7-Zip/7z.exe', 'D:/Program Files/7-Zip/7z.exe'])
        if not compiler or not extractor:
            self.skipTest('makensis and 7z required; dependencies are never installed by this test')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / 'package'
            (package / 'nested').mkdir(parents=True)
            (package / 'neo.exe').write_bytes(b'inert-not-executable')
            (package / 'nested/payload.txt').write_bytes(b'int8 model stand-in')
            v.sources.inventory(package, 'int8', '0.1.0')
            records = {p.relative_to(package).as_posix(): v.file_record(p)
                       for p in package.rglob('*') if p.is_file()}
            art = root / v.INSTALLER_ICON
            art.parent.mkdir(parents=True)
            art.write_bytes(b'icon bytes copied as a File, not an executable resource')
            script = root / 'tiny.nsi'
            script.write_text('''Unicode true
Name tiny
OutFile tiny.exe
SetCompressor /SOLID lzma
RequestExecutionLevel user
!include MUI2.nsh
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE English
Var StageDir
Section
StrCpy $StageDir $INSTDIR
SetOutPath $StageDir
File /r "package\\*.*"
File /oname=neo.ico "target\\package\\installer-art\\neo.ico"
WriteUninstaller "$StageDir\\uninstall.exe"
System::Call 'kernel32::GetCurrentProcessId() i.r0'
SectionEnd
Section Uninstall
Delete "$INSTDIR\\neo.exe"
SectionEnd
''', encoding='utf-8')
            flag = '/V2' if os.name == 'nt' else '-V2'
            compiled = subprocess.run([compiler, flag, str(script)], cwd=root, stdin=subprocess.DEVNULL,
                                      capture_output=True, timeout=60)
            self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
            archive = root / 'tiny.exe'
            listing = REAL_RUN_EXTRACTOR([extractor, 'l', '-slt', '-sccUTF-8', '-tNsis', '--', str(archive)], cwd=root)
            self.assertIn('uninstall.exe', listing)
            self.assertIn('$PLUGINSDIR', listing)
            work = root / 'target/check'
            v.verify_installer(archive, package, records, root=root, extractor=extractor, work_root=work)
            self.assertEqual(list(work.iterdir()), [])
            wrong = copy.deepcopy(records)
            wrong['nested/payload.txt']['sha256'] = '0' * 64
            with self.assertRaisesRegex(ValueError, 'bytes/hash mismatch'):
                v.verify_installer(archive, package, wrong, root=root, extractor=extractor, work_root=work)
            self.assertEqual(list(work.iterdir()), [])


class CliTests(unittest.TestCase):
    def test_cli_exact_arguments_and_error_exit(self):
        for mode in ('package', 'archive'):
            argv = [mode, '--package', 'dist/neo-int8', '--variant', 'int8', '--version', '0.1.0']
            archive = None
            if mode == 'archive':
                archive = Path('dist/neo-0.1.0-int8-portable-x64.zip')
                argv += ['--archive', str(archive)]
            with patch.object(v, 'verify', return_value={'status': 'integrity-verified'}) as verify:
                with redirect_stdout(io.StringIO()):
                    self.assertEqual(v.main(argv), 0)
                verify.assert_called_once_with(mode, Path('dist/neo-int8'), 'int8', '0.1.0', archive=archive)
            with patch.object(v, 'verify', side_effect=ValueError('fixture failure')):
                output = io.StringIO()
                with redirect_stderr(output):
                    self.assertEqual(v.main(argv), 1)
                self.assertIn('BLOCKED: fixture failure', output.getvalue())

    def test_installer_cli(self):
        with patch.object(v, 'verify', return_value={'status': 'integrity-verified'}) as verify:
            with redirect_stdout(io.StringIO()):
                self.assertEqual(v.main(['installer', '--package', 'dist/neo-int8', '--variant', 'int8',
                                         '--version', '0.1.0', '--archive', 'installer.exe',
                                         '--extractor', '7z', '--work-root', 'target/verify-installer']), 0)
            verify.assert_called_once_with('installer', Path('dist/neo-int8'), 'int8', '0.1.0',
                                           archive=Path('installer.exe'), extractor='7z',
                                           work_root=Path('target/verify-installer'))

    def test_archive_option_mode_constraints(self):
        for mode, archive in (('archive', None), ('installer', None), ('package', Path('archive.zip'))):
            with self.assertRaisesRegex(ValueError, '--archive'):
                v.verify(mode, Path('unused'), 'int8', '0.1.0', archive=archive)


if __name__ == '__main__':
    unittest.main()
