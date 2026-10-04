"""Offline tests: no source archive downloads, SDK, or executable runtime needed."""
import copy
import hashlib
import io
import json
import struct
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

if __package__:
    from . import runtime_sources as sources
else:
    import runtime_sources as sources


def sha(data):
    return hashlib.sha256(data).hexdigest()


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runtime = self.root / 'runtime'
        (self.runtime / 'etc').mkdir(parents=True)
        (self.runtime / 'etc/package-versions.txt').write_bytes(b'bash 5.3.015-2\ngrep 1~3.0-7\nbash 5.3.015-2\n')
        (self.runtime / 'sh.exe').write_bytes(b'fixture-not-executable')
        self.manifest = sources.inventory(self.runtime)
        self.archive = self.root / 'source.zip'
        contents = {'src/code.c': b'int x;', 'src/COPYING': b'fixture license',
                    'src/fix.patch': b'fixture patch', 'src/PKGBUILD': b'fixture build script'}
        with zipfile.ZipFile(self.archive, 'w') as archive:
            for name, data in contents.items():
                archive.writestr(name, data)
        self.manifest['artifacts'] = [{
            'id': 'fixture-source', 'path': 'source.zip',
            'url': 'https://example.org/fixture-source.zip',
            'hash_evidence_url': 'https://example.org/fixture-metadata',
            **sources.file_record(self.archive),
            'members': [{'path': name, 'sha256': sha(data), 'role': role}
                        for (name, data), role in zip(contents.items(),
                                                    ('source', 'license', 'patches', 'build-scripts'))]}]
        for package in self.manifest['packages']:
            package['artifact_ids'] = ['fixture-source']
            package['mapping_evidence'] = 'https://example.org/fixture-package-mapping'

    def verify(self, manifest=None):
        return sources.verify(manifest or self.manifest, self.runtime, self.root)

    def pe_fixture(self, signed=False, pe64=False):
        raw = bytearray(1024)
        raw[:2] = b'MZ'
        struct.pack_into('<I', raw, 0x3c, 0x80)
        raw[0x80:0x84] = b'PE\0\0'
        struct.pack_into('<H', raw, 0x86, 1)
        size = 240 if pe64 else 224
        struct.pack_into('<H', raw, 0x94, size)
        opt = 0x98
        struct.pack_into('<H', raw, opt, 0x20b if pe64 else 0x10b)
        struct.pack_into('<I', raw, opt + 60, 512)
        directory = opt + (112 if pe64 else 96)
        struct.pack_into('<I', raw, directory - 4, 16)
        section = opt + size
        struct.pack_into('<II', raw, section + 16, 512, 512)
        raw[512:520] = b'CODEDATA'
        if signed:
            struct.pack_into('<I', raw, opt + 64, 12345)
            struct.pack_into('<II', raw, directory + 32, len(raw), 16)
            raw.extend(struct.pack('<IHH', 16, 0x200, 2) + b'fakecert')
        return raw

    def test_pe_signing_only_change_does_not_hide_code_or_resources(self):
        for pe64 in (False, True):
            original = self.pe_fixture(pe64=pe64)
            signed = self.pe_fixture(signed=True, pe64=pe64)
            one = sources.pe_signing_content_record(original)
            two = sources.pe_signing_content_record(signed)
            self.assertEqual(one['sha256'], two['sha256'])
            self.assertFalse(two['signature_verified'])
            for offset in (512, 600, 900, 0x88):
                altered = bytearray(signed)
                altered[offset] ^= 1
                self.assertNotEqual(one['sha256'], sources.pe_signing_content_record(altered)['sha256'])
            self.assertNotEqual(one['sha256'], sources.pe_signing_content_record(original + b'overlay')['sha256'])

    def test_pe_signing_rejects_unsafe_certificate_bounds(self):
        good = self.pe_fixture(signed=True)
        for offset, value in ((0x118, 512), (0x118, 1025), (0x11c, 8), (1024, 1000)):
            bad = bytearray(good)
            struct.pack_into('<I', bad, offset, value)
            with self.assertRaises(ValueError):
                sources.pe_signing_content_record(bad)
        for raw in (b'not PE', good[:200], good + b'extra'):
            with self.assertRaises(ValueError):
                sources.pe_signing_content_record(raw)

    def test_notice_delivery_binds_all_runtime_bytes_and_preserves_blocker(self):
        state, _ = sources.snapshot(self.runtime)
        notice = self.root/'NOTICE.txt'
        notice.write_text('upstream notice')
        data = {'runtime_snapshot_sha256': hashlib.sha256(json.dumps(
                    state, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                'notice_files': [{'path': notice.name, **sources.file_record(notice)}],
                'files': [{'path': 'sh.exe', **sources.file_record(self.runtime/'sh.exe'),
                           'notice_files': [notice.name], 'packages': [{'id': 'fixture', 'version': '1'}]}],
                'unmapped_executable_files': [], 'blockers': [{'id': 'restricted-license'}]}
        result = sources.verify_notice_delivery(data, self.runtime, self.root)
        self.assertTrue(result['integrity_ok'])
        self.assertEqual(result['blockers'], data['blockers'])
        self.assertFalse(result['release_ready'])
        self.assertFalse(result['approval'])
        # An unrelated runtime addition still invalidates the source/notice scope.
        extra = self.runtime/'new-config'
        extra.write_bytes(b'new')
        self.assertFalse(sources.verify_notice_delivery(data, self.runtime, self.root)['integrity_ok'])
        extra.unlink()
        notice.write_text('changed')
        self.assertFalse(sources.verify_notice_delivery(data, self.runtime, self.root)['integrity_ok'])
        notice.write_text('upstream notice')
        data['files'][0]['notice_files'] = ['missing.txt']
        self.assertFalse(sources.verify_notice_delivery(data, self.runtime, self.root)['integrity_ok'])

    def test_integrity_never_approves(self):
        result = self.verify()
        self.assertTrue(result['integrity_ok'], result)
        self.assertFalse(result['approval'])
        self.assertFalse(result['corresponding_source_complete'])

    def test_identical_duplicates_and_epoch_preserved(self):
        self.assertEqual(sources.parse_versions(b'a 1~3.0-7\na 1~3.0-7\nb 2:1.0-1\n'),
                         {'a': '1~3.0-7', 'b': '2:1.0-1'})

    def test_bad_package_lists(self):
        for raw in (b'', b'a 1\na 2', b'a 1 extra', b'../a 1', b'a https://host'):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                sources.parse_versions(raw)

    def test_runtime_changed_added_removed(self):
        for mode in ('changed', 'added', 'removed'):
            path = self.runtime / 'sh.exe'
            path.write_bytes(b'fixture-not-executable')
            extra = self.runtime / 'extra'
            if extra.exists():
                extra.unlink()
            if mode == 'changed':
                path.write_bytes(b'changed')
            elif mode == 'added':
                extra.write_bytes(b'added')
            else:
                path.unlink()
            with self.subTest(mode=mode):
                self.assertFalse(self.verify()['integrity_ok'])

    def test_package_missing_extra_mismatch_duplicate(self):
        for mode in ('missing', 'extra', 'version', 'duplicate'):
            manifest = copy.deepcopy(self.manifest)
            if mode == 'missing':
                manifest['packages'].pop()
            elif mode == 'extra':
                manifest['packages'].append({**manifest['packages'][0], 'name': 'extra'})
            elif mode == 'version':
                manifest['packages'][0]['version'] = 'different'
            else:
                manifest['packages'].append(manifest['packages'][0])
            with self.subTest(mode=mode):
                self.assertFalse(self.verify(manifest)['integrity_ok'])

    def test_manifest_inventory_starts_unresolved(self):
        result = self.verify(sources.inventory(self.runtime))
        self.assertFalse(result['integrity_ok'])
        self.assertFalse(result['approval'])

    def test_archive_tampered_even_if_evidence_files_unchanged(self):
        with zipfile.ZipFile(self.archive, 'a') as archive:
            archive.writestr('unlisted-file', b'tampered')
        self.assertFalse(self.verify()['integrity_ok'])

    def test_artifact_required_fields(self):
        changes = [('sha256', ''), ('size', True), ('url', 'http://example.org/source'),
                   ('hash_evidence_url', ''), ('path', '../source.zip'), ('members', [])]
        for field, value in changes:
            manifest = copy.deepcopy(self.manifest)
            manifest['artifacts'][0][field] = value
            with self.subTest(field=field):
                self.assertFalse(self.verify(manifest)['integrity_ok'])

    def test_member_hash_and_roles(self):
        for field, value in [('sha256', 'a' * 64), ('path', 'absent'), ('role', 'unknown')]:
            manifest = copy.deepcopy(self.manifest)
            manifest['artifacts'][0]['members'][0][field] = value
            with self.subTest(field=field):
                self.assertFalse(self.verify(manifest)['integrity_ok'])
        self.manifest['artifacts'][0]['members'].pop()
        self.assertFalse(self.verify()['integrity_ok'])

    def test_patch_waiver_explicit(self):
        self.manifest['artifacts'][0]['members'].pop(2)
        self.assertFalse(self.verify()['integrity_ok'])
        for package in self.manifest['packages']:
            package['patches_not_required_reason'] = 'Fixture contains unmodified source; test only'
        self.assertTrue(self.verify()['integrity_ok'])

    def test_unknown_or_duplicate_artifact_reference(self):
        for refs in ([], ['missing'], ['fixture-source', 'fixture-source']):
            self.manifest['packages'][0]['artifact_ids'] = refs
            self.assertFalse(self.verify()['integrity_ok'])

    def test_inspect_hashes_all_regular_members(self):
        result = sources.inspect_archive(self.archive)
        self.assertEqual(result['sha256'], sha(self.archive.read_bytes()))
        self.assertEqual(len(result['members']), 4)
        self.assertTrue(all(m['sha256'] for m in result['members']))
        self.assertFalse(result['nested_archives_inspected'])

    def test_budget(self):
        with self.assertRaisesRegex(ValueError, 'budget'):
            sources.inspect_archive(self.archive, max_content=1)

    def test_bad_archive_paths(self):
        for name in ('../escape', '/absolute', 'C:/file', 'file:ads', 'CON.txt', 'x\\y', 'trailing.', 'a//b'):
            path = self.root / 'bad.zip'
            with zipfile.ZipFile(path, 'w') as archive:
                # ZipInfo's constructor normalizes backslashes on Windows;
                # set the raw stored name afterwards to exercise hostile input.
                info = zipfile.ZipInfo('placeholder')
                info.filename = name
                archive.writestr(info, b'x')
            with self.subTest(name=name), self.assertRaises(ValueError):
                sources.inspect_archive(path)
        self.assertFalse((self.root / 'escape').exists())

    def test_duplicate_case_alias(self):
        with zipfile.ZipFile(self.archive, 'a') as archive:
            archive.writestr('SRC/COPYING', b'x')
        with self.assertRaisesRegex(ValueError, 'alias'):
            sources.inspect_archive(self.archive)

    def test_tar_links_recorded_not_followed(self):
        path = self.root / 'source.tar.gz'
        with tarfile.open(path, 'w:gz') as archive:
            info = tarfile.TarInfo('src/code')
            info.size = 4
            archive.addfile(info, io.BytesIO(b'code'))
            link = tarfile.TarInfo('src/link')
            link.type = tarfile.SYMTYPE
            link.linkname = '../../outside'
            archive.addfile(link)
        result = sources.inspect_archive(path)
        self.assertEqual(result['members'][0]['sha256'], sha(b'code'))
        self.assertEqual(result['members'][1]['target'], '../../outside')
        self.assertFalse((self.root / 'src').exists())

    def test_symlink_runtime_rejected(self):
        path = self.runtime / 'link'
        try:
            path.symlink_to(self.archive)
        except OSError:
            self.skipTest('OS does not permit test symlinks')
        with self.assertRaises(ValueError):
            sources.inventory(self.runtime)

    def test_discovery_does_not_map_candidates(self):
        calls = []
        def fetch(url):
            calls.append(url)
            if '.versions.json' in url:
                data = {'5.3.015-2': 'fixed-tag'}
            else:
                data = {'tag_name': 'fixed-tag', 'draft': False, 'assets': [
                    {'name': 'bash-5.3.015-2.src.tar.gz', 'size': 123, 'id': 1,
                     'digest': 'sha256:' + 'a' * 64,
                     'browser_download_url': 'https://example.org/source.tar.gz'}]}
            return {'url': url, 'data': data, 'response_sha256': 'b' * 64}
        result = sources.discover(sources.inventory(self.runtime), fetch)
        self.assertEqual(len(result['packages'][0]['candidates']), 1)
        self.assertEqual(result['packages'][0]['artifact_ids'], [])
        self.assertEqual(result['packages'][1]['discovery']['status'], 'unresolved')
        self.assertEqual(len(calls), 3)
        self.assertFalse(result['approval'])

    def test_network_failure_is_unresolved(self):
        fetch = mock.Mock(side_effect=OSError('offline'))
        result = sources.discover(self.manifest, fetch)
        self.assertTrue(all(p['discovery']['status'] == 'unresolved' for p in result['packages']))

    def test_duplicate_json_keys_rejected(self):
        path = self.root / 'bad.json'
        path.write_text('{"schema_version": 1, "schema_version": 2}')
        with self.assertRaises(ValueError):
            sources.load_json(path)

    def test_output_not_overwritten(self):
        path = self.root / 'output.json'
        sources.write_json(path, {'first': True})
        with self.assertRaises(FileExistsError):
            sources.write_json(path, {})
        self.assertEqual(json.loads(path.read_text()), {'first': True})

    def asset(self, data=b'source'):
        return {'name': 'source.tar.gz', 'url': 'https://github.com/git-for-windows/pacman-repo/releases/download/tag/source.tar.gz',
                'evidence_url': sources.API_ROOT + 'tag', 'size': len(data), 'sha256': sha(data)}

    def test_acquire_pin_cache_and_budget(self):
        asset = self.asset()
        def get(url, path, limit, seconds):
            path.write_bytes(b'source')
            return {**sources.file_record(path), 'bytes_received': 6}
        with mock.patch.object(sources, 'bounded_get', side_effect=get) as fetch:
            result = sources.acquire([asset], self.root, 70000)
            self.assertEqual(result['assets'][0]['status'], 'verified-download')
            cached = sources.acquire([asset], self.root, 0)
            self.assertEqual(cached['assets'][0]['status'], 'verified-cache')
            self.assertEqual(fetch.call_count, 1)
        other = {**asset, 'name': 'other.tar.gz'}
        result = sources.acquire([other], self.root, 6)
        self.assertEqual(result['assets'][0]['status'], 'budget-skipped')
        self.assertFalse(result['approval'])

    def test_acquire_bad_hash_and_failure_charged(self):
        asset = self.asset()
        def get(url, path, limit, seconds):
            path.write_bytes(b'wrong!')
            return {**sources.file_record(path), 'bytes_received': 6}
        with mock.patch.object(sources, 'bounded_get', side_effect=get):
            result = sources.acquire([asset], self.root, 70000)
        self.assertEqual(result['assets'][0]['status'], 'download-failed')
        self.assertEqual(result['reserved_bytes'], 65542)
        self.assertFalse((self.root / asset['name']).exists())
        self.assertFalse((self.root / (asset['name'] + '.partial')).exists())

    def test_acquire_preserves_existing_partial_and_bad_cache(self):
        asset = self.asset()
        partial = self.root / (asset['name'] + '.partial')
        partial.write_bytes(b'user work')
        with self.assertRaises(ValueError):
            sources.acquire([asset], self.root, 70000)
        self.assertEqual(partial.read_bytes(), b'user work')
        partial.unlink()
        (self.root / asset['name']).write_bytes(b'wrong')
        with self.assertRaises(ValueError):
            sources.acquire([asset], self.root, 70000)

    def test_download_rejects_unofficial_and_bad_range(self):
        for url in ('http://github.com/a', 'https://evil.example/a', 'https://github.com.evil.example/a',
                    'https://user:pass@github.com/a', 'https://github.com:444/a'):
            with self.subTest(url=url), self.assertRaises(ValueError):
                sources.official_url(url)
        with self.assertRaises(ValueError):
            sources.bounded_get('https://github.com/a', self.root/'download', 100, 121)
        with self.assertRaises(ValueError):
            sources.ranged_get(self.asset(), self.root/'download', 1, 8)

    def test_transport_redirect_allowlist_and_range_validation(self):
        def redirect(command, **kwargs):
            Path(command[command.index('--output') + 1]).write_bytes(b'')
            Path(command[command.index('--dump-header') + 1]).write_text('Location: https://evil.example/file\n')
            return sources.subprocess.CompletedProcess(command, 0, b'302', b'')
        with mock.patch.object(sources.subprocess, 'run', side_effect=redirect) as run:
            with self.assertRaises(ValueError):
                sources.bounded_get('https://github.com/a', self.root/'download', 100, 1)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(run.call_args.args[0][1], '-q')
        def wrong_range(command, **kwargs):
            Path(command[command.index('--output') + 1]).write_bytes(b'abc')
            Path(command[command.index('--dump-header') + 1]).write_text('Content-Range: bytes 1-3/6\n')
            return sources.subprocess.CompletedProcess(command, 0, b'206', b'')
        with (mock.patch.object(sources.subprocess, 'run', side_effect=wrong_range),
              self.assertRaisesRegex(ValueError, 'range response')):
            sources.bounded_get('https://github.com/a', self.root/'download', 100, 1, (0, 2, 6))

    def test_nested_srcinfo_actual_content_not_urls(self):
        inner = io.BytesIO()
        with zipfile.ZipFile(inner, 'w') as archive:
            archive.writestr('src/code.c', b'int x;')
            archive.writestr('src/LICENSE', b'license')
        raw = inner.getvalue()
        path = self.root/'outer.zip'
        info = 'pkgname = bash\npkgver = 5.3.015\npkgrel = 2\nsource = https://example.org/input.zip\nsha256sums = ' + sha(raw)
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('pkg/.SRCINFO', info)
            archive.writestr('pkg/PKGBUILD', 'exit 99 # never executed')
            archive.writestr('pkg/input.zip', raw)
        result = sources.inspect_source_tree(path)
        self.assertTrue(result['nested_archives_inspected'])
        self.assertTrue(result['srcinfo_checks'][0]['inputs'][0]['sha256_matches'])
        self.assertEqual(result['nested'][0]['inspection']['members'][0]['sha256'], sha(b'int x;'))
        self.assertFalse(result['corresponding_source_complete'])
        with self.assertRaisesRegex(ValueError, 'budget'):
            sources.inspect_source_tree(path, max_content=1)
        limited = sources.inspect_source_tree(path, max_depth=0)
        self.assertFalse(limited['nested_archives_inspected'])
        self.assertEqual(limited['nested_archive_gaps'][0]['member_path'], 'pkg/input.zip')
        self.assertEqual(limited['nested_archive_gaps'][0]['sha256'], sha(raw))
        self.assertEqual(limited['nested_archive_gaps'][0]['archive_chain'], [])

    def test_nested_gap_index_preserves_chain_hash_size_and_reason(self):
        inner = io.BytesIO()
        with zipfile.ZipFile(inner, 'w') as archive:
            archive.writestr('tests/fixture.tar', b'fixture bytes')
            archive.writestr('broken.tar.gz', b'not an archive')
        path = self.root/'gaps.zip'
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('source.zip', inner.getvalue())
        result = sources.inspect_source_tree(path)
        self.assertFalse(result['nested_archives_inspected'])
        gaps = {g['member_path']: g for g in result['nested_archive_gaps']}
        self.assertEqual(set(gaps), {'tests/fixture.tar', 'broken.tar.gz'})
        for name, data in [('tests/fixture.tar', b'fixture bytes'),
                           ('broken.tar.gz', b'not an archive')]:
            gap = gaps[name]
            self.assertEqual(gap['archive_chain'], ['source.zip'])
            self.assertEqual(gap['sha256'], sha(data))
            self.assertEqual(gap['size'], len(data))
            self.assertTrue(gap['bytes_retained_in_parent'])
            self.assertFalse(gap['contents_indexed'])
        self.assertIn('test-fixture', gaps['tests/fixture.tar']['reason'])
        self.assertIn('inspection failed', gaps['broken.tar.gz']['reason'])

    def test_opaque_containers_are_explicit_index_gaps(self):
        path = self.root/'opaque.zip'
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('source.tar.lz', b'lzip bytes')
            archive.writestr('repo/objects/pack/source.pack', b'git pack bytes')
        result = sources.inspect_source_tree(path)
        self.assertFalse(result['nested_archives_inspected'])
        self.assertEqual(len(result['nested_archive_gaps']), 2)
        for gap in result['nested_archive_gaps']:
            self.assertIn('separate inspection', gap['reason'])
            self.assertFalse(gap['contents_indexed'])
            self.assertTrue(gap['bytes_retained_in_parent'])
            self.assertEqual(gap['archive_chain'], [])

    def test_shared_mingw_source_requires_recipe_arch_version_and_split(self):
        fields = {'pkgbase': ['mingw-w64-winpthreads'], 'pkgver': ['14.0'],
                  'pkgrel': ['1'], 'pkgname': ['mingw-w64-ucrt-x86_64-libwinpthread']}
        recipe = ('_realname=winpthreads\n'
                  'mingw_arch=(\'mingw64\' \'ucrt64\')\n'
                  'pkgname=("${MINGW_PACKAGE_PREFIX}-${_realname}"\n'
                  '         "${MINGW_PACKAGE_PREFIX}-libwinpthread")\n')
        package = 'mingw-w64-x86_64-libwinpthread'
        result = sources.mingw_source_mapping(fields, recipe, package, '14.0-1')
        self.assertTrue(result['matched'])
        self.assertFalse(result['binary_equivalence_claimed'])
        self.assertFalse(result['shell_executed'])
        for text, name, version in [
                (recipe.replace("'mingw64'", "'clang64'"), package, '14.0-1'),
                (recipe, package, '14.0-2'),
                (recipe.replace('-libwinpthread', '-other'), package, '14.0-1'),
                ('pkgbase=mingw-w64-winpthreads', package, '14.0-1'),
                (recipe, 'mingw-w64-x86_64-winpthreads', '14.0-1')]:
            self.assertFalse(sources.mingw_source_mapping(fields, text, name, version)['matched'])

    def test_unquoted_shared_template_and_conditional_not_evaluated(self):
        fields = {'pkgver': ['1'], 'pkgrel': ['2'],
                  'pkgname': ['mingw-w64-ucrt-x86_64-libiconv']}
        recipe = ('_realname=libiconv\nmingw_arch=(mingw64 ucrt64)\n'
                  'pkgname=(${MINGW_PACKAGE_PREFIX}-${_realname})\n')
        package = 'mingw-w64-x86_64-libiconv'
        self.assertTrue(sources.mingw_source_mapping(fields, recipe, package, '1-2')['matched'])
        conditional = recipe.replace('pkgname=(', 'pkgname=($(echo ')
        self.assertFalse(sources.mingw_source_mapping(fields, conditional, package, '1-2')['matched'])

    def test_fixture_is_retained_not_a_delivery_blocker(self):
        path = self.root/'fixtures.zip'
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('tests/hostile.tar', b'not to be expanded')
        result = sources.inspect_source_tree(path)
        self.assertFalse(result['nested_archives_inspected'])
        self.assertEqual(result['source_delivery_review_gaps'], [])
        self.assertTrue(result['nested_archive_gaps'][0]['bytes_retained_in_parent'])
        # A declared package input is not waived merely for residing in tests/.
        declared = self.root/'declared.zip'
        with zipfile.ZipFile(declared, 'w') as archive:
            archive.writestr('.SRCINFO', 'source = tests/hostile.tar\n')
            archive.writestr('tests/hostile.tar', b'not an archive')
        self.assertTrue(sources.inspect_source_tree(declared)['source_delivery_review_gaps'])

    def test_nested_zip_links_recorded_not_followed(self):
        path = self.root/'link.zip'
        with zipfile.ZipFile(path, 'w') as archive:
            info = zipfile.ZipInfo('link')
            info.create_system = 3
            info.external_attr = (0o120777 << 16)
            archive.writestr(info, '../../outside')
        with self.assertRaises(ValueError):
            sources.inspect_archive(path)
        result = sources.inspect_source_tree(path)
        self.assertEqual(result['members'][0]['target'], '../../outside')
        self.assertFalse((self.root/'outside').exists())

    def test_bundle_reproducible_and_tamper_rejected(self):
        manifest = self.root/'companion.json'
        sources.write_json(manifest, {'approval': False, 'corresponding_source_complete': False,
            'artifacts': [{'path': self.archive.name, **sources.file_record(self.archive)}]})
        first, second = self.root/'first.zip', self.root/'second.zip'
        one = sources.bundle_sources(manifest, self.root, first)
        two = sources.bundle_sources(manifest, self.root, second)
        self.assertEqual(one['sha256'], two['sha256'])
        self.assertFalse(one['published'])
        with zipfile.ZipFile(first) as archive:
            self.assertEqual(archive.read('archives/source.zip'), self.archive.read_bytes())
        self.archive.write_bytes(b'tamper')
        with self.assertRaisesRegex(ValueError, 'differs'):
            sources.bundle_sources(manifest, self.root, self.root/'third.zip')
        self.assertFalse((self.root/'third.zip').exists())

    def test_complete_companion_requires_scoped_review_not_approval(self):
        evidence = self.root/'review.json'
        sources.write_json(evidence, {'scope': 'synthetic source delivery test'})
        data = {'approval': False, 'corresponding_source_complete': True,
                'artifacts': [{'id': 'source', 'path': self.archive.name,
                               **sources.file_record(self.archive)}],
                'packages': [{'name': 'bash', 'version': '1-1', 'artifact_ids': ['source'],
                              'mapping_basis': 'fixture recipe'}],
                'evidence_files': [{'path': evidence.name, **sources.file_record(evidence)}],
                'delivery_review': {'scope': 'synthetic test only',
                                    'package_versions_text': 'bash 1-1\n',
                                    'unresolved_required_inputs': [],
                                    'evidence_paths': [evidence.name]}}
        manifest = self.root/'complete.json'
        sources.write_json(manifest, data)
        result = sources.bundle_sources(manifest, self.root, self.root/'complete.zip')
        self.assertTrue(result['corresponding_source_complete'])
        self.assertFalse(result['approval'])
        self.assertFalse(result['release_ready'])
        data['delivery_review']['unresolved_required_inputs'] = ['missing patch']
        bad = self.root/'bad-complete.json'
        sources.write_json(bad, data)
        with self.assertRaisesRegex(ValueError, 'scoped delivery review'):
            sources.bundle_sources(bad, self.root, self.root/'bad.zip')
        self.assertFalse((self.root/'bad.zip').exists())

    def test_cli_returns_failure_for_unresolved_manifest(self):
        manifest = self.root / 'manifest.json'
        sources.write_json(manifest, sources.inventory(self.runtime))
        report = self.root / 'report.json'
        code = sources.main(['verify', '--runtime', str(self.runtime), '--manifest', str(manifest),
                             '--archives', str(self.root), '--output', str(report)])
        self.assertEqual(code, 1)
        self.assertFalse(json.loads(report.read_text())['approval'])


if __name__ == '__main__':
    unittest.main()
