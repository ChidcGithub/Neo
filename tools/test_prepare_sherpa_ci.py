"""Standalone offline regressions: fake commands, temporary inputs, no GUI/network."""
import argparse
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

if __package__:
    from . import prepare_sherpa_ci as ci
else:
    import prepare_sherpa_ci as ci

build = ci.build


class PrepareSherpaTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name, value in (("ROOT", self.root), ("WORK", self.root / "target/sherpa-asr"),
                            ("CACHE", self.root / ".cache/sherpa-asr")):
            mock = patch.object(build, name, value)
            mock.start()
            self.addCleanup(mock.stop)
        build.WORK.mkdir(parents=True)
        build.CACHE.mkdir(parents=True)
        # Any accidental real command/network operation is a test failure.
        self.command = patch.object(ci.subprocess, "run", side_effect=AssertionError("unexpected command"))
        self.runner = self.command.start()
        self.addCleanup(self.command.stop)
        self.args = argparse.Namespace(cmake=None, vswhere=None, budget_seconds=600,
                                       validation_seconds=180, max_download_mib=256)

    def source_archive(self):
        archive = build.CACHE / build.SOURCE_FILE
        with tarfile.open(archive, "w:gz") as out:
            info = tarfile.TarInfo(f"sherpa-onnx-{build.COMMIT}/LICENSE")
            info.size = 7
            out.addfile(info, io.BytesIO(b"fixture"))
        return archive

    def test_exact_tracked_source_and_wheel_pins(self):
        self.assertEqual(build.SOURCE_SHA256, "0a8db6c55dd318f4a688faba85f7760b99a6c92e8ef8864479d418531bee1ac2")
        self.assertEqual(ci.CMAKE_SHA256, "0c55af0e1b2db232a94a7c34e89f25f3dbf410a4669b11134d07de0bd7aad03e")

    def test_cached_source_without_lock_is_verified_and_records_exact_pin(self):
        archive = self.source_archive()
        with patch.object(build, "SOURCE_SHA256", build.digest(archive)):
            build.fetch_source()
            lock = build.check_source()
            self.assertEqual(lock["sha256"], build.SOURCE_SHA256)
            self.assertIn("tracked known SHA-256", lock["trust"])
            before = (build.CACHE / "source-lock.json").read_bytes()
            build.fetch_source()
            self.assertEqual((build.CACHE / "source-lock.json").read_bytes(), before)
        self.runner.assert_not_called()

    def test_source_download_uses_expected_hash_before_lock_creation(self):
        with patch.object(build, "download", side_effect=ValueError("stop")) as download:
            with self.assertRaises(ValueError):
                build.fetch_source()
            self.assertEqual(download.call_args.args[2], build.SOURCE_SHA256)
            self.assertEqual(download.call_args.kwargs["timeout"], 120)
        self.assertFalse((build.CACHE / "source-lock.json").exists())

    def test_wrong_archive_and_self_consistent_wrong_lock_fail_closed(self):
        archive = self.source_archive()
        with self.assertRaisesRegex(ValueError, "archive"):
            build.fetch_source()
        lock = {"version": build.VERSION, "commit": build.COMMIT, "url": build.SOURCE_URL,
                "archive": build.SOURCE_FILE, **build.record(archive)}
        build.write_json(build.CACHE / "source-lock.json", lock)
        with self.assertRaisesRegex(ValueError, "hash/size"):
            build.check_source()
        self.runner.assert_not_called()

    def vs_fixture(self, major):
        root = self.root / "Visual Studio space" / str(major)
        toolset = root / "VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt"
        toolset.parent.mkdir(parents=True)
        toolset.write_text("14.51.36231\n")
        dumpbin = root / "VC/Tools/MSVC/14.51.36231/bin/Hostx64/x64/dumpbin.exe"
        dumpbin.parent.mkdir(parents=True)
        dumpbin.write_bytes(b"fake command; never executed")
        return root, dumpbin, [{"installationPath": str(root), "installationVersion": f"{major}.8.12023.21"}]

    def test_vswhere_selects_real_vs17_or_vs18_and_absolute_dumpbin(self):
        before = dict(os.environ)
        for major in (17, 18):
            with self.subTest(major=major):
                root, dumpbin, instances = self.vs_fixture(major)
                with patch.object(ci, "capture", return_value=json.dumps(instances)) as run:
                    generator, instance, binary = ci.discover_vs("fake-vswhere.exe")
                self.assertEqual(generator, ci.GENERATORS[major])
                self.assertEqual(instance, f"{root},version={major}.8.12023.21")
                self.assertEqual(binary, str(dumpbin))
                self.assertIn("Microsoft.VisualStudio.Component.VC.Tools.x86.x64", run.call_args.args[0])
                self.assertIn("-utf8", run.call_args.args[0])
        self.assertEqual(dict(os.environ), before)

    def test_unknown_vs_missing_vs_and_missing_dumpbin_rejected(self):
        for instances in ([], [{"installationVersion": "19.0", "installationPath": str(self.root)}]):
            with patch.object(ci, "capture", return_value=json.dumps(instances)), self.assertRaises(ValueError):
                ci.discover_vs("fake")
        _, dumpbin, instances = self.vs_fixture(17)
        dumpbin.unlink()
        with patch.object(ci, "capture", return_value=json.dumps(instances)), self.assertRaisesRegex(ValueError, "dumpbin"):
            ci.discover_vs("fake")

    def wheel_fixture(self, member="cmake/data/bin/cmake.exe"):
        wheel = build.ROOT / ".cache/tools" / ci.CMAKE_WHEEL
        wheel.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(wheel, "w") as archive:
            archive.writestr(member, b"fake command; never executed")
        return wheel

    def test_wheel_is_checked_before_extraction_or_execution(self):
        wheel = self.wheel_fixture()
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            ci.prepare_cmake()
        self.assertFalse((build.WORK / "ci-cmake").exists())
        self.runner.assert_not_called()
        with patch.object(ci, "CMAKE_SHA256", build.digest(wheel)):
            executable = ci.prepare_cmake()
            self.assertTrue(executable.is_file())
            with self.assertRaisesRegex(ValueError, "retained"):
                ci.prepare_cmake()
        self.runner.assert_not_called()

    def test_download_uses_official_hash_required_wheel_and_no_install(self):
        with patch.object(build, "run", side_effect=ValueError("stop")) as run:
            with self.assertRaises(ValueError):
                ci.prepare_cmake()
        command = run.call_args.args[0]
        self.assertIn("download", command)
        self.assertNotIn("install", command)
        self.assertIn("--isolated", command)
        self.assertIn("--require-hashes", command)
        self.assertIn("https://pypi.org/simple", command)
        self.assertIn(ci.CMAKE_SHA256, (build.WORK / "ci-cmake-requirements.txt").read_text())

    def test_wheel_traversal_rejected_before_extraction(self):
        wheel = self.wheel_fixture("../escape")
        with patch.object(ci, "CMAKE_SHA256", build.digest(wheel)), self.assertRaises(ValueError):
            ci.prepare_cmake()
        self.assertFalse((build.WORK / "ci-cmake").exists())

    def test_cmake_version_and_generator_are_verified(self):
        info = {"version": {"string": ci.CMAKE_VERSION}, "generators": [{"name": ci.GENERATORS[18]}]}
        with patch.object(ci, "capture", return_value=json.dumps(info)):
            ci.check_cmake("fake-cmake", ci.GENERATORS[18])
            with self.assertRaisesRegex(ValueError, "generator"):
                ci.check_cmake("fake-cmake", ci.GENERATORS[17])
        info["version"]["string"] = "3.31.0"
        with patch.object(ci, "capture", return_value=json.dumps(info)), self.assertRaisesRegex(ValueError, "4.2.3"):
            ci.check_cmake("fake-cmake", ci.GENERATORS[18])

    def install_fixture(self):
        attempt = build.WORK / "native"
        libdir = attempt / "install/lib"
        libdir.mkdir(parents=True)
        libraries = {}
        reports = {}
        for name in build.INSTALLED_LIBS:
            library = libdir / (name + ".lib")
            library.write_bytes(b"!<arch>\nSYNTHETIC TEST ONLY")
            libraries[library.name] = build.record(library)
            report = attempt / "symbols" / (name + ".symbols.txt")
            report.parent.mkdir(exist_ok=True)
            report.write_text("SYNTHETIC TEST ONLY")
            reports[report.name] = build.record(report)
        graph = attempt / "graph.json"
        build.write_json(graph, [])
        links = attempt / "omitted-source-links.json"
        build.write_json(links, [])
        license = libdir.parent / "licenses/sherpa/LICENSE"
        license.parent.mkdir(parents=True)
        license.write_text("synthetic license")
        receipt = {"source_lock": {"version": build.VERSION, "commit": build.COMMIT,
                   "url": build.SOURCE_URL, "archive": build.SOURCE_FILE, "sha256": build.SOURCE_SHA256,
                   "trust": "first-observed HTTPS commit archive; not an upstream signed digest"},
                   "options": build.OPTIONS,
                   "dependency_archives": {k: {"url": v[0], "sha256": v[1]} for k, v in build.DEPS.items()},
                   "commands": {stage + ".command.json": {"returncode": 0} for stage in ("configure", "build", "install")},
                   "symbol_reports": reports, "graph": build.record(graph),
                   "omitted_source_links": build.record(links),
                   "licenses": {"licenses/sherpa/LICENSE": build.record(license)}}
        build.write_json(libdir / "neo-asr-receipt.json", receipt)
        manifest = {"schema": 1, "status": "native-validated", "version": build.VERSION,
                    "source_commit": build.COMMIT, "source_sha256": build.SOURCE_SHA256,
                    "target": "x86_64-pc-windows-msvc", "configuration": "Release",
                    "options": build.OPTIONS, "libraries": libraries,
                    "receipt": build.record(libdir / "neo-asr-receipt.json")}
        build.write_json(libdir / "neo-sherpa-asr.json", manifest)
        return libdir

    def test_existing_valid_install_is_checked_without_commands_or_rewrites(self):
        libdir = self.install_fixture()
        before = {p: p.read_bytes() for p in build.WORK.rglob("*") if p.is_file()}
        ci.prepare(self.args)
        self.assertEqual(before, {p: p.read_bytes() for p in build.WORK.rglob("*") if p.is_file()})
        self.runner.assert_not_called()
        self.assertTrue((libdir / "neo-sherpa-asr.json").exists())

    def test_invalid_install_and_partial_attempt_never_rebuild(self):
        libdir = self.install_fixture()
        (libdir / "sherpa-onnx-core.lib").write_bytes(b"tampered")
        with patch.object(build, "build") as compile:
            with self.assertRaises(ValueError):
                ci.prepare(self.args)
            (libdir / "neo-sherpa-asr.json").unlink()
            with self.assertRaisesRegex(ValueError, "Incomplete"):
                ci.prepare(self.args)
            compile.assert_not_called()

    def test_extra_library_and_modified_evidence_rejected(self):
        libdir = self.install_fixture()
        extra = libdir / "espeak-ng.lib"
        extra.write_bytes(b"old TTS")
        with self.assertRaisesRegex(ValueError, "library set"):
            ci.validate_install(libdir)
        extra.unlink()
        (build.WORK / "native/symbols/onnxruntime.symbols.txt").write_text("tampered")
        with self.assertRaisesRegex(ValueError, "hash/size"):
            ci.validate_install(libdir)

    def test_receipt_source_lock_and_failed_stage_rejected_even_if_rehashed(self):
        libdir = self.install_fixture()
        receipt_path = libdir / "neo-asr-receipt.json"
        manifest_path = libdir / "neo-sherpa-asr.json"
        receipt = json.loads(receipt_path.read_text())
        manifest = json.loads(manifest_path.read_text())
        for failure in ("source", "stage"):
            receipt["source_lock"]["sha256"] = "a" * 64 if failure == "source" else build.SOURCE_SHA256
            receipt["commands"]["build.command.json"]["returncode"] = 1 if failure == "stage" else 0
            build.write_json(receipt_path, receipt)
            manifest["receipt"] = build.record(receipt_path)
            build.write_json(manifest_path, manifest)
            with self.assertRaises(ValueError):
                ci.validate_install(libdir)

    def test_validation_has_explicit_total_budget(self):
        libdir = self.install_fixture()
        with patch.object(build.time, "monotonic", side_effect=[0, 181]), self.assertRaisesRegex(ValueError, "budget"):
            ci.validate_install(libdir, 180)

    def test_fresh_prepare_orders_fetch_build_validate_and_passes_budgets(self):
        events = []
        with patch.object(ci, "discover_vs", return_value=(ci.GENERATORS[17], "instance,version=17.0", "dumpbin")), \
             patch.object(ci, "prepare_cmake", return_value=Path("cmake")), \
             patch.object(ci, "check_cmake", side_effect=lambda *a: events.append("cmake")), \
             patch.object(build, "fetch_source", side_effect=lambda: events.append("source")), \
             patch.object(build, "fetch_deps", side_effect=lambda cap: events.append(cap)), \
             patch.object(build, "build", side_effect=lambda a: events.append(a)) as compile, \
             patch.object(ci, "validate_install", side_effect=lambda *a: events.append("validate")) as validate:
            ci.prepare(self.args)
        self.assertEqual(events[:3], ["cmake", "source", 256 * 1024**2])
        self.assertEqual(events[-1], "validate")
        args = compile.call_args.args[0]
        self.assertEqual(args.budget_seconds, 600)
        self.assertEqual(args.generator, "Visual Studio 17 2022")
        self.assertEqual(args.vs_instance, "instance,version=17.0")
        self.assertEqual(validate.call_args.args[1], 180)

    def test_build_failure_is_not_retried_or_turned_into_validation_recovery(self):
        self.args.cmake = str(self.root / "cmake.exe")
        with patch.object(ci, "discover_vs", return_value=(ci.GENERATORS[18], "instance", "dumpbin")), \
             patch.object(ci, "check_cmake"), patch.object(build, "fetch_source"), \
             patch.object(build, "fetch_deps"), patch.object(ci, "validate_install") as validate, \
             patch.object(build, "validate_existing") as recover, \
             patch.object(build, "build", side_effect=ValueError("timeout")) as compile:
            with self.assertRaisesRegex(ValueError, "timeout"):
                ci.prepare(self.args)
        compile.assert_called_once()
        validate.assert_not_called()
        recover.assert_not_called()

    def test_capture_executes_fake_command_and_propagates_failure(self):
        # Restore subprocess only for these harmless Python fixture commands.
        self.command.stop()
        self.assertEqual(ci.capture([sys.executable, "-c", "print('fixture')"]).strip(), "fixture")
        with self.assertRaises(subprocess.CalledProcessError):
            ci.capture([sys.executable, "-c", "raise SystemExit(7)"])


if __name__ == "__main__":
    unittest.main()
