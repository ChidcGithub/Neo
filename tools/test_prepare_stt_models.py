"""Synthetic offline fixtures; patch only pins/limits, never integrity checks."""
import bz2
import hashlib
import io
import json
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

if __package__:
    from . import prepare_stt_models as p
else:
    import prepare_stt_models as p


def digest(data):
    return hashlib.sha256(data).hexdigest()


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cache = self.root / 'cache'
        self.cache.mkdir()
        self.output = self.root / 'delivery/models'
        self.report = self.root / 'evidence/member-sha256.json'
        self.model, self.tokens, self.vad = b'model fixture', b'tokens fixture', b'vad fixture'
        self.prefix = p.artifacts.PREFIX
        self.log = io.StringIO()
        self.enterContext(redirect_stdout(self.log))
        self.enterContext(patch.object(p.notices, 'MODEL_PINS', {
            'resources/models/stt/sense-voice/model.int8.onnx': digest(self.model),
            'resources/models/stt/sense-voice/tokens.txt': digest(self.tokens),
        }))
        self.fixture()

    def members(self):
        return [(self.prefix, b'', tarfile.DIRTYPE),
                (self.prefix + '/model.int8.onnx', self.model, tarfile.REGTYPE),
                (self.prefix + '/tokens.txt', self.tokens, tarfile.REGTYPE),
                (self.prefix + '/test.wav', b'not copied or played', tarfile.REGTYPE),
                (self.prefix + '/LICENSE', b'not copied', tarfile.REGTYPE)]

    def fixture(self, members=None, transform=None):
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, data, kind in self.members() if members is None else members:
                info = tarfile.TarInfo(name)
                info.type, info.size = kind, len(data)
                if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    info.linkname = '../outside'
                archive.addfile(info, io.BytesIO(data))
        data = bz2.compress(raw.getvalue())
        if transform:
            data = transform(data)
        (self.cache / 'sv.tar.bz2').write_bytes(data)
        (self.cache / 'silero_vad.onnx').write_bytes(self.vad)
        self.pin_raw()

    def pin_raw(self):
        pins = tuple((name, remote, (self.cache / name).stat().st_size,
                      digest((self.cache / name).read_bytes()))
                     for name, remote, _, _ in p.artifacts.ARTIFACTS)
        self.enterContext(patch.object(p.artifacts, 'ARTIFACTS', pins))

    def prepare(self, **kwargs):
        return p.prepare(self.cache, self.output, self.report, **kwargs)

    def assert_no_output(self):
        self.assertFalse(self.output.exists())
        self.assertFalse(self.report.exists())
        self.assertEqual(list(self.root.rglob('.stt-*')), [])

    def test_success_whitelist_metadata_raw_unchanged_and_progress(self):
        before = {path.name: (path.read_bytes(), path.stat().st_mtime_ns) for path in self.cache.iterdir()}
        with patch.object(tarfile.TarFile, 'extractall', side_effect=AssertionError('extractall forbidden')), \
                patch.object(subprocess, 'run', side_effect=AssertionError('subprocess forbidden')):
            evidence = self.prepare()
        self.assertEqual({path.relative_to(self.output).as_posix(): path.read_bytes()
                          for path in self.output.rglob('*') if path.is_file()}, {
                              'sense-voice/model.int8.onnx': self.model,
                              'sense-voice/tokens.txt': self.tokens,
                              'vad/silero_vad.onnx': self.vad})
        self.assertEqual(json.loads(self.report.read_text()), evidence)
        expected = [{'archive': 'sv.tar.bz2', 'path': name, 'size': len(data), 'sha256': digest(data)}
                    for name, data, kind in self.members() if kind == tarfile.REGTYPE]
        expected.sort(key=lambda row: row['path'])
        expected.append({'archive': None, 'path': 'silero_vad.onnx',
                         'size': len(self.vad), 'sha256': digest(self.vad)})
        self.assertEqual(evidence['members'], expected)
        self.assertEqual(before, {path.name: (path.read_bytes(), path.stat().st_mtime_ns)
                                  for path in self.cache.iterdir()})
        text = self.log.getvalue()
        for label in ('starting', 'Hash start', 'Hash end', 'Archive start', 'Archive member',
                      'Archive end', 'VAD copy end', 'STT preparation end', 'expanded_bytes='):
            self.assertIn(label, text)
        self.assertLessEqual(len(text.splitlines()), 2 * p.artifacts.MAX_MEMBERS + 12)
        self.assertLess(max(map(len, text.splitlines())), 500)

    def test_progress_is_flushed(self):
        with patch('builtins.print') as printed:
            p.progress('test')
        printed.assert_called_once_with('test', flush=True)

    def test_defaults_and_custom_cli_paths(self):
        with patch.object(p, 'prepare') as prepare:
            self.assertEqual(p.main([]), 0)
            prepare.assert_called_once_with(Path('.cache/stt'), None, None, timeout=180)
        self.assertEqual(p.main(['--cache', str(self.cache), '--output', str(self.output),
                                 '--report', str(self.report)]), 0)

    def test_cache_relative_default_destinations(self):
        p.prepare(self.cache)
        self.assertTrue((self.cache / 'models/sense-voice/model.int8.onnx').is_file())
        self.assertTrue((self.cache / 'member-sha256.json').is_file())

    def test_raw_size_and_hash_mismatch_preserve_cache(self):
        for name in ('sv.tar.bz2', 'silero_vad.onnx'):
            path = self.cache / name
            original = path.read_bytes()
            for bad in (original + b'x', bytes([original[0] ^ 1]) + original[1:]):
                with self.subTest(name=name, size=len(bad)):
                    path.write_bytes(bad)
                    with self.assertRaisesRegex(ValueError, 'size mismatch|SHA-256 mismatch'):
                        self.prepare()
                    self.assertEqual(path.read_bytes(), bad)
                    self.assert_no_output()
            path.write_bytes(original)

    def test_member_pin_mismatch_and_missing_required(self):
        for name in p.MODEL_NAMES:
            with self.subTest(name=name):
                members = self.members()
                self.fixture([(n, b'bad' if n.endswith('/' + name) else data, kind)
                              for n, data, kind in members])
                with self.assertRaisesRegex(ValueError, 'Model/tokens SHA-256 mismatch'):
                    self.prepare()
                self.assert_no_output()
                self.fixture([row for row in members if not row[0].endswith('/' + name)])
                with self.assertRaisesRegex(ValueError, 'Missing required'):
                    self.prepare()
                self.assert_no_output()

    def test_all_members_checked_even_after_whitelist(self):
        bad_names = ('../escape', '/absolute', 'C:/file', 'a\\b', 'a/../b', 'a//b',
                     'file:ads', 'NUL.txt', 'bad. ', 'bad\nname')
        bad_names = list(bad_names) + [self.prefix + '/' + name for name in bad_names]
        bad_names += ['other/extra', self.prefix + '-suffix/extra', self.prefix.upper() + '/extra']
        for name in bad_names:
            with self.subTest(name=name):
                self.fixture(self.members() + [(name, b'x', tarfile.REGTYPE)])
                with self.assertRaises(ValueError):
                    self.prepare()
                self.assert_no_output()

    def test_duplicate_case_and_normalized_names(self):
        for name in (self.prefix + '/tokens.txt', './' + self.prefix + '/tokens.txt',
                     self.prefix + '/TOKENS.TXT', self.prefix + '/'):
            with self.subTest(name=name):
                self.fixture(self.members() + [(name, b'', tarfile.DIRTYPE)])
                with self.assertRaisesRegex(ValueError, 'Duplicate'):
                    self.prepare()
                self.assert_no_output()

    def test_links_special_sparse_and_extension_headers(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE,
                     tarfile.BLKTYPE, tarfile.GNUTYPE_SPARSE, tarfile.XHDTYPE,
                     tarfile.XGLTYPE, tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK):
            with self.subTest(kind=kind):
                self.fixture(self.members() + [(self.prefix + '/extra', b'', kind)])
                with self.assertRaisesRegex(ValueError, 'forbidden'):
                    self.prepare()
                self.assert_no_output()

    def test_directory_size_and_required_file_type(self):
        for row in ((self.prefix + '/extra', b'x', tarfile.DIRTYPE),
                    (self.prefix + '/tokens.txt', b'', tarfile.DIRTYPE),
                    (self.prefix, b'', tarfile.REGTYPE)):
            with self.subTest(row=row):
                self.fixture([item for item in self.members() if item[0] != row[0]] + [row])
                with self.assertRaises(ValueError):
                    self.prepare()
                self.assert_no_output()

    def test_member_count_and_member_size_budgets(self):
        for key, value in (('MAX_MEMBERS', 3), ('MAX_MEMBER', 3), ('MAX_EXPANDED', 1024)):
            with self.subTest(key=key), patch.object(p.artifacts, key, value):
                with self.assertRaisesRegex(ValueError, 'limit'):
                    self.prepare()
                self.assert_no_output()

    def test_declared_full_archive_budget(self):
        # A final huge declaration must fail before reading/copying its payload.
        info = tarfile.TarInfo(self.prefix + '/extra')
        info.size = p.artifacts.MAX_EXPANDED + 1
        self.fixture(transform=lambda data: bz2.compress(info.tobuf() + b'\0' * 1024))
        with patch.object(p.artifacts, 'MAX_MEMBER', info.size), \
                self.assertRaisesRegex(ValueError, 'declared-size limit'):
            self.prepare()
        self.assert_no_output()

    def test_expanded_budget_counts_padding_and_trailing_stream(self):
        for transform in (None, lambda data: data + bz2.compress(b'0' * 20000)):
            with self.subTest(trailing=transform is not None):
                self.fixture(transform=transform)
                # tar payload is tiny, but record padding alone is 10240 bytes.
                with patch.object(p.artifacts, 'MAX_EXPANDED', 15000 if transform else 9000), \
                        self.assertRaisesRegex(ValueError, 'expanded-byte limit'):
                    self.prepare()
                self.assert_no_output()

    def test_corrupt_and_truncated_streams_leave_no_output(self):
        def bad_crc(data):
            mutated = bytearray(data)
            mutated[-5] ^= 0xff
            return bytes(mutated)
        transforms = (lambda data: b'not bz2', lambda data: data[:30],
                      lambda data: data[:-5], bad_crc,
                      lambda data: data + bz2.compress(b'trailing')[:-4],
                      lambda data: bz2.compress(bz2.decompress(data)[:600]))
        for transform in transforms:
            with self.subTest(transform=transform):
                self.fixture(transform=transform)
                with self.assertRaises((OSError, ValueError, EOFError, tarfile.TarError)):
                    self.prepare()
                self.assert_no_output()

    def test_timeout_at_hash_scan_drain_copy_and_publication(self):
        # Advance the monotonic clock at observable stages; no real sleeping.
        labels = ('Hash start:', 'Archive start:', 'Archive member 4:', 'Archive end:',
                  'VAD copy start:', 'VAD copy end:')
        for label in labels:
            with self.subTest(label=label):
                clock = [0.0]
                original = p.progress
                def progress(message, original=original, label=label, clock=clock):
                    original(message)
                    if message.startswith(label):
                        clock[0] = 180.0
                with patch.object(p.time, 'monotonic', side_effect=lambda clock=clock: clock[0]), \
                        patch.object(p, 'progress', side_effect=progress), self.assertRaises(TimeoutError):
                    self.prepare()
                self.assert_no_output()
        for seconds in (0, -1, 181, float('inf'), float('nan')):
            with self.subTest(seconds=seconds), self.assertRaises(ValueError):
                self.prepare(timeout=seconds)
        deadline = p.Deadline(180)
        with patch.object(p.time, 'monotonic', return_value=deadline.end), self.assertRaises(TimeoutError):
            p.TimedReader(io.BytesIO(b'x'), 100, deadline).read(1)

    def test_existing_output_or_report_not_overwritten(self):
        for target in (self.output, self.report):
            with self.subTest(target=target):
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(b'keep')
                with self.assertRaisesRegex(ValueError, 'not be overwritten'):
                    self.prepare()
                self.assertEqual(target.read_bytes(), b'keep')
                target.unlink()
        self.output.mkdir()
        with self.assertRaisesRegex(ValueError, 'not be overwritten'):
            self.prepare()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_output_report_overlap_and_parent_traversal(self):
        for report in (self.output, self.output / 'report.json'):
            with self.assertRaisesRegex(ValueError, 'overlap'):
                p.prepare(self.cache, self.output, report)
        with self.assertRaisesRegex(ValueError, 'traversal'):
            p.prepare(self.cache, self.root / 'missing/../models', self.report)
        self.assert_no_output()

    def test_link_reparse_and_hardlink_checks_without_privileges(self):
        # lstat fakes avoid Windows symlink privilege requirements; actual
        # filesystem checks still run for all unrelated components.
        original = Path.lstat
        for target in (self.output.parent, self.output, self.report.parent, self.report,
                       self.cache / 'sv.tar.bz2'):
            for mode, attributes, links in ((stat.S_IFLNK | 0o777, 0, 1),
                                            (stat.S_IFDIR | 0o777, 0x400, 1),
                                            (stat.S_IFREG | 0o666, 0, 2)):
                with self.subTest(target=target, mode=mode, attributes=attributes):
                    def lstat(path, *args, target=target, mode=mode, attributes=attributes, links=links, **kwargs):
                        if path == target:
                            return SimpleNamespace(st_mode=mode, st_file_attributes=attributes, st_nlink=links)
                        return original(path, *args, **kwargs)
                    with patch.object(Path, 'lstat', lstat), self.assertRaisesRegex(ValueError, 'forbidden'):
                        self.prepare()
                    self.assert_no_output()

    def test_staged_hardlink_is_rejected(self):
        original = Path.lstat
        def lstat(path, *args, **kwargs):
            info = original(path, *args, **kwargs)
            if path.name == 'model.int8.onnx' and '.stt-models-' in str(path):
                return SimpleNamespace(st_mode=info.st_mode, st_nlink=2, st_file_attributes=0)
            return info
        with patch.object(Path, 'lstat', lstat), self.assertRaisesRegex(ValueError, 'hardlink'):
            self.prepare()
        self.assert_no_output()

    def test_report_or_model_rename_failure_rolls_back(self):
        original = Path.rename
        for target in (self.output, self.report):
            with self.subTest(target=target):
                def rename(path, destination, target=target):
                    if destination == target:
                        raise OSError('synthetic rename failure')
                    return original(path, destination)
                with patch.object(Path, 'rename', rename), self.assertRaisesRegex(OSError, 'synthetic'):
                    self.prepare()
                self.assert_no_output()

    def test_timeout_after_report_publish_rolls_back(self):
        original = Path.rename
        clock = [0.0]
        def rename(path, target):
            result = original(path, target)
            if target == self.report:
                clock[0] = 180.0
            return result
        with patch.object(Path, 'rename', rename), \
                patch.object(p.time, 'monotonic', side_effect=lambda: clock[0]), self.assertRaises(TimeoutError):
            self.prepare()
        self.assert_no_output()

    def test_cli_failure_returns_nonzero(self):
        (self.cache / 'sv.tar.bz2').write_bytes(b'bad')
        self.assertEqual(p.main(['--cache', str(self.cache), '--output', str(self.output),
                                 '--report', str(self.report)]), 1)
        self.assertIn('STT preparation failed:', self.log.getvalue())
        self.assert_no_output()

    def test_script_and_package_help(self):
        root = Path(__file__).resolve().parents[1]
        for arguments in (['tools/prepare_stt_models.py', '--help'],
                          ['-m', 'tools.prepare_stt_models', '--help']):
            with self.subTest(arguments=arguments):
                result = subprocess.run([sys.executable, '-B', *arguments], cwd=root,
                                        capture_output=True, text=True, timeout=10, check=False)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn('--output', result.stdout)
                self.assertIn('--report', result.stdout)


if __name__ == '__main__':
    unittest.main()
