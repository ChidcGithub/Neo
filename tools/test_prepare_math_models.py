"""Offline regressions; tiny fake HTTPS responses, no models/GUI/external commands."""
from __future__ import annotations

import copy
import hashlib
import io
import json
import shutil
import stat
import tempfile
import unittest
import urllib.error
import urllib.request
import warnings
import zipfile
from http.client import HTTPMessage
from pathlib import Path
from unittest.mock import Mock, patch

if __package__:
    from . import prepare_math_models as models
else:
    import prepare_math_models as models


def pin(data):
    return {'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


class Response(io.BytesIO):
    status = 200

    def __init__(self, data, url, headers=None):
        super().__init__(data)
        self.url = url
        self.headers = headers if headers is not None else {'Content-Length': str(len(data))}

    def geturl(self):
        return self.url


class PrepareMathModelsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.package = self.root / 'base package'
        (self.package / 'apps/blackboard').mkdir(parents=True)
        (self.package / 'apps/drawing').mkdir()
        self.manifest = b'{"quality_validated": false, "unchanged": true}\n'
        (self.package / 'MANIFEST.json').write_bytes(self.manifest)
        self.cache = self.root / 'cache'
        self.lock_path = self.root / 'lock.json'
        self.lock = copy.deepcopy(models.load_lock())
        self.int8 = {name: ('fixture: ' + name).encode() for name in models.INT8_FILES}
        self.int8['optimization.json'] = self.manifest
        self.int8['FILES.sha256.json'] = json.dumps({
            'texteller-int8/' + name: {'bytes': len(data), 'sha256': pin(data)['sha256']}
            for name, data in self.int8.items() if name != 'FILES.sha256.json'
        }).encode()
        self.lock['int8']['files'] = {name: pin(data) for name, data in self.int8.items()}
        self.fp32 = {name: ('fp32 fixture: ' + name).encode() for name in self.lock['fp32']['files']}
        for name, data in self.fp32.items():
            self.lock['fp32']['files'][name].update(pin(data))
        license_path = self.root / models.LICENSE_SOURCE
        license_path.parent.mkdir(parents=True)
        license_path.write_bytes(self.fp32['LICENSE-APACHE-2.0.txt'])
        self.payloads = {self.lock['fp32']['files'][name]['url']: data for name, data in self.fp32.items()}
        self.set_archive(self.make_zip())
        self.opener = Mock()
        self.opener.open.side_effect = self.respond
        network = patch.object(models.urllib.request, 'build_opener', return_value=self.opener)
        self.network = network.start()
        self.addCleanup(network.stop)

    def respond(self, request, timeout):
        self.assertEqual(timeout, 60)
        self.assertEqual(request.get_header('Accept-encoding'), 'identity')
        self.assertTrue(request.full_url.startswith('https://'))
        return Response(self.payloads[request.full_url], request.full_url)

    def make_zip(self, *, flat=False, entries=None):
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w', zipfile.ZIP_DEFLATED) as archive:
            for name, body in (entries if entries is not None else [
                    (('' if flat else 'texteller-int8/') + name, body)
                    for name, body in self.int8.items()]):
                if isinstance(name, str):
                    info = zipfile.ZipInfo(name)
                    # Preserve hostile raw names: Windows ZipInfo normalizes backslashes.
                    info.filename = name
                    info.orig_filename = name
                else:
                    info = name
                archive.writestr(info, body)
        return data.getvalue()

    def set_archive(self, data):
        self.archive = data
        self.lock['int8']['archive'].update(pin(data))
        self.payloads[models.INT8_URL] = data
        self.save_lock()

    def save_lock(self):
        self.lock_path.write_text(json.dumps(self.lock), encoding='utf-8')

    def prepare(self, variant='int8', package=None, **kwargs):
        return models.prepare(variant, package or self.package, self.cache,
                              lock_path=self.lock_path, root=self.root, **kwargs)

    def assert_untouched(self, package=None):
        package = package or self.package
        self.assertEqual((package / 'MANIFEST.json').read_bytes(), self.manifest)
        self.assertEqual(list((package / 'apps/drawing').iterdir()), [])
        self.assertEqual(list(package.glob('.math-models-*')), [])
        self.assertEqual(list(self.cache.glob('*.partial')), [])

    def assert_failed_cleanly(self, pattern=None):
        with self.assertRaises((ValueError, RuntimeError, zipfile.BadZipFile)) as caught:
            self.prepare()
        if pattern:
            self.assertRegex(str(caught.exception), pattern)
        self.assertFalse((self.package / 'apps/blackboard/models').exists())
        self.assert_untouched()

    def test_production_lock_pins_and_public_license(self):
        lock = models.load_lock()
        self.assertEqual(lock['revision'], models.REVISION)
        self.assertEqual(lock['int8']['archive'], {
            'url': models.INT8_URL, 'size': 292826942,
            'sha256': '07fdb9e9d4f7a684c5a243a25d117f16b0719d199101985ae77d2b789d9610ab'})
        self.assertEqual(lock['fp32']['files']['encoder_model.onnx']['size'], 343553824)
        self.assertEqual(lock['fp32']['files']['decoder_model.onnx']['sha256'],
                         '288cb8e37cdda66f725f2739efd2e7846636666a815e27533891c68389ec3659')
        license_pin = lock['fp32']['files']['LICENSE-APACHE-2.0.txt']
        models.verify_file(models.ROOT / license_pin['source'], license_pin)
        self.assertEqual(lock['int8']['files']['UPSTREAM-MODEL-CARD.md']['sha256'],
                         lock['fp32']['files']['UPSTREAM-MODEL-CARD.md']['sha256'])

    def test_int8_wrapped_layout_preserves_all_bytes_and_quality(self):
        destination = self.prepare()
        self.assertEqual(destination, self.package / 'apps/blackboard/models/texteller-int8')
        self.assertEqual({p.name: p.read_bytes() for p in destination.iterdir()}, self.int8)
        self.assertFalse(json.loads((destination / 'optimization.json').read_bytes())['quality_validated'])
        self.assertEqual(self.opener.open.call_count, 1)
        self.assert_untouched()

    def test_int8_flat_layout(self):
        self.set_archive(self.make_zip(flat=True))
        destination = self.prepare()
        self.assertEqual({p.name: p.read_bytes() for p in destination.iterdir()}, self.int8)
        self.assert_untouched()

    def test_wrapped_directory_entry(self):
        entries = [('texteller-int8/', b'')] + [('texteller-int8/' + n, d) for n, d in self.int8.items()]
        self.set_archive(self.make_zip(entries=entries))
        self.prepare()

    def test_both_variants_from_same_base(self):
        fp32_package = self.root / 'neo-fp32'
        shutil.copytree(self.package, fp32_package)
        int8_destination = self.prepare()
        fp32_destination = self.prepare('fp32', fp32_package)
        self.assertEqual(fp32_destination, fp32_package / 'apps/blackboard/models/texteller')
        self.assertEqual({p.name: p.read_bytes() for p in fp32_destination.iterdir()}, self.fp32)
        self.assertFalse((fp32_destination.parent / 'texteller-int8').exists())
        self.assertFalse((int8_destination.parent / 'texteller').exists())
        self.assert_untouched(fp32_package)
        self.assert_untouched()
        self.assertEqual(self.opener.open.call_count, 7)  # ZIP + six FP32 remote files

    def test_cache_reuse_for_new_package_and_idempotent_install(self):
        for variant in ('int8', 'fp32'):
            package = self.root / variant
            shutil.copytree(self.package, package)
            destination = self.prepare(variant, package)
            with patch.object(self.opener, 'open', side_effect=AssertionError('network on cache hit')):
                self.assertEqual(self.prepare(variant, package), destination)
                fresh = self.root / (variant + '-fresh')
                shutil.copytree(self.package, fresh)
                self.prepare(variant, fresh)
        self.assert_untouched()

    def test_local_int8_archive_is_verified_and_never_modified(self):
        local = self.root / 'texteller-int8-v1.zip'
        local.write_bytes(self.archive)
        self.opener.open.side_effect = AssertionError('local archive must not download')
        self.prepare(int8_archive=local)
        self.assertEqual(local.read_bytes(), self.archive)
        self.opener.open.assert_not_called()

    def test_bad_local_archive_and_wrong_variant(self):
        local = self.root / 'bad.zip'
        local.write_bytes(b'wrong')
        with self.assertRaisesRegex(ValueError, 'Size mismatch'):
            self.prepare(int8_archive=local)
        with self.assertRaisesRegex(ValueError, 'only valid'):
            self.prepare('fp32', int8_archive=local)
        self.opener.open.assert_not_called()
        self.assert_untouched()

    def test_network_failure_cleans_staging_and_partial(self):
        self.opener.open.side_effect = urllib.error.URLError('secret signed URL')
        self.assert_failed_cleanly('download failed')
        self.assertEqual(list(self.cache.iterdir()), [])

    def test_bad_hash_and_size_never_enter_cache_or_package(self):
        for data in (b'x' * len(self.archive), self.archive[:-1], self.archive + b'x'):
            with self.subTest(size=len(data)):
                self.payloads[models.INT8_URL] = data
                self.assert_failed_cleanly('mismatch')
                self.assertEqual(list(self.cache.iterdir()), [])

    def test_corrupt_cache_fails_closed_without_redownload(self):
        self.cache.mkdir()
        target = self.cache / (self.lock['int8']['archive']['sha256'] + '.blob')
        target.write_bytes(b'x' * len(self.archive))
        self.assert_failed_cleanly('SHA-256 mismatch')
        self.opener.open.assert_not_called()
        self.assertTrue(target.exists())

    def test_hash_valid_but_bad_zip(self):
        self.set_archive(b'not a ZIP')
        self.assert_failed_cleanly('not a zip file')

    def test_zip_paths_are_validated_before_extraction(self):
        for name in ('../escape', '/escape', 'C:/escape', 'texteller-int8/../../escape',
                     'texteller-int8\\escape', 'texteller-int8/CON.txt',
                     'texteller-int8/evil:ads', 'texteller-int8/trailing.',
                     'texteller-int8//empty', 'texteller-int8/./dot'):
            with self.subTest(name=name):
                self.set_archive(self.make_zip(entries=[(name, b'evil')]))
                self.assert_failed_cleanly('Unsafe path')
                self.assertFalse((self.root / 'escape').exists())

    def test_zip_duplicate_case_collision_and_mixed_layout(self):
        for entries in ([('config.json', b'a'), ('config.json', b'a')],
                        [('config.json', b'a'), ('CONFIG.JSON', b'a')]):
            # Use the correct size so the first header reaches duplicate detection.
            entries = [(n, self.int8['config.json']) for n, _ in entries]
            with warnings.catch_warnings():
                warnings.simplefilter('ignore', UserWarning)
                self.set_archive(self.make_zip(entries=entries))
            self.assert_failed_cleanly('Duplicate/case-colliding')
        entries = [(('' if n == 'config.json' else 'texteller-int8/') + n, d)
                   for n, d in self.int8.items()]
        self.set_archive(self.make_zip(entries=entries))
        self.assert_failed_cleanly('mixed-layout')

    def test_zip_links_special_files_and_size_limits(self):
        for kind in (stat.S_IFLNK, stat.S_IFIFO, stat.S_IFCHR):
            info = zipfile.ZipInfo('config.json')
            info.create_system = 3
            info.external_attr = (kind | 0o777) << 16
            self.set_archive(self.make_zip(entries=[(info, self.int8['config.json'])]))
            self.assert_failed_cleanly('special ZIP')
        self.set_archive(self.make_zip(entries=[('config.json', b'x' * 10000)]))
        self.assert_failed_cleanly('file/size')
        entries = [('extra' + str(i), b'x') for i in range(len(self.int8) + 2)]
        self.set_archive(self.make_zip(entries=entries))
        self.assert_failed_cleanly('member count')

    def test_missing_extra_and_corrupt_members(self):
        for missing in models.INT8_FILES:
            with self.subTest(missing=missing):
                self.set_archive(self.make_zip(entries=[(n, d) for n, d in self.int8.items() if n != missing]))
                self.assert_failed_cleanly('Incomplete')
        entries = list(self.int8.items()) + [('unexpected.txt', b'x')]
        self.set_archive(self.make_zip(entries=entries))
        self.assert_failed_cleanly('Unexpected ZIP')
        changed = dict(self.int8)
        changed['config.json'] = b'x' * len(changed['config.json'])
        self.set_archive(self.make_zip(entries=list(changed.items())))
        self.assert_failed_cleanly('SHA-256 mismatch')

    def test_internal_inventory_must_match_lock(self):
        self.int8['FILES.sha256.json'] = b'{}'
        self.lock['int8']['files']['FILES.sha256.json'] = pin(b'{}')
        self.set_archive(self.make_zip())
        self.assert_failed_cleanly('FILES.sha256.json differs')

    def test_fp32_failure_is_atomic_and_retry_uses_verified_cache(self):
        card_url = self.lock['fp32']['files']['UPSTREAM-MODEL-CARD.md']['url']
        card = self.payloads[card_url]
        self.payloads[card_url] = b'x' * len(card)
        with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
            self.prepare('fp32')
        self.assertFalse((self.package / 'apps/blackboard/models').exists())
        self.assert_untouched()
        self.opener.open.reset_mock()
        self.payloads[card_url] = card
        self.prepare('fp32')
        self.assertEqual(self.opener.open.call_count, 1)

    def test_public_license_network_fallback_without_development_files(self):
        (self.root / models.LICENSE_SOURCE).unlink()
        self.prepare('fp32')
        self.assertEqual(self.opener.open.call_count, 7)
        self.assert_untouched()

    def test_corrupt_public_license_is_not_silently_replaced(self):
        (self.root / models.LICENSE_SOURCE).write_bytes(b'corrupt')
        with self.assertRaisesRegex(ValueError, 'Size mismatch'):
            self.prepare('fp32')
        self.assertFalse((self.package / 'apps/blackboard/models').exists())
        self.assert_untouched()

    def test_existing_incomplete_tree_is_preserved(self):
        destination = self.package / 'apps/blackboard/models/texteller-int8'
        destination.mkdir(parents=True)
        (destination / 'custom.txt').write_bytes(b'preserve me')
        with self.assertRaisesRegex(ValueError, 'file set differs'):
            self.prepare()
        self.assertEqual((destination / 'custom.txt').read_bytes(), b'preserve me')
        self.opener.open.assert_not_called()
        self.assert_untouched()

    def test_existing_installed_file_is_rehashed(self):
        destination = self.prepare()
        changed = destination / 'config.json'
        changed.write_bytes(b'x' * changed.stat().st_size)
        self.opener.open.reset_mock()
        with self.assertRaisesRegex(ValueError, 'SHA-256 mismatch'):
            self.prepare()
        self.opener.open.assert_not_called()

    def test_conflicting_variant_or_drawing_duplicate_is_not_deleted(self):
        for name in ('apps/blackboard/models/texteller', 'apps/drawing/models/texteller',
                     'apps/drawing/models/texteller-int8'):
            with self.subTest(name=name):
                path = self.package / name
                path.mkdir(parents=True)
                with self.assertRaisesRegex(ValueError, 'Conflicting model tree'):
                    self.prepare()
                self.assertTrue(path.is_dir())
                path.rmdir()
        self.opener.open.assert_not_called()

    def test_invalid_package_and_overlapping_cache(self):
        with self.assertRaisesRegex(ValueError, 'base package'):
            self.prepare(package=self.root / 'absent')
        self.cache = self.package / '.cache'
        with self.assertRaisesRegex(ValueError, 'must not overlap'):
            self.prepare()
        self.opener.open.assert_not_called()

    def test_symlink_package_component_is_rejected(self):
        outside = self.root / 'outside'
        outside.mkdir()
        link = self.package / 'apps/blackboard/models'
        try:
            link.symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest('Host does not permit symlink creation')
        with self.assertRaisesRegex(ValueError, 'Link/reparse'):
            self.prepare()
        self.assertEqual(list(outside.iterdir()), [])
        self.opener.open.assert_not_called()

    def test_reparse_attribute_is_rejected_without_symlink_privilege(self):
        real_stat = Path.lstat

        def reparse(path):
            info = real_stat(path)
            if path == self.package:
                return Mock(st_mode=info.st_mode, st_file_attributes=0x400)
            return info

        with patch.object(Path, 'lstat', reparse), self.assertRaisesRegex(ValueError, 'Link/reparse'):
            self.prepare()
        self.opener.open.assert_not_called()

    def test_lock_rejects_unpinned_urls_sources_and_missing_legal_files(self):
        original = copy.deepcopy(self.lock)
        for change in ('http', 'main', 'local', 'missing', 'size', 'hash'):
            self.lock = copy.deepcopy(original)
            entry = self.lock['fp32']['files']['config.json']
            if change == 'http':
                entry['url'] = entry['url'].replace('https:', 'http:')
            elif change == 'main':
                entry['url'] = entry['url'].replace(models.REVISION, 'main')
            elif change == 'local':
                entry['source'] = '../ignored/config.json'
            elif change == 'missing':
                del self.lock['fp32']['files']['UPSTREAM-MODEL-CARD.md']
            elif change == 'size':
                entry['size'] = -1
            else:
                entry['sha256'] = 'bad'
            self.save_lock()
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.prepare('fp32')
        self.opener.open.assert_not_called()

    def test_json_duplicate_key_is_rejected(self):
        self.lock_path.write_text('{"schema":1,"schema":1}', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'Duplicate JSON key'):
            self.prepare()

    def test_download_redirects_cannot_downgrade_https(self):
        handler = models.HTTPSRedirect()
        request = urllib.request.Request(models.INT8_URL)
        for url in ('http://example.com/model', 'file:///secret', 'https://user:pass@example.com/x'):
            with self.subTest(url=url), self.assertRaises(ValueError):
                handler.redirect_request(request, io.BytesIO(), 302, 'Found', HTTPMessage(), url)
        redirected = handler.redirect_request(request, io.BytesIO(), 302, 'Found',
                                              HTTPMessage(), 'https://cdn.example.com/model')
        assert redirected is not None
        self.assertEqual(redirected.full_url, 'https://cdn.example.com/model')

    def test_download_stream_limits_status_encoding_and_deadline(self):
        url = models.INT8_URL
        for response in (Response(self.archive[:-1], url, {}),
                         Response(self.archive + b'x', url, {}),
                         Response(self.archive, 'http://example.com/model'),
                         Response(self.archive, url, {'Content-Encoding': 'gzip'})):
            self.opener.open.side_effect = None
            self.opener.open.return_value = response
            self.assert_failed_cleanly()
        response = Response(self.archive, url)
        response.status = 206
        self.opener.open.return_value = response
        self.assert_failed_cleanly('HTTP status')
        self.opener.open.return_value = Response(self.archive, url)
        with patch.object(models.time, 'monotonic', side_effect=[0, models.DOWNLOAD_SECONDS + 1]):
            self.assert_failed_cleanly('download failed')

    def test_cli_dispatch_and_nonzero_failure(self):
        with patch.object(models, 'prepare', return_value=self.package) as prepare, \
                patch('sys.stdout', new_callable=io.StringIO):
            self.assertEqual(models.main(['--variant', 'int8', '--package', str(self.package),
                                          '--cache', str(self.cache)]), 0)
            prepare.assert_called_once_with('int8', self.package, self.cache, int8_archive=None)
        with patch.object(models, 'prepare', side_effect=ValueError('bad input')), \
                patch('sys.stderr', new_callable=io.StringIO) as stderr:
            self.assertEqual(models.main(['--variant', 'fp32', '--package', str(self.package),
                                          '--cache', str(self.cache)]), 1)
            self.assertIn('bad input', stderr.getvalue())


if __name__ == '__main__':
    unittest.main()
