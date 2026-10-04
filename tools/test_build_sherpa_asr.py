"""Offline regression tests. Scratch files stay under target/sherpa-asr."""
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

if __package__:
    from . import build_sherpa_asr as build
    from . import setup_native_tools as setup
else:
    import build_sherpa_asr as build
    import setup_native_tools as setup


class SourceBuilderTests(unittest.TestCase):
    def setUp(self):
        build.WORK.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=build.WORK)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def tar(self, name="root/LICENSE", data=b"upstream license", kind=None):
        archive = self.root / "source.tar.gz"
        with tarfile.open(archive, "w:gz") as out:
            member = tarfile.TarInfo(name)
            member.size = len(data)
            if kind:
                member.type = kind
                member.linkname = "../outside"
                member.size = 0
            out.addfile(member, io.BytesIO(data))
        return archive

    def test_complete_license_preserved(self):
        archive = self.tar()
        source = build.extract(archive, self.root / "out")
        self.assertEqual((source / "LICENSE").read_bytes(), b"upstream license")

    def test_tar_traversal_rejected(self):
        with self.assertRaises(ValueError):
            build.extract(self.tar("../outside"), self.root / "out")
        self.assertFalse((self.root / "outside").exists())

    def test_zip_traversal_rejected(self):
        archive = self.root / "input.zip"
        with zipfile.ZipFile(archive, "w") as out:
            out.writestr("../outside", "bad")
        with self.assertRaises(ValueError):
            build.extract(archive, self.root / "out")

    def test_links_rejected(self):
        with self.assertRaises(ValueError):
            build.extract(self.tar(kind=tarfile.SYMTYPE), self.root / "out")

    def test_source_example_links_are_inventoried_not_followed(self):
        name = f"sherpa-onnx-{build.COMMIT}/scripts/example.py"
        archive = self.tar(name=name, kind=tarfile.SYMTYPE)
        with tarfile.open(archive, "r:gz") as source:
            member = source.next()
        with tarfile.open(archive, "w:gz") as out:
            out.addfile(member)
            license = tarfile.TarInfo(f"sherpa-onnx-{build.COMMIT}/LICENSE")
            license.size = 4
            out.addfile(license, io.BytesIO(b"keep"))
        omitted = []
        root = build.extract(archive, self.root / "out", omitted_links=omitted)
        self.assertEqual((root / "LICENSE").read_bytes(), b"keep")
        self.assertFalse((root / "scripts/example.py").exists())
        self.assertEqual(omitted, [{"path": name, "target": "../outside"}])

    def test_native_and_license_links_still_rejected(self):
        for suffix in ("sherpa-onnx/csrc/real.cc", "scripts/LICENSE", "scripts/CMakeLists.txt"):
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                build.extract(self.tar(name=f"sherpa-onnx-{build.COMMIT}/{suffix}",
                                       kind=tarfile.SYMTYPE),
                              self.root / str(len(list(self.root.iterdir()))), omitted_links=[])

    def test_source_download_has_120_second_limit(self):
        with patch.object(build, "CACHE", self.root), patch.object(build, "download", side_effect=ValueError) as fetch:
            with self.assertRaises(ValueError):
                build.fetch_source()
            self.assertEqual(fetch.call_args.kwargs["timeout"], 120)

    def test_repeated_command_preserves_previous_log(self):
        log = self.root / "probe.log"
        log.write_text("previous evidence")
        with patch.object(build.subprocess, "run") as runner:
            runner.return_value.returncode = 0
            build.run(["fixture"], log, timeout=1)
        self.assertEqual((self.root / "probe.log.previous").read_text(), "previous evidence")

    def test_extraction_budget(self):
        with self.assertRaises(ValueError):
            build.extract(self.tar(), self.root / "out", max_expanded=1)

    def test_no_reuse_of_sources(self):
        archive = self.tar()
        build.extract(archive, self.root / "out")
        with self.assertRaises(ValueError):
            build.extract(archive, self.root / "out")

    def test_no_retry_of_partial_download(self):
        path = self.root / "source.tar.gz"
        path.with_name(path.name + ".partial").write_bytes(b"incomplete")
        with patch.object(build, "run") as runner:
            with self.assertRaises(ValueError):
                build.download(build.SOURCE_URL, path, None, 100)
            runner.assert_not_called()

    def test_hash_mismatch_not_overwritten(self):
        path = self.root / "source.tar.gz"
        path.write_bytes(b"wrong")
        with patch.object(build, "run") as runner:
            with self.assertRaises(ValueError):
                build.download(build.SOURCE_URL, path, "0" * 64, 100)
            runner.assert_not_called()
        self.assertEqual(path.read_bytes(), b"wrong")

    def test_source_lock_detects_tampering(self):
        archive = self.root / build.SOURCE_FILE
        archive.write_bytes(b"fixture")
        lock = {"version": build.VERSION, "commit": build.COMMIT, "url": build.SOURCE_URL,
                "archive": build.SOURCE_FILE, **build.record(archive)}
        build.write_json(self.root / "source-lock.json", lock)
        with patch.object(build, "CACHE", self.root), patch.object(build, "SOURCE_SHA256", lock["sha256"]):
            self.assertEqual(build.check_source(), lock)
            archive.write_bytes(b"tampered")
            with self.assertRaises(ValueError):
                build.check_source()

    def test_no_tts_dependency_and_exact_pins(self):
        self.assertEqual(build.COMMIT, "11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf")
        for name, (_, sha, _) in build.DEPS.items():
            self.assertFalse(build.RISK.search(name))
            self.assertRegex(sha, r"^[0-9a-f]{64}$")
        self.assertEqual(build.OPTIONS["SHERPA_ONNX_ENABLE_TTS"], "OFF")
        self.assertEqual(build.OPTIONS["FETCHCONTENT_FULLY_DISCONNECTED"], "ON")
        self.assertEqual(build.OPTIONS["SHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE"], "OFF")

    def test_speaker_symbols_are_not_espeak(self):
        for name in ("SherpaOnnxCreateSpeakerEmbeddingExtractor",
                     "SherpaOnnxCreateOfflineSpeakerDiarization", "OfflineSpeaker"):
            self.assertIsNone(build.RISK.search(name))
        for name in ("espeak_Initialize", "espeak_ng_Initialize", "EspeakNg",
                     "piper_phonemize.lib", "offline-tts.cc", "phonemize.obj"):
            self.assertIsNotNone(build.RISK.search(name))

    def test_budget_uses_total_deadline(self):
        with patch.object(build.time, "monotonic", return_value=100):
            self.assertEqual(build.remaining(110, 60), 10)
            self.assertEqual(build.remaining(200, 60), 60)
            with self.assertRaises(ValueError):
                build.remaining(100, 60)

    def test_effective_dependency_versions(self):
        self.assertEqual(len(build.DEPS), 9)
        self.assertEqual(build.DEPS["eigen"][2], "eigen-5.0.1.tar.gz")
        self.assertEqual(build.DEPS["openfst"][2], "openfst-1.8.5-2026-07-09.tar.gz")

    def test_graph_utf8_and_tts_rejection(self):
        reply = self.root / ".cmake/api/v1/reply"
        build.write_json(reply / "index-test.json", {"objects": [{"kind": "codemodel", "jsonFile": "model.json"}]})
        build.write_json(reply / "model.json", {"paths": {"source": "D:/work/piper/native/source/sherpa", "build": "D:/work/piper/native/build"}, "configurations": [{"name": "Release", "targets": [
            {"jsonFile": "api.json"}, {"jsonFile": "core.json"}]}]})
        build.write_json(reply / "api.json", {"name": "sherpa-onnx-c-api", "sources": []})
        paths = ["D:/work/piper/native/source/sherpa/高二/offline-sense-voice-model.cc",
                 "silero-vad-model.cc", "voice-activity-detector.cc",
                 "D:/work/piper/native/deps/openfst/src/fst.cc",
                 "D:/work/piper/native/build/generated.cc"]
        core = {"name": "sherpa-onnx-core", "sources": [{"path": s} for s in paths]}
        build.write_json(reply / "core.json", core)
        self.assertEqual(build.check_graph(self.root)[1]["sources"], paths)
        for bad in ("offline-tts.cc", "piper/innocent.cc",
                    "D:/work/piper/native/source/sherpa/piper/innocent.cc",
                    "D:/work/piper/native/deps/piper/src/innocent.cc",
                    "D:/work/piper/native/build/piper/generated.cc"):
            with self.subTest(path=bad):
                core["sources"].append({"path": bad})
                build.write_json(reply / "core.json", core)
                with self.assertRaises(ValueError):
                    build.check_graph(self.root)
                core["sources"].pop()
        core["name"] = "piper"
        build.write_json(reply / "core.json", core)
        with self.assertRaises(ValueError):
            build.check_graph(self.root)

    def test_symbol_risk_uses_identities_not_dump_path(self):
        for line in (r"Dump of file D:\work\piper\espeak\good.lib",
                     "Copyright (C) Microsoft Corporation", "File Type: LIBRARY",
                     "001 00000000 SECT1 notype External | SherpaOnnxCreateSpeakerEmbeddingExtractor",
                     r"Archive member name at 120: D:\work\piper\good.obj"):
            self.assertIsNone(build.RISK.search(build.symbol_risk_text(line)), line)
        for line in ("001 00000000 SECT1 notype External | espeak_Initialize",
                     "001 00000000 UNDEF notype External | espeak_ng_Initialize",
                     "001 00000000 SECT1 notype External | ?phonemize@piper@@",
                     "Archive member name at 120: piper/innocent.obj",
                     r"Archive member name at 120: D:\work\piper\espeak_api.obj"):
            self.assertIsNotNone(build.RISK.search(build.symbol_risk_text(line)), line)

    def test_default_script_has_no_legacy_fallback(self):
        source = (build.ROOT / "vendor/sherpa-onnx-sys/build.rs").read_text(encoding="utf-8")
        self.assertIn("../../target/sherpa-asr/native/install/lib", source)
        self.assertIn("neo_asr::validate(&path)?", source)
        self.assertNotIn("download_prebuilt_libs", source)
        self.assertNotIn('include!("build-before-default.rs")', source)

    def test_extra_old_library_rejected_before_dumpbin(self):
        for name in (*build.INSTALLED_LIBS, "espeak-ng"):
            (self.root / (name + ".lib")).write_bytes(b"!<arch>\nfixture")
        with patch.object(build, "run") as runner:
            with self.assertRaises(ValueError):
                build.validate_artifacts(self.root, "dumpbin", self.root / "logs")
            runner.assert_not_called()

    def test_no_native_manifest_on_preflight_failure(self):
        with patch.object(build, "WORK", self.root), patch.object(build, "CACHE", self.root / "cache"):
            code = build.main(["build", "--cmake", "definitely-missing-cmake-executable"])
        self.assertEqual(code, 2)
        self.assertFalse(list(self.root.rglob("neo-sherpa-asr.json")))
        self.assertEqual(json.loads((self.root / "last-failure.json").read_text())["status"], "blocked")

    def test_fetch_needs_explicit_budget(self):
        with patch.object(build, "WORK", self.root), patch.object(build, "CACHE", self.root / "cache"), patch.object(build, "download") as download:
            self.assertEqual(build.main(["fetch-deps"]), 2)
            download.assert_not_called()

    def test_timeout_receipt_and_no_retry(self):
        import subprocess
        with patch.object(build.subprocess, "run", side_effect=subprocess.TimeoutExpired("cmake", 180)) as runner:
            with self.assertRaisesRegex(ValueError, "descendants may still be running") as error:
                build.run(["cmake", "--build", "fixture", "--parallel", "2"], self.root / "build.log")
            self.assertNotIn("stopped", str(error.exception))
            self.assertEqual(runner.call_count, 1)
        receipt = json.loads((self.root / "build.command.json").read_text())
        self.assertTrue(receipt["timed_out"])
        self.assertEqual(receipt["cleanup_status"], "requires-manual-review")
        self.assertTrue(receipt["descendants_may_be_running"])
        self.assertFalse(receipt["automatic_retry"])
        self.assertNotIn("returncode", receipt)

    def test_rust_validator_rejects_tampering_and_failed_receipt(self):
        import copy
        import subprocess
        validator = build.WORK / "rust-check/debug/neo-sherpa-validator-check.exe"
        source = build.ROOT / "vendor/sherpa-onnx-sys/neo_asr.rs"
        if not validator.is_file() or validator.stat().st_mtime < source.stat().st_mtime:
            self.skipTest("Rebuild the target-local Rust validator harness before this integration test")
        # Synthetic integrity fixtures ONLY: never written over native evidence.
        libdir = self.root / "install/lib"
        libdir.mkdir(parents=True)
        artifacts = {}
        for name in build.INSTALLED_LIBS:
            path = libdir / (name + ".lib")
            path.write_bytes(b"!<arch>\nSYNTHETIC TEST FIXTURE, NOT NATIVE CODE")
            artifacts[path.name] = build.record(path)
        build.write_json(self.root / "graph.json", [])
        build.write_json(self.root / "omitted-source-links.json", [])
        symbols = self.root / "symbols"
        symbols.mkdir()
        for name in build.INSTALLED_LIBS:
            (symbols / (name + ".symbols.txt")).write_text("synthetic report")
        license_path = self.root / "install/licenses/sherpa/LICENSE"
        license_path.parent.mkdir(parents=True)
        license_path.write_text("synthetic license")
        receipt = {
            "source_lock": {"commit": build.COMMIT, "sha256": build.SOURCE_SHA256,
                            "version": build.VERSION, "url": build.SOURCE_URL, "archive": build.SOURCE_FILE},
            "dependency_archives": {k: {"url": v[0], "sha256": v[1]} for k, v in build.DEPS.items()},
            "options": dict(build.OPTIONS),
            "commands": {name + ".command.json": {"returncode": 0} for name in ("configure", "build", "install")},
            "graph": build.record(self.root / "graph.json"),
            "omitted_source_links": build.record(self.root / "omitted-source-links.json"),
            "symbol_reports": {p.name: build.record(p) for p in symbols.iterdir()},
            "licenses": {"licenses/sherpa/LICENSE": build.record(license_path)},
        }
        manifest = {"schema": 1, "status": "native-validated", "version": build.VERSION,
                    "source_commit": build.COMMIT, "source_sha256": build.SOURCE_SHA256,
                    "target": "x86_64-pc-windows-msvc", "configuration": "Release",
                    "options": dict(build.OPTIONS), "libraries": artifacts}
        def check(expected, m=None, r=None):
            m = copy.deepcopy(manifest if m is None else m)
            build.write_json(libdir / "neo-asr-receipt.json", receipt if r is None else r)
            m["receipt"] = build.record(libdir / "neo-asr-receipt.json")
            build.write_json(libdir / "neo-sherpa-asr.json", m)
            result = subprocess.run([str(validator), str(libdir)], capture_output=True, timeout=10)
            self.assertEqual(result.returncode, expected, result.stderr.decode(errors="replace"))
        check(0)
        fake = copy.deepcopy(manifest)
        fake_receipt = copy.deepcopy(receipt)
        fake["source_sha256"] = fake_receipt["source_lock"]["sha256"] = "a" * 64
        check(2, fake, fake_receipt)  # self-consistent but not the fixed source
        for key in ("url", "archive", "version", "sha256"):
            bad = copy.deepcopy(receipt)
            bad["source_lock"][key] = "wrong"
            check(2, r=bad)
        for key in ("url", "sha256"):
            bad = copy.deepcopy(receipt)
            bad["dependency_archives"]["eigen"][key] = "wrong"
            check(2, r=bad)
        bad = copy.deepcopy(receipt)
        del bad["dependency_archives"]["eigen"]
        check(2, r=bad)
        for path in [self.root / "graph.json", self.root / "omitted-source-links.json",
                     symbols / "onnxruntime.symbols.txt", license_path, libdir / "sherpa-onnx-core.lib"]:
            with self.subTest(entity=path.name):
                original = path.read_bytes()
                path.unlink()
                check(2)
                path.write_bytes(original + b"tampered")
                check(2)
                path.write_bytes(original)
        for unsafe in ("../LICENSE", "licenses/../../LICENSE", "licenses/C:/LICENSE",
                       "licenses\\sherpa\\LICENSE", "/licenses/LICENSE", "licenses/sherpa/./LICENSE",
                       "licenses/sherpa/LICENSE."):
            bad = copy.deepcopy(receipt)
            bad["licenses"] = {unsafe: build.record(license_path)}
            check(2, r=bad)
        for key in ("graph", "omitted_source_links"):
            bad = copy.deepcopy(receipt)
            bad[key]["sha256"] = "0" * 64
            check(2, r=bad)
        bad = copy.deepcopy(receipt)
        bad["symbol_reports"]["unexpected.symbols.txt"] = bad["symbol_reports"].pop("onnxruntime.symbols.txt")
        check(2, r=bad)
        bad = copy.deepcopy(receipt)
        bad["commands"]["build.command.json"]["returncode"] = 1
        check(2, r=bad)
        fake = copy.deepcopy(manifest)
        fake["options"]["SHERPA_ONNX_ENABLE_TTS"] = "ON"
        check(2, fake)
        extra = libdir / "espeak-ng.lib"
        extra.write_bytes(b"old")
        check(2)
        extra.unlink()
        fake = copy.deepcopy(manifest)
        bad = copy.deepcopy(receipt)
        fake["options"]["UNEXPECTED_OPTION"] = bad["options"]["UNEXPECTED_OPTION"] = "ON"
        check(2, fake, bad)
        for key in ("symbol_reports", "licenses"):
            bad = copy.deepcopy(receipt)
            bad[key] = {}
            check(2, r=bad)
        bad = copy.deepcopy(receipt)
        bad["unused_padding"] = "x" * (4 * 1024**2)
        check(2, r=bad)  # JSON allocation budget; not a multi-GB hash fixture
        check(0)

    def test_cmake_uses_official_checksum_and_no_unverified_execution(self):
        checksums = self.root / setup.CHECKSUMS
        checksums.write_text("a" * 64 + "  " + setup.ARCHIVE + "\n")
        with patch.object(setup, "CACHE", self.root), patch.object(build, "download", side_effect=ValueError("hash mismatch")) as fetch, patch.object(build, "extract") as extract, patch.object(build, "run") as run:
            with self.assertRaises(ValueError):
                setup.main()
            self.assertEqual(fetch.call_args.args[2], "a" * 64)
            extract.assert_not_called()
            run.assert_not_called()

    def wheel_fixture(self, setup, member="cmake/data/bin/cmake.exe"):
        name = f"cmake-{setup.VERSION}-py3-none-win_amd64.whl"
        archive = self.root / name
        with zipfile.ZipFile(archive, "w") as out:
            out.writestr(member, b"synthetic archive fixture, not executable")
        entry = {"filename": name, "packagetype": "bdist_wheel",
                 "url": "https://files.pythonhosted.org/packages/fixture/" + name,
                 "digests": {"sha256": build.digest(archive)}, "size": archive.stat().st_size}
        info = {"info": {"name": "cmake", "version": setup.VERSION}, "urls": [entry]}
        path = self.root / f"cmake-{setup.VERSION}-pypi.json"
        build.write_json(path, info)
        return archive, path, info

    def test_wheel_hash_failure_never_extracts_or_executes(self):
        archive, _, _ = self.wheel_fixture(setup)
        archive.write_bytes(b"tampered")
        with patch.object(setup, "CACHE", self.root), patch.object(build, "run") as run:
            with self.assertRaises(ValueError):
                setup.wheel()
            run.assert_not_called()
        self.assertFalse((self.root / f"cmake-{setup.VERSION}-wheel").exists())

    def test_wheel_rejects_traversal_before_extraction(self):
        self.wheel_fixture(setup, "../outside")
        with patch.object(setup, "CACHE", self.root), patch.object(build, "run") as run:
            with self.assertRaises(ValueError):
                setup.wheel()
            run.assert_not_called()
        self.assertFalse((self.root / f"cmake-{setup.VERSION}-wheel").exists())

    def test_wheel_rejects_nonofficial_host(self):
        _, path, info = self.wheel_fixture(setup)
        info["urls"][0]["url"] = "https://untrusted.invalid/cmake.whl"
        build.write_json(path, info)
        with patch.object(setup, "CACHE", self.root), patch.object(build, "download") as fetch:
            with self.assertRaises(ValueError):
                setup.wheel()
            fetch.assert_not_called()

    def test_rust_and_python_library_contract(self):
        source = (build.ROOT / "vendor/sherpa-onnx-sys/neo_asr.rs").read_text(encoding="utf-8")
        self.assertIn(build.SOURCE_SHA256, source)
        for name, (url, sha, _) in build.DEPS.items():
            for pin in (name, url, sha):
                self.assertIn('"' + pin + '"', source)
        for name in build.INSTALLED_LIBS:
            self.assertIn('"' + name + '"', source)
        for key, value in build.OPTIONS.items():
            self.assertRegex(source, r'\(\s*"' + key + r'",\s*"' + value + r'",?\s*\)')


if __name__ == "__main__":
    unittest.main()
