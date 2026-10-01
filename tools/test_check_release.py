"""通过合成报告与临时载荷测试发行校验，绝不执行 PE 文件。"""
from contextlib import redirect_stdout
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.check_release import CRT_NAMES, MODELS, check_package, parse_pe_report, validate_redist_dir


class ReleaseTests(unittest.TestCase):
    # 独立列出发行契约，避免 fixture 随校验清单一起漏掉新资源。
    payload_files = (
        "neo.exe", "assets/onnxruntime.dll", *(f"assets/{name}" for name in MODELS),
        "assets-stt/sense-voice/model.int8.onnx", "assets-stt/sense-voice/tokens.txt",
        "assets-stt/vad/silero_vad.onnx", "LICENSE", "README.md",
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

    def test_both_fetch_runtime_bash_layouts_are_accepted(self):
        for name in self.bash_paths:
            with self.subTest(path=name), tempfile.TemporaryDirectory() as td:
                package = self.make_package(Path(td), bash_path=name)
                with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll"}) as audit:
                    self.assertEqual(check_package(package, "unused"), set())
                self.assertEqual({call.args[0] for call in audit.call_args_list},
                                 {package / "neo.exe", package / "assets/onnxruntime.dll"})

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

    def test_additional_asset_dll_must_be_nonempty(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            (package / "assets/onnxruntime_providers_shared.dll").touch()
            with patch("tools.check_release.inspect_pe") as audit:
                with self.assertRaisesRegex(ValueError, "onnxruntime_providers_shared"):
                    check_package(package, "unused")
            audit.assert_not_called()

    def test_success_reports_limited_audit_scope(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            output = io.StringIO()
            with patch("tools.check_release.inspect_pe", return_value={"kernel32.dll", "third_party.dll"}), \
                    redirect_stdout(output):
                self.assertEqual(check_package(package, "unused"), set())
            self.assertIn("payload files exist and are non-empty", output.getvalue())
            self.assertIn("Full third-party DLL dependency closure is NOT audited", output.getvalue())
            self.assertIn("MinGit PE images/dependencies and model validity are NOT audited", output.getvalue())
            self.assertIn("Windows 10 19041 runtime validation is still required", output.getvalue())

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
            self.assertFalse((package / "assets/vcruntime140.dll").exists())
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
