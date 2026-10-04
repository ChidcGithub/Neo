"""通过合成报告与临时载荷测试发行校验，绝不执行 PE 文件。"""
from contextlib import redirect_stdout
import io
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from tools.check_release import (CRT_NAMES, CRT_MIN_VERSION, CRT_DOWNLOAD_URL, MODELS,
                                 check_package, inspect_native_exports, inspect_pe, main,
                                 parse_pe_report, validate_redist_dir, validate_hi_neo, verify_distribution)


def native_fixture(symbol=b"unrelated", forwarder=False):
    """Minimal inert PE32+ export table, never executable test code."""
    data = bytearray(0x800)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0x8664, 1, 0, 0, 0, 240, 0x22)
    struct.pack_into("<H", data, 0x98, 0x20B)
    struct.pack_into("<I", data, 0x98 + 108, 16)
    struct.pack_into("<II", data, 0x98 + 112, 0x1100, 0x100)
    section = 0x98 + 240
    data[section:section + 8] = b".text\0\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x600, 0x1000, 0x600, 0x200)
    struct.pack_into("<I", data, section + 36, 0x60000020)
    struct.pack_into("<IIHHIIIIIII", data, 0x300, 0, 0, 0, 0, 0, 1, 1, 1, 0x1140, 0x1144, 0x1148)
    struct.pack_into("<IIH", data, 0x340, 0x1180 if forwarder else 0x1020, 0x1200, 0)
    data[0x380:0x38D] = b"other.espeak\0"
    data[0x400:0x401 + len(symbol)] = symbol + b"\0"
    return bytes(data)


class NativeExportReleaseTests(unittest.TestCase):
    def test_risk_markers_rejected_before_dumpbin_without_executing_image(self):
        for symbol in (b"espeak_Initialize", b"_espeak_ng_Initialize",
                       b"?phonemize_eSpeak@piper@@signature", b"?phonemize_codepoints@piper@@signature"):
            for forwarder in (False, True):
                with self.subTest(symbol=symbol, forwarder=forwarder), tempfile.TemporaryDirectory() as td:
                    binary = Path(td) / "synthetic.exe"
                    data = native_fixture(symbol, forwarder)
                    binary.write_bytes(data)
                    with patch("tools.check_release.subprocess.run") as run:
                        with self.assertRaisesRegex(ValueError, "Distribution BLOCKED.*eSpeak/Piper"):
                            inspect_pe(binary, "unused")
                    run.assert_not_called()
                    self.assertEqual(binary.read_bytes(), data)

    def test_malformed_pe_fails_closed_before_dumpbin(self):
        with tempfile.TemporaryDirectory() as td:
            binary = Path(td) / "synthetic.exe"
            for data in (b"inert", native_fixture()[:0x250]):
                binary.write_bytes(data)
                with patch("tools.check_release.subprocess.run") as run, self.assertRaises(ValueError):
                    inspect_pe(binary, "unused")
                run.assert_not_called()

    def test_no_markers_or_exports_never_claim_clearance(self):
        no_exports = bytearray(native_fixture())
        struct.pack_into("<II", no_exports, 0x98 + 112, 0, 0)
        with tempfile.TemporaryDirectory() as td:
            binary = Path(td) / "synthetic.exe"
            for data in (native_fixture(b"RustNamedPipeRead"), bytes(no_exports)):
                binary.write_bytes(data)
                output = io.StringIO()
                with redirect_stdout(output):
                    inspect_native_exports(binary)
                self.assertIn("absence is inconclusive, NOT legal clearance", output.getvalue())
                result = subprocess.CompletedProcess([], 0, " 8664 machine (x64)\n KERNEL32.dll\n")
                with patch("tools.check_release.subprocess.run", return_value=result) as run:
                    self.assertEqual(inspect_pe(binary, "synthetic-dumpbin"), {"kernel32.dll"})
                self.assertEqual(run.call_args.args[0][0], "synthetic-dumpbin")
                self.assertEqual(run.call_args.args[0][-1], str(binary))

    @patch('tools.check_release.verify_distribution')
    def test_package_path_enforces_real_export_scan(self, filtered):
        with tempfile.TemporaryDirectory() as td:
            package = ReleaseTests().make_package(Path(td))
            (package / "neo.exe").write_bytes(native_fixture(b"espeak_Synth"))
            with patch("tools.check_release.subprocess.run") as run, \
                    patch("tools.check_release.shutil.copy2") as copy:
                with self.assertRaisesRegex(ValueError, "Distribution BLOCKED"):
                    check_package(package, "unused")
            run.assert_not_called()
            copy.assert_not_called()


class FilteredPayloadTests(unittest.TestCase):
    def setUp(self):
        from tools import prepare_runtime_distribution as prep
        self.prep = prep
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runtime = self.root / 'runtime/gitbash'
        contents = {'etc/gitconfig': b'[core]\n autocrlf = true\n',
                    'etc/package-versions.txt': b'git 2.55.0.5\n',
                    **{f'usr/file-{i}': f'inert-{i}'.encode() for i in range(308)}}
        for name, data in contents.items():
            path = self.runtime / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        state, _ = prep.snapshot(self.runtime)
        original = json.loads(json.dumps(state))
        for row in original['files']:
            if row['path'].startswith('etc/'):
                row['sha256'] = '0' * 64
        policy = {'source_files': original['files'], 'remove_files': [], 'blocked_sha256': []}
        policy_path = self.root / 'gitbash-distribution-policy.json'
        policy_path.write_text(json.dumps(policy))
        (self.root / 'MODIFICATIONS.md').write_text('Synthetic modification notice; NOT approval')
        before = prep.records(original['files'])
        self.report = {'schema': 'neo-gitbash-distribution-manifest-v1',
                       'policy': prep.file_record(policy_path), 'runtime': state,
                       'original_runtime': original, 'removed': [],
                       'runtime_files_sha256': prep.digest_json(state['files']),
                       'modifications': [{'path': n, 'before': before[n], 'after': r}
                                         for n, r in prep.records(state['files']).items() if r != before[n]]}
        (self.root / 'MANIFEST.json').write_text(json.dumps(self.report))
        # Synthetic bytes exercise the real verifier, with a test-only policy/hash pin.
        for name, value in [('POLICY', policy_path), ('FILTERED_FILES_SHA256', self.report['runtime_files_sha256'])]:
            patcher = patch.object(prep, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(prep, 'load_policy', return_value=policy)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_exact_310_files_and_modifications_pass(self):
        self.assertEqual(verify_distribution(self.root), self.report)

    def test_extra_removed_metadata_missing_and_changed_bytes_rejected(self):
        for name, data in [('mingw64/bin/git-credential-manager.exe', b'GCM'),
                           ('.neo-version', b'v2.55.0.windows.5'),
                           ('etc/gitconfig', b'[credential]\n helper = manager\n'),
                           ('etc/package-versions.txt', b'git 9.99\n'),
                           ('usr/file-0', None)]:
            with self.subTest(name=name):
                path = self.runtime / name
                old = path.read_bytes() if path.exists() else None
                path.parent.mkdir(parents=True, exist_ok=True)
                if data is None:
                    path.unlink()
                else:
                    path.write_bytes(data)
                with self.assertRaises(ValueError):
                    verify_distribution(self.root)
                if old is None:
                    path.unlink()
                else:
                    path.write_bytes(old)

    def test_manifest_cannot_self_approve_tampered_runtime(self):
        (self.runtime / 'usr/file-0').write_bytes(b'tampered')
        state, _ = self.prep.snapshot(self.runtime)
        self.report.update(runtime=state, runtime_files_sha256=self.prep.digest_json(state['files']))
        (self.root / 'MANIFEST.json').write_text(json.dumps(self.report))
        with self.assertRaisesRegex(ValueError, 'exact reviewed 310'):
            verify_distribution(self.root)

    def test_markers_required_and_modifications_bound(self):
        for name in self.prep.DISTRIBUTION_DOCUMENTS:
            path = self.root / name
            data = path.read_bytes()
            path.unlink()
            with self.assertRaises(ValueError):
                verify_distribution(self.root)
            path.write_bytes(data)
        self.report['modifications'] = []
        (self.root / 'MANIFEST.json').write_text(json.dumps(self.report))
        with self.assertRaisesRegex(ValueError, 'modifications marker'):
            verify_distribution(self.root)

    def test_hi_neo_bytes_index_and_nonempty_mit(self):
        model = self.root / 'model.onnx'
        model.write_bytes(b'synthetic model')
        index = {'path': 'resources/models/wake/hi_neo.onnx', **self.prep.file_record(model),
                 'license': 'MIT', 'license_file': 'hi_neo-MIT.txt'}
        index_path = self.root / 'hi_neo-model.json'
        index_path.write_text(json.dumps(index))
        license_path = self.root / 'hi_neo-MIT.txt'
        license_path.write_text('Synthetic nonempty text, not actual license/approval')
        self.assertEqual(validate_hi_neo(model, self.root), index)
        model.write_bytes(b'changed model')
        with self.assertRaisesRegex(ValueError, 'bytes/license index mismatch'):
            validate_hi_neo(model, self.root)
        model.write_bytes(b'synthetic model')
        for key, value in [('sha256', '0' * 64), ('size', True), ('license', 'unknown'),
                           ('license_file', '../license.txt'), ('path', '../model.onnx')]:
            index_path.write_text(json.dumps({**index, key: value}))
            with self.assertRaises(ValueError):
                validate_hi_neo(model, self.root)
        index_path.write_text(json.dumps(index))
        license_path.write_text(' \n\t')
        with self.assertRaisesRegex(ValueError, 'MIT license text is empty'):
            validate_hi_neo(model, self.root)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        # PE/layout tests use inert files. Full distribution validation is tested separately.
        self.filtered = patch('tools.check_release.verify_distribution').start()
        self.addCleanup(patch.stopall)
    # 独立列出发行契约，避免 fixture 随校验清单一起漏掉新资源。
    payload_files = (
        "neo.exe", "runtime/onnx/onnxruntime.dll", *(f"resources/models/wake/{name}" for name in MODELS),
        "resources/models/stt/sense-voice/model.int8.onnx", "resources/models/stt/sense-voice/tokens.txt",
        "resources/models/stt/vad/silero_vad.onnx", "LICENSE", "NOTICE",
        "docs/licenses/cargo-notices.txt",
        "docs/licenses/models/hi_neo-model.json", "docs/licenses/models/hi_neo-MIT.txt",
        "resources/lang/zh-CN.lang", "resources/lang/en-US.lang",
    )
    bash_paths = ("runtime/gitbash/bin/bash.exe", "runtime/gitbash/usr/bin/bash.exe")

    def test_imports_include_delayed_crt_and_ignore_case(self):
        report = " 8664 machine (x64)\n    KERNEL32.dll\n    VCRUNTIME140.dll\n  delay load imports\n    MSVCP140_1.dll\n"
        self.assertEqual(parse_pe_report(report), {"kernel32.dll", "vcruntime140.dll", "msvcp140_1.dll"})

    def test_reject_wrong_arch_empty_imports_and_debug_runtime(self):
        for report in [" AA64 machine (ARM64)\n KERNEL32.dll", " 14C machine (x86)\n KERNEL32.dll",
                       " 8664 machine (x64)\n", " 8664 machine (x64)\n MSVCP140D.dll",
                       " 8664 machine (x64)\n MSVCR120.dll", " 8664 machine (x64)\n VCCORLIB140.dll",
                       " 8664 machine (x64)\n VCRUNTIME999.dll", "corrupt binary"]:
            with self.subTest(report=report), self.assertRaises(ValueError):
                parse_pe_report(report)

    def make_package(self, root, bash_path="runtime/gitbash/usr/bin/bash.exe"):
        package = root / "package"
        for name in (*self.payload_files, bash_path):
            path = package / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"inert")
        for language, translation in (("zh-CN", "你好，{name}"), ("en-US", "Hello, {name}")):
            (package / f"resources/lang/{language}.lang").write_text(
                json.dumps({"你好，{name}": translation}, ensure_ascii=False), encoding="utf-8")
        (package / 'docs/licenses/models/hi_neo-model.json').write_text(json.dumps({
            'path': 'resources/models/wake/hi_neo.onnx', 'sha256': hashlib.sha256(b'inert').hexdigest(),
            'size': 5, 'license': 'MIT', 'license_file': 'hi_neo-MIT.txt'}), encoding='utf-8')
        return package

    def make_redist(self, root):
        redist = root / "VC/Redist/MSVC/14.44.1/x64/Microsoft.VC143.CRT"
        redist.mkdir(parents=True)
        return redist

    def test_refuse_non_redist_sources(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            good = self.make_redist(root)
            self.assertEqual(validate_redist_dir(good), good.resolve())
            for bad in [root / "System32", root / "downloads", good.parent.parent / "x86/Microsoft.VC143.CRT",
                        root / "VC/Redist/MSVC/14.44.1/onecore/x64/Microsoft.VC143.CRT",
                        root / "VC/Redist/MSVC/14.44.1/debug_nonredist/x64/Microsoft.VC143.CRT"]:
                with self.subTest(path=bad), self.assertRaises(ValueError):
                    validate_redist_dir(bad)

    def test_missing_empty_or_directory_payload_fails_before_binary_audit(self):
        for name in (*self.payload_files, self.bash_paths[1]):
            for state in ("missing", "empty", "directory"):
                with self.subTest(path=name, state=state), tempfile.TemporaryDirectory() as td:
                    package = self.make_package(Path(td))
                    path = package / name
                    path.unlink()
                    if state == "empty":
                        path.touch()
                    elif state == "directory":
                        path.mkdir()
                    with patch("tools.check_release.inspect_pe") as audit, \
                            patch("tools.check_release.shutil.copy2") as copy:
                        with self.assertRaisesRegex(ValueError, "Required release resource missing/empty") as error:
                            check_package(package, "unused")
                        self.assertIn(str(path), str(error.exception))
                    audit.assert_not_called()
                    copy.assert_not_called()

    def test_invalid_language_catalog_fails_before_binary_audit(self):
        invalid = [b"not json", b"\xff", b"{}", b"[]", b"null", b'"text"',
                   b'{"key": null}', b'{"key": 1}', b'{"key": true}',
                   b'{"key": []}', b'{"key": {}}', b'{"": "value"}', b'{"  ": "value"}']
        for language in ("zh-CN", "en-US"):
            for data in invalid:
                with self.subTest(language=language, data=data), tempfile.TemporaryDirectory() as td:
                    package = self.make_package(Path(td))
                    path = package / f"resources/lang/{language}.lang"
                    path.write_bytes(data)
                    with patch("tools.check_release.inspect_pe") as audit, \
                            patch("tools.check_release.shutil.copy2") as copy:
                        with self.assertRaises(ValueError) as error:
                            check_package(package, "unused")
                        self.assertIn(str(path), str(error.exception))
                    audit.assert_not_called()
                    copy.assert_not_called()

    def test_language_key_mismatch_and_blank_english_fail(self):
        for catalog, message in [({"other": "Other"}, "keys differ"),
                                 ({"你好，{name}": ""}, "English translations must not be empty"),
                                 ({"你好，{name}": " \t\n"}, "English translations must not be empty")]:
            with self.subTest(catalog=catalog), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td))
                (package / "resources/lang/en-US.lang").write_text(json.dumps(catalog), encoding="utf-8")
                with patch("tools.check_release.inspect_pe") as audit:
                    with self.assertRaisesRegex(ValueError, message):
                        check_package(package, "unused")
                audit.assert_not_called()

    def test_unexpected_release_language_fails(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            (package / "resources/lang/fr-FR.lang").write_text('{"key": "value"}', encoding="utf-8")
            with patch("tools.check_release.inspect_pe") as audit:
                with self.assertRaisesRegex(ValueError, "Unsupported release language resources.*fr-FR"):
                    check_package(package, "unused")
            audit.assert_not_called()

    def test_language_key_order_does_not_matter(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            for language, catalog in (("zh-CN", {"设置": "设置", "取消": "取消"}),
                                      ("en-US", {"取消": "Cancel", "设置": "Settings"})):
                (package / f"resources/lang/{language}.lang").write_text(
                    json.dumps(catalog, ensure_ascii=False), encoding="utf-8")
            with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll"}):
                self.assertEqual(check_package(package, "unused"), set())

    def test_both_fetch_runtime_bash_layouts_are_accepted(self):
        for name in self.bash_paths:
            with self.subTest(path=name), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td), bash_path=name)
                with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll"}) as audit:
                    self.assertEqual(check_package(package, "unused"), set())
                self.assertEqual({call.args[0] for call in audit.call_args_list},
                                 {package / "neo.exe", package / "runtime/onnx/onnxruntime.dll"})

    def test_sh_alone_is_not_a_bundled_bash(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td), bash_path="runtime/gitbash/usr/bin/sh.exe")
            with patch("tools.check_release.inspect_pe") as audit:
                with self.assertRaisesRegex(ValueError, "bash.exe"):
                    check_package(package, "unused")
            audit.assert_not_called()

    def test_invalid_bash_is_not_hidden_by_other_layout(self):
        for name in self.bash_paths:
            for state in ("empty", "directory"):
                with self.subTest(path=name, state=state), tempfile.TemporaryDirectory() as td:
                    root = Path(td)
                    for layout in self.bash_paths:
                        package = self.make_package(root, bash_path=layout)
                    path = package / name
                    path.unlink()
                    if state == "empty":
                        path.touch()
                    else:
                        path.mkdir()
                    with patch("tools.check_release.inspect_pe") as audit:
                        with self.assertRaisesRegex(ValueError, "bash.exe"):
                            check_package(package, "unused")
                    audit.assert_not_called()

    def test_additional_onnx_dll_must_be_nonempty(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            (package / "runtime/onnx/onnxruntime_providers_shared.dll").touch()
            with patch("tools.check_release.inspect_pe") as audit:
                with self.assertRaisesRegex(ValueError, "onnxruntime_providers_shared"):
                    check_package(package, "unused")
            audit.assert_not_called()

    def test_audits_all_onnx_dlls_but_not_gitbash(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            provider = package / "runtime/onnx/onnxruntime_providers_shared.dll"
            provider.write_bytes(b"inert provider")
            (package / "runtime/gitbash/usr/bin/msys-2.0.dll").write_bytes(b"inert gitbash")
            with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll"}) as audit:
                check_package(package, "unused")
            self.assertEqual({call.args[0] for call in audit.call_args_list},
                             {package / "neo.exe", package / "runtime/onnx/onnxruntime.dll", provider})

    def test_legacy_layout_does_not_satisfy_new_payload_contract(self):
        pairs = [("runtime/onnx/onnxruntime.dll", "assets/onnxruntime.dll"),
                 ("resources/models/wake/hi_neo.onnx", "assets/hi_neo.onnx"),
                 ("resources/models/stt/sense-voice/tokens.txt", "assets-stt/sense-voice/tokens.txt")]
        for current, legacy in pairs:
            with self.subTest(path=current), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td))
                old = package / legacy
                old.parent.mkdir(parents=True, exist_ok=True)
                (package / current).rename(old)
                with patch("tools.check_release.inspect_pe") as audit:
                    with self.assertRaisesRegex(ValueError, "Required release resource missing/empty"):
                        check_package(package, "unused")
                audit.assert_not_called()

    def test_payload_without_descriptive_readmes_passes(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            self.assertFalse(list(package.rglob("README.md")))
            with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll"}):
                self.assertEqual(check_package(package, "unused", crt_policy="external"), set())

    def test_success_reports_limited_audit_scope(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            output = io.StringIO()
            with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll", "third_party.dll"}), \
                    redirect_stdout(output):
                self.assertEqual(check_package(package, "unused"), set())
            self.assertIn("payload files exist and are non-empty", output.getvalue())
            self.assertIn("payload completeness only; license compliance is NOT verified", output.getvalue())
            self.assertIn("Unresolved legal release blockers still require review", output.getvalue())
            self.assertIn("private audit reports are not release payloads", output.getvalue())
            self.assertIn("neo.exe, runtime/onnx/*.dll and imported allowlisted CRT only", output.getvalue())
            self.assertIn("Full third-party DLL dependency closure is NOT audited", output.getvalue())
            self.assertIn("MinGit PE images/dependencies and model validity are NOT audited", output.getvalue())
            self.assertIn("Windows 10 19041 runtime validation is still required", output.getvalue())

    def test_external_audits_main_and_all_onnx_without_copying_crt(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            provider = package / "runtime/onnx/onnxruntime_providers_shared.dll"
            provider.write_bytes(b"inert")
            before = {p.relative_to(package): p.read_bytes() for p in package.rglob("*") if p.is_file()}
            output = io.StringIO()
            imports = {"neo.exe": {"vcruntime140.dll", "third_party.dll"},
                       "onnxruntime.dll": {"msvcp140.dll"},
                       provider.name: {"vcruntime140_1.dll"}}
            with patch("tools.check_release.inspect_pe", side_effect=lambda p, _: imports[p.name]) as audit, \
                    patch("tools.check_release.shutil.copy2") as copy, redirect_stdout(output):
                required = check_package(package, "unused", crt_policy="external")
            self.assertEqual(required, {"vcruntime140.dll", "msvcp140.dll", "vcruntime140_1.dll"})
            self.assertEqual({c.args[0] for c in audit.call_args_list},
                             {package / "neo.exe", package / "runtime/onnx/onnxruntime.dll", provider})
            copy.assert_not_called()
            self.assertEqual(before, {p.relative_to(package): p.read_bytes()
                                      for p in package.rglob("*") if p.is_file()})
            text = output.getvalue()
            for statement in ("external-prerequisite", "Required CRT names", CRT_MIN_VERSION, CRT_DOWNLOAD_URL,
                              "NOT a minimum inferred from PE imports", "compatibility is NOT established",
                              "license compliance is NOT verified", "Full third-party DLL dependency closure is NOT audited"):
                self.assertIn(statement, text)
            self.assertNotIn("Required app-local CRT", text)

    def test_external_rejects_bundled_crt_even_unused_empty_or_nested(self):
        for name in CRT_NAMES:
            for parent in ("", "runtime/onnx", "runtime/gitbash/usr/bin"):
                with self.subTest(name=name, parent=parent), tempfile.TemporaryDirectory() as td:
                    package = self.make_package(Path(td))
                    bundled = package / parent / name.upper()
                    bundled.touch()
                    with patch("tools.check_release.inspect_pe") as audit, \
                            patch("tools.check_release.shutil.copy2") as copy:
                        with self.assertRaisesRegex(ValueError, "forbids bundled allowlisted CRT"):
                            check_package(package, "unused", crt_policy="external")
                    audit.assert_not_called()
                    copy.assert_not_called()
                    self.assertTrue(bundled.exists())

    def test_external_rejects_redist_argument_and_unknown_policy(self):
        with patch("tools.check_release.validate_payload") as validate:
            with self.assertRaisesRegex(ValueError, "requires --crt-policy app-local"):
                check_package("unused", "unused", "unused", crt_policy="external")
            with self.assertRaisesRegex(ValueError, "Unknown CRT policy"):
                check_package("unused", "unused", crt_policy="typo")
        validate.assert_not_called()

    def test_cli_defaults_external_and_requires_explicit_app_local_for_redist(self):
        for extra, expected in [([], "external"), (["--crt-policy", "external"], "external"),
                                (["--crt-policy", "app-local"], "app-local"),
                                (["--crt-policy", "app-local", "--redist-dir", "licensed"], "app-local")]:
            with self.subTest(extra=extra), patch("sys.argv", ["check_release.py", *extra]), \
                    patch("tools.check_release.check_package") as check:
                main()
                check.assert_called_once_with(Path("dist/neo"), "dumpbin.exe",
                                              Path("licensed") if "--redist-dir" in extra else None,
                                              crt_policy=expected)
        for extra in ([], ["--crt-policy", "external"]):
            with patch("sys.argv", ["check_release.py", "--redist-dir", "licensed", *extra]), \
                    patch("tools.check_release.check_package") as check, patch("sys.stderr", new=io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    main()
                self.assertEqual(error.exception.code, 2)
                check.assert_not_called()

    def test_external_still_blocks_native_markers_architecture_and_unknown_crt(self):
        for target in ("neo.exe", "runtime/onnx/onnxruntime.dll"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td))
                for name in ("neo.exe", "runtime/onnx/onnxruntime.dll"):
                    (package / name).write_bytes(native_fixture(b"espeak_Synth" if name == target else b"safe"))
                result = subprocess.CompletedProcess([], 0, " 8664 machine (x64)\n VCRUNTIME140.dll\n")
                with patch("tools.check_release.subprocess.run", return_value=result), \
                        self.assertRaisesRegex(ValueError, "Distribution BLOCKED"):
                    check_package(package, "unused", crt_policy="external")
        for report in (" AA64 machine (ARM64)\n KERNEL32.dll", " 8664 machine (x64)\n MSVCP140D.dll"):
            with self.subTest(report=report), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td))
                (package / "neo.exe").write_bytes(native_fixture())
                with patch("tools.check_release.subprocess.run", return_value=subprocess.CompletedProcess([], 0, report)), \
                        self.assertRaises(ValueError):
                    check_package(package, "unused", crt_policy="external")

    def test_collect_only_imported_crt_recursively_next_to_exe(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            package = self.make_package(root)
            redist = self.make_redist(root)
            for name in ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "concrt140.dll"]:
                (redist / name).write_bytes(name.encode())
            imports = {"neo.exe": {"vcruntime140.dll", "kernel32.dll"},
                       "onnxruntime.dll": {"msvcp140.dll", "vcruntime140.dll"},
                       "vcruntime140.dll": {"kernel32.dll"},
                       "msvcp140.dll": {"vcruntime140_1.dll", "vcruntime140.dll"},
                       "vcruntime140_1.dll": {"vcruntime140.dll"}}
            with patch("tools.check_release.inspect_pe", side_effect=lambda p, _: imports[p.name]) as audit:
                required = check_package(package, "unused", redist)
            self.assertEqual(required, {"vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"})
            self.assertEqual(audit.call_count, 5)
            for name in required:
                self.assertEqual((package / name).read_bytes(), (redist / name).read_bytes())
            self.assertFalse((package / "concrt140.dll").exists())
            self.assertFalse((package / "runtime/onnx/vcruntime140.dll").exists())
            self.assertFalse((package / "resources/models/wake/vcruntime140.dll").exists())
            with patch("tools.check_release.inspect_pe", side_effect=lambda p, _: imports[p.name]):
                self.assertEqual(check_package(package, "unused"), required)

    def test_every_supported_crt_is_collected_and_audited(self):
        report = " 8664 machine (x64)\n" + "\n".join(name.upper() for name in CRT_NAMES)
        self.assertEqual(parse_pe_report(report), set(CRT_NAMES))
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            package = self.make_package(root)
            redist = self.make_redist(root)
            for name in CRT_NAMES:
                (redist / name).write_bytes(name.encode())
            with patch("tools.check_release.inspect_pe", side_effect=lambda p, _: (
                    set(CRT_NAMES) if p.name == "neo.exe" else {"kernel32.dll"})) as audit:
                self.assertEqual(check_package(package, "unused", redist), set(CRT_NAMES))
            self.assertEqual(audit.call_count, len(CRT_NAMES) + 2)
            for name in CRT_NAMES:
                self.assertEqual((package / name).read_bytes(), name.encode())

    def test_missing_crt_fails_closed_without_system_fallback(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            package = self.make_package(root)
            redist = self.make_redist(root)
            with patch("tools.check_release.inspect_pe", return_value={"vcruntime140.dll"}):
                with self.assertRaisesRegex(ValueError, "Missing app-local CRT"):
                    check_package(package, "unused")
                with self.assertRaisesRegex(ValueError, "absent from licensed"):
                    check_package(package, "unused", redist)

    def test_empty_crt_fails_without_copying_or_auditing_it(self):
        for use_redist in (False, True):
            with self.subTest(use_redist=use_redist), tempfile.TemporaryDirectory() as td:
                root = Path(td)
                package = self.make_package(root)
                redist = self.make_redist(root)
                source = (redist if use_redist else package) / "vcruntime140.dll"
                source.touch()
                with patch("tools.check_release.inspect_pe", return_value={"vcruntime140.dll"}) as audit, \
                        patch("tools.check_release.shutil.copy2") as copy:
                    with self.assertRaisesRegex(ValueError, "empty"):
                        check_package(package, "unused", redist if use_redist else None)
                audit.assert_called_once_with(package / "neo.exe", "unused")
                copy.assert_not_called()


if __name__ == "__main__":
    unittest.main()
