"""通过合成报告与临时载荷测试发行校验，绝不执行 PE 文件。"""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.check_release import CRT_NAMES, MODELS, check_package, parse_pe_report, validate_redist_dir


class ReleaseTests(unittest.TestCase):
    def test_imports_include_delayed_crt_and_ignore_case(self):
        report = " 8664 machine (x64)\n    KERNEL32.dll\n    VCRUNTIME140.dll\n  delay load imports\n    MSVCP140_1.dll\n"
        self.assertEqual(parse_pe_report(report), {"kernel32.dll", "vcruntime140.dll", "msvcp140_1.dll"})

    def test_reject_wrong_arch_empty_imports_and_debug_runtime(self):
        for report in [" AA64 machine (ARM64)\n KERNEL32.dll", " 14C machine (x86)\n KERNEL32.dll",
                       " 8664 machine (x64)\n", " 8664 machine (x64)\n MSVCP140D.dll",
                       " 8664 machine (x64)\n MSVCR120.dll", "corrupt binary"]:
            with self.subTest(report=report), self.assertRaises(ValueError):
                parse_pe_report(report)

    def make_package(self, root):
        package = root / "package"
        assets = package / "assets"
        assets.mkdir(parents=True)
        for path in [package / "neo.exe", assets / "onnxruntime.dll", *(assets / name for name in MODELS)]:
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
                        root / "VC/Redist/MSVC/14.44.1/onecore/x64/Microsoft.VC143.CRT"]:
                with self.subTest(path=bad), self.assertRaises(ValueError):
                    validate_redist_dir(bad)

    def test_missing_model_fails_before_binary_audit(self):
        with tempfile.TemporaryDirectory() as td:
            package = self.make_package(Path(td))
            (package / "assets/hi_neo.onnx").unlink()
            with patch("tools.check_release.inspect_pe") as audit, self.assertRaisesRegex(ValueError, "hi_neo"):
                check_package(package, "unused")
            audit.assert_not_called()

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


if __name__ == "__main__":
    unittest.main()
