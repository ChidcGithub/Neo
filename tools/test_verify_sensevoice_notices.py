"""Synthetic, offline fixtures; link checks need no OS link privileges."""
import hashlib
import io
import json
from pathlib import Path
import shutil
import stat
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

if __package__:
    from . import verify_sensevoice_notices as v
else:
    import verify_sensevoice_notices as v


def digest(content):
    return hashlib.sha256(content).hexdigest()


def put(root, name, content):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


class SenseVoiceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='sensevoice-notices-')
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root, self.package = self.base / 'root', self.base / 'package'
        contents = {name: ('synthetic ' + name).encode() + b'\r\n\xff'
                    for name in v.DOCUMENTS}
        # No keywords or legal conclusions are required in the parent's notice.
        contents[v.NOTICE] = b'\xff\r\n'
        for name, content in contents.items():
            put(self.root, name, content)
        shutil.copytree(self.root, self.package)
        model_contents = {name: ('synthetic model ' + name).encode() for name in v.MODEL_PINS}
        for name, content in model_contents.items():
            put(self.package, name, content)
        self.enterContext(patch.dict(v.LICENSE_PINS,
                                    {name: digest(contents[name]) for name in v.LICENSE_PINS}))
        self.enterContext(patch.dict(v.MODEL_PINS,
                                    {name: digest(content) for name, content in model_contents.items()}))
        # Fail immediately if verification ever grows download/execution paths.
        for target in ('socket.socket', 'urllib.request.urlopen', 'subprocess.Popen', 'os.system'):
            self.enterContext(patch(target, side_effect=AssertionError('No network or execution')))

    def verify(self):
        return v.verify(self.root, self.package)

    def cli(self, *args):
        output = io.StringIO()
        with patch.object(v, 'ROOT', self.root), patch('sys.stdout', output):
            code = v.main(list(args))
        return code, json.loads(output.getvalue())

    def snapshot(self):
        return {p.relative_to(self.base).as_posix(): p.read_bytes()
                for p in self.base.rglob('*') if p.is_file()}

    def test_copies_pass_without_writes_or_approval(self):
        before = self.snapshot()
        report = self.verify()
        self.assertEqual(report['mode'], 'integrity-only')
        self.assertEqual(report['status'], 'verified')
        self.assertFalse(report['release_clearance'])
        self.assertEqual(set(report['documents']), set(v.DOCUMENTS))
        self.assertEqual(set(report['models']), set(v.MODEL_PINS))
        self.assertEqual(before, self.snapshot())

    def test_default_cli_reads_root_docs_only(self):
        shutil.rmtree(self.package)
        code, report = self.cli()
        self.assertEqual(code, 0)
        self.assertEqual(report['scope'], 'root-docs')
        self.assertEqual(report['models'], {})
        self.assertFalse(report['release_clearance'])

    def test_package_cli(self):
        code, report = self.cli('--package', str(self.package))
        self.assertEqual(code, 0)
        self.assertEqual(report['scope'], 'root-docs-and-package')
        self.assertEqual(report['mode'], 'integrity-only')
        self.assertFalse(report['release_clearance'])

    def test_missing_documents_in_either_tree(self):
        for root in (self.root, self.package):
            for name in v.DOCUMENTS:
                with self.subTest(root=root, name=name):
                    path = root / name
                    content = path.read_bytes()
                    path.unlink()
                    with self.assertRaises(OSError):
                        self.verify()
                    path.write_bytes(content)

    def test_empty_required_documents(self):
        for root in (self.root, self.package):
            for name in v.DOCUMENTS:
                with self.subTest(root=root, name=name):
                    path = root / name
                    content = path.read_bytes()
                    path.write_bytes(b'')
                    with self.assertRaisesRegex(ValueError, 'Empty required file'):
                        self.verify()
                    path.write_bytes(content)

    def test_both_license_pins_reject_even_matching_mutations(self):
        for name in v.LICENSE_PINS:
            with self.subTest(name=name):
                content = (self.root / name).read_bytes()
                for root in (self.root, self.package):
                    put(root, name, content + b'mutation')
                with self.assertRaisesRegex(ValueError, 'Root license SHA-256 mismatch'):
                    self.verify()
                for root in (self.root, self.package):
                    put(root, name, content)

    def test_every_package_document_must_match_raw_root_bytes(self):
        for name in v.DOCUMENTS:
            with self.subTest(name=name):
                path = self.package / name
                content = path.read_bytes()
                path.write_bytes(content.replace(b'\r\n', b'\n'))
                with self.assertRaisesRegex(ValueError, 'Package/root document bytes differ'):
                    self.verify()
                path.write_bytes(content)

    def test_unpinned_documents_follow_root_not_keywords_or_approval(self):
        for name in set(v.DOCUMENTS) - set(v.LICENSE_PINS):
            for root in (self.root, self.package):
                put(root, name, b'changed bytes; APPROVED does not grant clearance\r\n')
        report = self.verify()
        self.assertFalse(report['release_clearance'])
        self.assertEqual(report['mode'], 'integrity-only')

    def test_models_missing_empty_or_mutated(self):
        for name in v.MODEL_PINS:
            path = self.package / name
            content = path.read_bytes()
            for replacement in (None, b'', content + b'mutation'):
                with self.subTest(name=name, replacement=replacement):
                    if replacement is None:
                        path.unlink()
                    else:
                        path.write_bytes(replacement)
                    with self.assertRaises((OSError, ValueError)):
                        self.verify()
                    path.write_bytes(content)

    def test_models_cannot_use_root_or_package_local_pin_substitutes(self):
        for name in v.MODEL_PINS:
            put(self.package, name, b'substituted')
            put(self.root, 'crates/neo-stt/assets/sense-voice/' + Path(name).name, b'substituted')
        put(self.package, 'model-pins.json', b'{}')
        with self.assertRaisesRegex(ValueError, 'Package model/tokens SHA-256 mismatch'):
            self.verify()

    def test_links_and_reparse_ancestors_without_link_privileges(self):
        original = Path.lstat
        targets = (self.root, self.root / v.NOTICE, self.package,
                   self.package / 'docs/licenses/models', self.package / v.NOTICE,
                   self.package / next(iter(v.MODEL_PINS)))
        for target in targets:
            for kind in ('symlink', 'reparse', 'hardlink'):
                if kind == 'hardlink' and target.is_dir():
                    continue
                with self.subTest(target=target, kind=kind):
                    def fake_lstat(path, *args, **kwargs):
                        info = original(path, *args, **kwargs)
                        if path != target:
                            return info
                        return SimpleNamespace(
                            st_mode=stat.S_IFLNK if kind == 'symlink' else info.st_mode,
                            st_file_attributes=0x400 if kind == 'reparse' else 0,
                            st_nlink=2 if kind == 'hardlink' else 1)
                    with patch.object(Path, 'lstat', fake_lstat):
                        with self.assertRaisesRegex(ValueError, 'Linked'):
                            self.verify()

    def test_directory_in_place_of_file(self):
        path = self.package / v.NOTICE
        path.unlink()
        path.mkdir()
        with self.assertRaisesRegex(ValueError, 'non-regular'):
            self.verify()

    def test_cli_failure_is_integrity_only_and_does_not_write_notice(self):
        (self.root / v.NOTICE).unlink()
        before = self.snapshot()
        code, report = self.cli()
        self.assertEqual(code, 1)
        self.assertEqual(report['status'], 'failed')
        self.assertEqual(report['mode'], 'integrity-only')
        self.assertFalse(report['release_clearance'])
        self.assertIn('SENSEVOICE-NOTICE.txt', report['error'])
        self.assertEqual(before, self.snapshot())

    def test_unreadable_file_fails_without_changing_permissions(self):
        with patch.object(Path, 'open', side_effect=PermissionError('synthetic denied')):
            code, report = self.cli()
        self.assertEqual(code, 1)
        self.assertIn('synthetic denied', report['error'])
        self.assertFalse(report['release_clearance'])


if __name__ == '__main__':
    unittest.main()
