
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import zipfile
import tempfile
import unittest
from unittest import mock

if __package__:
    from . import prepare_runtime_distribution as prep
    from .runtime_sources import file_record, snapshot
else:
    import prepare_runtime_distribution as prep
    from runtime_sources import file_record, snapshot

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / '.cache/runtime/gitbash'


def minimal_pe(import_name=b'shared.dll', delay=False):
    data = bytearray(1024)
    data[:2] = b'MZ'
    struct.pack_into('<I', data, 0x3c, 0x80)
    data[0x80:0x84] = b'PE\0\0'
    struct.pack_into('<H', data, 0x86, 1)
    struct.pack_into('<H', data, 0x94, 240)
    struct.pack_into('<H', data, 0x98, 0x20b)
    struct.pack_into('<Q', data, 0x98 + 24, 0x140000000)
    struct.pack_into('<I', data, 0x98 + 108, 16)
    index, width = (13, 32) if delay else (1, 20)
    struct.pack_into('<II', data, 0x98 + 112 + index * 8, 0x1000, width * 2)
    struct.pack_into('<IIII', data, 0x98 + 240 + 8, 512, 0x1000, 512, 512)
    if delay:
        struct.pack_into('<II', data, 512, 1, 0x1080)
    else:
        struct.pack_into('<I', data, 512 + 12, 0x1080)
    data[640:640 + len(import_name) + 1] = import_name + b'\0'
    return bytes(data)


class ConfigAndPolicyTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which('git'), 'Git required for checkout regression')
    def test_windows_checkout_preserves_companion_policy_bytes(self):
        name = 'tools/gitbash-distribution-policy.json'
        expected = 'd917b4be48541b64c0e099e87bd30a275732c3f80aca1f96301660b495934679'
        original = (ROOT / name).read_bytes()
        self.assertEqual(hashlib.sha256(original).hexdigest(), expected)
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)

            def git(*args):
                return subprocess.run(['git', '--no-pager', '-c', 'core.autocrlf=true', *args],
                                      cwd=root, capture_output=True, check=True, timeout=15)

            git('init', '-q')
            (root / 'tools').mkdir()
            (root / '.gitattributes').write_bytes((ROOT / '.gitattributes').read_bytes())
            (root / name).write_bytes(original)
            git('add', '.gitattributes', name)
            (root / name).unlink()
            git('checkout-index', '--force', '--', name)
            self.assertEqual((root / name).read_bytes(), original)
            self.assertEqual(hashlib.sha256((root / name).read_bytes()).hexdigest(), expected)

    def test_exact_multivalue_helpers_and_unrelated_bytes(self):
        raw = (b'# preserve\r\n[credential]\r\n helper = manager\r\n'
               b' helper = "manager-core" # remove\r\n helper = selector\r\n'
               b' helper = helper-selector\r\n helper =\r\n helper = wincred\r\n'
               b' helper = store --file=a\r\n helper = !echo manager\r\n'
               b' helper = manager-custom\r\n[core]\r\n helper = manager\r\n'
               b'[credential "https://example.invalid"]\r\n helper = manager\r\n'
               b' username = somebody\r\n[include]\r\n path = local.cfg\r\n'
               b' path = "C:/Program Files/Git/etc/gitconfig"\r\n'
               b' path = C:/Program Files (x86)/Git/etc/gitconfig\r\n'
               b'[includeIf "gitdir:work/"]\r\n path = team.cfg\r\n')
        result, removed = prep.sanitize_config(raw)
        self.assertEqual(len(removed), 7)
        for line in (b' helper =\r\n', b' helper = wincred\r\n', b' helper = !echo manager\r\n',
                     b' helper = store --file=a\r\n', b' helper = manager-custom\r\n',
                     b'[core]\r\n helper = manager\r\n', b' path = local.cfg\r\n',
                     b'[includeIf "gitdir:work/"]\r\n path = team.cfg\r\n'):
            self.assertIn(line, result)
        expected = raw.decode().splitlines(keepends=True)
        for change in reversed(removed):
            del expected[change['line'] - 1]
        self.assertEqual(result, ''.join(expected).encode())

    def test_fetch_metadata_exact_pair_only_and_not_in_output_manifest(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            policy = {'source_files': [{'path': 'input'}]}
            state = {'files': [{'path': 'input'}, *({'path': n} for n in prep.FETCH_METADATA)]}
            for name, value in prep.FETCH_METADATA.items():
                (root / name).write_bytes(value.encode())
            with mock.patch.object(prep, 'snapshot', return_value=(state, {})):
                self.assertEqual(prep.verify_snapshot(root, policy)['files'], policy['source_files'])
            for name in prep.FETCH_METADATA:
                (root / name).write_bytes(b'unknown')
                with mock.patch.object(prep, 'snapshot', return_value=({'files': [{'path': 'input'}, *({'path': n} for n in prep.FETCH_METADATA)]}, {})):
                    with self.assertRaisesRegex(ValueError, 'Fetch metadata'):
                        prep.verify_snapshot(root, policy)
                (root / name).write_bytes(prep.FETCH_METADATA[name].encode())
            with mock.patch.object(prep, 'snapshot', return_value=({'files': [{'path': 'input'}, {'path': '.neo-version'}]}, {})):
                with self.assertRaisesRegex(ValueError, 'incomplete Fetch metadata'):
                    prep.verify_snapshot(root, policy)

    def test_ambiguous_config_rejected(self):
        with self.assertRaises(ValueError):
            prep.sanitize_config(b'[credential]\n helper = manager\\\n -core\n')

    def test_policy_covers_exact_component_manifest_and_nativeinterop(self):
        policy = prep.load_policy(prep.POLICY)
        gcm = json.loads((ROOT / 'docs/licenses/runtime/gcm/manifest.json').read_text())
        removed = prep.records(policy['remove_files'])
        for item in gcm['files']:
            self.assertEqual(removed[item['path']], {k: item[k] for k in ('sha256', 'size')})
        self.assertNotIn('mingw64/libexec/git-core/git-credential-wincred.exe', removed)
        self.assertEqual(len(removed), 56)
        self.assertEqual(len(prep.records(policy['source_files'])), 366)

    def test_blocked_bytes_cannot_hide_under_another_filename(self):
        policy = prep.load_policy(prep.POLICY)
        for sha in policy['blocked_sha256']:
            with self.assertRaises(ValueError):
                prep.assert_absent([{'path': 'renamed.bin', 'sha256': sha}], policy)

    def test_normal_and_delay_imports(self):
        self.assertEqual(prep.pe_imports(minimal_pe()), ['shared.dll'])
        self.assertEqual(prep.pe_imports(minimal_pe(b'MSALRuntime.dll', delay=True)), ['msalruntime.dll'])
        with self.assertRaises(ValueError):
            prep.pe_imports(b'MZ')

    def test_dependency_on_removed_shared_dll_is_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / 'git.exe').write_bytes(minimal_pe())
            before = {'git.exe': {}, 'shared.dll': {}}
            with self.assertRaisesRegex(ValueError, 'removed local DLL'):
                prep.dependency_review(root, before, {'shared.dll'})

    def test_unsafe_and_case_colliding_policy_paths(self):
        row = {'path': '../bad', 'size': 0, 'sha256': '0' * 64}
        with self.assertRaises(ValueError):
            prep.records([row])
        with self.assertRaises(ValueError):
            prep.records([{**row, 'path': 'A'}, {**row, 'path': 'a'}])


@unittest.skipUnless(CACHE.is_dir(), 'pinned local runtime not available')
class LocalRuntimeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.policy = prep.load_policy(prep.POLICY)
        cls.original = prep.verify_snapshot(CACHE, cls.policy)
        cls.temp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.temp.name)
        cls.source = cls.root / 'source'
        shutil.copytree(CACHE, cls.source)

    @classmethod
    def tearDownClass(cls):
        try:
            assert snapshot(CACHE)[0] == cls.original
        finally:
            cls.temp.cleanup()

    def test_success_preserves_source_and_retained_shared_libraries(self):
        before = snapshot(self.source)[0]
        out = self.root / 'success'
        report = prep.prepare(self.source, out, self.root, run_smoke=False)
        self.assertEqual(snapshot(self.source)[0], before)
        self.assertEqual(report['counts'], {'original': 366, 'removed': 56, 'retained': 310,
                                           'modified': 2, 'source_packages': 57})
        actual = prep.records(snapshot(out / 'runtime/gitbash')[0]['files'])
        expected = prep.records(before['files'])
        for name, record in actual.items():
            if name not in self.policy['modify_files']:
                self.assertEqual(record, expected[name])
        self.assertIn('mingw64/libexec/git-core/git-credential-wincred.exe', actual)
        prep.assert_absent(snapshot(out / 'runtime/gitbash')[0]['files'], self.policy)
        self.assertFalse(report['dependency_review']['missing_removed_local_imports'])
        self.assertEqual(prep.verify_distribution(out), report)
        self.assertFalse((out / 'source-companion-record.json').exists())
        self.assertIn('corresponding source NOT supplied', (out / 'MODIFICATIONS.md').read_text())
        with self.assertRaises(ValueError):
            prep.prepare(self.source, out, self.root, run_smoke=False)

    def test_tamper_unknown_version_missing_and_added_file_rejected(self):
        for rel in ('cmd/git.exe', 'etc/package-versions.txt', 'new.dll'):
            with self.subTest(rel=rel):
                path = self.source / rel
                old = path.read_bytes() if path.exists() else None
                try:
                    path.write_bytes(old.replace(b'2.55.0.5', b'9.99.0.0')
                                     if rel == 'etc/package-versions.txt' else b'tampered input')
                    with self.assertRaisesRegex(ValueError, 'unknown/tampered runtime'):
                        prep.prepare(self.source, self.root / 'reject', self.root, run_smoke=False)
                    self.assertFalse((self.root / 'reject').exists())
                finally:
                    if old is None:
                        path.unlink()
                    else:
                        path.write_bytes(old)
        path = self.source / 'mingw64/bin/msalruntime.dll'
        old = path.read_bytes()
        try:
            path.unlink()
            with self.assertRaisesRegex(ValueError, 'unknown/tampered runtime'):
                prep.verify_snapshot(self.source, self.policy)
        finally:
            path.write_bytes(old)

    def test_output_scope_and_overlap_rejected(self):
        for out in (self.root, self.source, self.source / 'new', self.root.parent / 'escape',
                            self.root / '..' / 'escape'):
            with self.subTest(out=out), self.assertRaises(ValueError):
                prep.prepare(self.source, out, self.root, run_smoke=False)

    def test_failure_cleans_stage_and_never_exposes_partial_output(self):
        out = self.root / 'failure'
        with mock.patch.object(prep, 'dependency_review', side_effect=ValueError('injected')):
            with self.assertRaisesRegex(ValueError, 'injected'):
                prep.prepare(self.source, out, self.root, run_smoke=False)
        self.assertFalse(out.exists())
        self.assertFalse(list(self.root.glob('.failure*')))
        prep.verify_snapshot(self.source, self.policy)

    def test_symlink_input_and_output_parent_rejected(self):
        link = self.root / 'link'
        try:
            link.symlink_to(self.source, target_is_directory=True)
        except OSError:
            self.skipTest('host does not permit creating symlinks')
        try:
            with self.assertRaisesRegex(ValueError, 'reparse/symlink'):
                prep.prepare(link, self.root / 'link-out', self.root, run_smoke=False)
            with self.assertRaisesRegex(ValueError, 'reparse/symlink'):
                prep.prepare(self.source, link / 'out', self.root, run_smoke=False)
        finally:
            link.unlink()

    @unittest.skipUnless(os.name == 'nt', 'Windows junction test')
    def test_junction_inside_source_rejected(self):
        link = self.source / 'junction'
        result = subprocess.run(['cmd.exe', '/d', '/c', 'mklink', '/J', str(link), str(self.root)],
                                capture_output=True, timeout=10)
        if result.returncode:
            self.skipTest('host does not permit creating junctions')
        try:
            with self.assertRaisesRegex(ValueError, 'reparse/symlink'):
                prep.verify_snapshot(self.source, self.policy)
        finally:
            link.rmdir()

    def test_wrong_companion_fails_atomically(self):
        archive = self.root / 'wrong.zip'
        archive.write_bytes(b'not the reviewed ZIP')
        with self.assertRaisesRegex(ValueError, 'historical source companion'):
            prep.prepare(self.source, self.root / 'wrong-companion', self.root,
                         old_companion=archive, run_smoke=False)
        self.assertFalse((self.root / 'wrong-companion').exists())


class PreparedCompanionTests(unittest.TestCase):
    def test_local_final_companion_excludes_nested_gcm_archive(self):
        root = ROOT / 'target/runtime-distribution/mingit-2.55.0.5-no-gcm-v2'
        if not root.is_dir():
            self.skipTest('final local companion not built')
        policy = prep.load_policy(prep.POLICY)
        record = json.loads((root / 'source-companion-record.json').read_text(encoding='utf-8'))
        self.assertEqual(file_record(root / record['path']), {k: record[k] for k in ('size', 'sha256')})
        with zipfile.ZipFile(root / record['path']) as archive:
            self.assertFalse(set(policy['source_companion']['exclude_members']) & set(archive.namelist()))
            manifest = json.loads(archive.read('source-manifest.json'))
            self.assertEqual(len(manifest['packages']), 57)
            self.assertNotIn(prep.GCM_PACKAGE, {p['name'] for p in manifest['packages']})
            self.assertNotIn(prep.GCM_ARTIFACT, {a['id'] for a in manifest['artifacts']})
            self.assertIn('mingw-w64-git-extra', {a['id'] for a in manifest['artifacts']})
            # Historical companion is immutable: verify its recorded builder, not today's script.
            import hashlib
            builder = archive.read('distribution/prepare_runtime_distribution.py')
            distribution = json.loads(archive.read('distribution/MANIFEST.json'))
            self.assertEqual({'sha256': hashlib.sha256(builder).hexdigest(), 'size': len(builder)},
                             distribution['preparation_script'])
            for item in record['members']:
                self.assertNotIn(item['sha256'], policy['blocked_sha256'])
        self.assertEqual(len(manifest['runtime']['files']), 310)


if __name__ == '__main__':
    unittest.main()
