import bz2
import hashlib
import io
import json
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

if __package__:
    from . import verify_model_artifacts as v
else:
    import verify_model_artifacts as v


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def archive(self, members):
        path = self.root / 'fixture.tar.bz2'
        with tarfile.open(path, 'w:bz2') as archive:
            for name, content, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                info.size = len(content)
                if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    info.linkname = '../outside'
                archive.addfile(info, io.BytesIO(content))
        return path

    def test_script_and_package_imports(self):
        root = Path(__file__).resolve().parents[1]
        commands = (
            ['tools/verify_model_artifacts.py', '--help'],
            ['-m', 'tools.verify_model_artifacts', '--help'],
            ['-c', 'import tools.verify_model_artifacts'],
        )
        for arguments in commands:
            with self.subTest(arguments=arguments):
                result = subprocess.run(
                    [sys.executable, *arguments], cwd=root,
                    capture_output=True, text=True, timeout=10, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                if '--help' in arguments:
                    self.assertIn('--download', result.stdout)

    def test_hashes_without_extraction(self):
        path = self.archive([('model', b'weight', tarfile.REGTYPE)])
        rows = v.archive_members(path)
        self.assertEqual(rows[0]['sha256'], hashlib.sha256(b'weight').hexdigest())
        self.assertEqual(list(self.root.iterdir()), [path])

    def test_unsafe_names(self):
        for name in ('../bad', '/absolute', 'C:/file', 'a\\b', 'a/../b', 'a//b', 'file:ads', 'NUL.txt', 'a. ', 'a\nfile'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                v.archive_members(self.archive([(name, b'x', tarfile.REGTYPE)]))

    def test_links_and_special_members(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE):
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                v.archive_members(self.archive([('unsafe', b'', kind)]))

    def test_duplicate_case_alias(self):
        with self.assertRaises(ValueError):
            v.archive_members(self.archive([('A', b'', tarfile.REGTYPE), ('./a', b'', tarfile.REGTYPE)]))

    def test_size_count_and_expanded_limits(self):
        path = self.archive([('a', b'1234', tarfile.REGTYPE), ('b', b'5678', tarfile.REGTYPE)])
        for limits in ({'max_member': 3}, {'max_members': 1}, {'max_expanded': 1024}):
            with self.subTest(limits=limits), self.assertRaises(ValueError):
                v.archive_members(path, **limits)

    def test_trailing_bomb(self):
        path = self.archive([('a', b'', tarfile.REGTYPE)])
        path.write_bytes(path.read_bytes() + bz2.compress(b'0' * 20000))
        with self.assertRaises(ValueError):
            v.archive_members(path, max_expanded=15000)

    def test_existing_source_preserved(self):
        path = self.root / 'source'
        path.write_bytes(b'keep')
        artifact = ('source', 'source', 4, hashlib.sha256(b'keep').hexdigest())
        with patch.object(v.subprocess, 'run') as run:
            self.assertEqual(v.download(self.root, artifact, self.root / 'log.json')['status'], 'verified-existing')
            with self.assertRaises(ValueError):
                v.download(self.root, (*artifact[:3], '0' * 64), self.root / 'log.json')
            run.assert_not_called()
        self.assertEqual(path.read_bytes(), b'keep')

    def test_budget_reserved_across_failed_runs(self):
        log = self.root / 'log.json'
        v.save_json(log, [{'reserved_bytes': v.NETWORK_BUDGET}])
        with patch.object(v.subprocess, 'run') as run, self.assertRaises(ValueError):
            v.download(self.root, ('source', 'source', 1, '0' * 64), log)
        run.assert_not_called()

    def test_timeout_is_recorded_without_retry(self):
        log = self.root / 'log.json'
        with patch.object(v.subprocess, 'run', side_effect=v.subprocess.TimeoutExpired('curl', 185)) as run:
            with self.assertRaises(v.subprocess.TimeoutExpired):
                v.download(self.root, ('source', 'source', 1, '0' * 64), log)
            self.assertEqual(run.call_count, 1)
        row = json.loads(log.read_text())[0]
        self.assertEqual(row['status'], 'failed')
        self.assertEqual(row['timeout_seconds'], 180)
        self.assertEqual(row['retries'], 0)

    def test_missing_artifacts_write_blocked_report(self):
        with patch.object(v, 'ROOT', self.root), patch('sys.argv', ['verify_model_artifacts.py']):
            self.assertEqual(v.main(), 1)
        report = json.loads((self.root / 'docs-pri/licenses/models-evidence/ci-artifact-verification.json').read_text())
        self.assertEqual(report['status'], 'blocked')
        self.assertFalse(report['members_verified'])
        self.assertFalse(report['release_clearance'])

    def test_download_failure_does_not_skip_other_artifact(self):
        with patch.object(v, 'ROOT', self.root), patch('sys.argv', ['verify_model_artifacts.py', '--download']), patch.object(v, 'download', side_effect=RuntimeError('offline')) as download:
            self.assertEqual(v.main(), 1)
            self.assertEqual(download.call_count, 2)

    def test_corrupt_archive(self):
        path = self.archive([('a', b'x' * 10000, tarfile.REGTYPE)])
        path.write_bytes(path.read_bytes()[:30])
        with self.assertRaises((EOFError, OSError, tarfile.TarError)):
            v.archive_members(path)


if __name__ == '__main__':
    unittest.main()
