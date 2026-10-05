"""Synthetic local packager tests; no real build, network or GUI."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from . import package_combined as p
else:
    import package_combined as p


def pe_fixture(delay=False, machine=0x8664):
    data = bytearray(2048)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3c, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, machine, 1, 0, 0, 0, 240, 0)
    opt = 0x98
    struct.pack_into("<H", data, opt, 0x20b)
    struct.pack_into("<Q", data, opt + 24, 0x140000000)
    struct.pack_into("<I", data, opt + 108, 16)
    index, size = (13, 64) if delay else (1, 40)
    struct.pack_into("<II", data, opt + 112 + index * 8, 0x1000, size)
    struct.pack_into("<IIII", data, opt + 240 + 8, 1024, 0x1000, 1024, 512)
    if delay:
        struct.pack_into("<IIIIIIII", data, 512, 1, 0x1100, 0, 0, 0, 0, 0, 0)
    else:
        struct.pack_into("<IIIII", data, 512, 0, 0, 0, 0x1100, 0)
    data[768:781] = b"DirectML.dll\0"
    return bytes(data)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.lock = json.loads(p.LOCK_PATH.read_text(encoding="utf-8"))
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def test_lock_exact_commit_and_policy(self):
        self.assertEqual(self.lock["commit"], "ea1ecc87ec97717117f03625fd958c75cc2c0a49")
        self.assertEqual(self.lock["rust"], "1.97.1")
        self.assertEqual(self.lock["protocol"], 1)
        self.assertNotIn("public_approved", self.lock)
        self.assertEqual(self.lock["source_binding"]["source_commit"], self.lock["commit"])

    def test_strong_source_allowlist(self):
        for name in ("Cargo.lock", "drawing/src/main.rs", "crates/board-core/src/lib.rs"):
            self.assertTrue(p.source_allowed(name, self.lock), name)
        for name in ("AGENTS.md", "api/PRIVATE.md", "models/a.onnx", "target/private.rs", "drawing/src/private.zip",
                     "drawing/src/../private.rs", "crates/unknown/src/lib.rs", "distribution/legal/private.md", "drawing/src/C:/x.rs"):
            self.assertFalse(p.source_allowed(name, self.lock), name)

    def test_output_confined(self):
        allowed = self.root / "target/combined-evaluation"
        self.assertEqual(p.confined(allowed / "one", allowed), (allowed / "one").resolve())
        for path in (allowed, self.root / "dist", allowed / "../escape"):
            with self.assertRaises(ValueError):
                p.confined(path, allowed)

    def test_reject_release_and_unsafe_name_before_writing(self):
        for mode, name in (("release", "x"), ("evaluation", "../dist"), ("evaluation", "x/y")):
            with self.assertRaises(ValueError):
                p.assemble(argparse.Namespace(mode=mode, name=name))

    def test_cli_has_no_release_upload_output_option(self):
        for argv in (["--mode", "release"], ["--upload"], ["--output", "dist"]):
            with self.assertRaises(SystemExit):
                p.main(argv)

    def test_streaming_hash_and_copy(self):
        source = self.root / "source"
        source.write_bytes(b"hello")
        target = self.root / "package/a"
        p.copy_verified(source, target)
        self.assertEqual(p.digest(target), {"bytes": 5, "sha256": hashlib.sha256(b"hello").hexdigest()})

    def test_empty_payload_rejected(self):
        source = self.root / "empty"
        source.touch()
        with self.assertRaises(ValueError):
            p.copy_verified(source, self.root / "dest")

    def test_normal_and_delay_pe_imports(self):
        for delay in (False, True):
            self.assertEqual(p.PEImports(pe_fixture(delay)).imports(), ["directml.dll"])

    def test_bad_architecture_and_truncation(self):
        for data in (b"bad", pe_fixture(machine=0x14c), pe_fixture()[:600]):
            with self.assertRaises((ValueError, struct.error)):
                p.PEImports(data).imports()

    def test_dependencies_do_not_use_other_app_directory(self):
        package = self.root / "package"
        (package / "apps/drawing").mkdir(parents=True)
        (package / "apps/blackboard").mkdir()
        (package / "apps/drawing/neo-drawing.exe").write_bytes(pe_fixture())
        (package / "apps/blackboard/DirectML.dll").write_bytes(pe_fixture())
        report = p.audit_dependencies(package)
        row = report["images"]["apps/drawing/neo-drawing.exe"]["imports"][0]
        self.assertNotEqual(row["resolution"], "same-directory")
        self.assertFalse(report["clean_install_verified"])

    def test_build_receipt_binds_source_and_artifacts(self):
        for name in ("neo-drawing.exe", "neo-blackboard.exe", "DirectML.dll"):
            (self.root / name).write_bytes(b"binary")
        snapshot = {"tree_sha256": "tree", "source_commit": "commit"}
        receipt = {"returncode": 0, "source_tree_sha256": "tree", "source_commit": "commit", "artifacts": p.runtime_hashes(self.root)}
        p.validate_receipt(receipt, snapshot, self.root)
        (self.root / "neo-drawing.exe").write_bytes(b"changed")
        with self.assertRaises(ValueError):
            p.validate_receipt(receipt, snapshot, self.root)

    def test_explicit_directml_overrides_linked_build_output(self):
        for name in ("neo-drawing.exe", "neo-blackboard.exe"):
            (self.root / name).write_bytes(b"exe")
        real = self.root / "real.dll"
        real.write_bytes(b"dll")
        hashes = p.runtime_hashes(self.root, real)
        self.assertEqual(hashes["DirectML.dll"], p.digest(real))
        self.assertFalse((self.root / "DirectML.dll").exists())

    def test_complete_synthetic_assembly_layout_and_manifest(self):
        neo = self.root / "existing-neo.exe"
        neo.write_bytes(pe_fixture())
        runtime = self.root / "runtime"
        runtime.mkdir()
        for name in ("neo-drawing.exe", "neo-blackboard.exe", "DirectML.dll"):
            (runtime / name).write_bytes(pe_fixture())
        output_root = self.root / "target/combined-evaluation"
        snapshot = {"source_commit": self.lock["commit"], "tree_sha256": "synthetic", "files": {}, "checkout": {"dirty": True}}
        args = argparse.Namespace(mode="evaluation", name="synthetic", neo_exe=neo,
                                  drawing_repository=self.root, local_source=self.root,
                                  runtime_dir=runtime, directml=None, build_runtime=False, build_receipt=None)
        with patch.object(p, "OUTPUT_ROOT", output_root), \
             patch.object(p, "confined", side_effect=lambda path: Path(path).resolve()), \
             patch.object(p, "source_snapshot", return_value=(self.root, snapshot)), \
             patch.object(p, "pinned_names", return_value=[]), \
             patch.object(p, "neo_source_manifest", return_value={"commit": "synthetic", "dirty": True}):
            output = p.assemble(args)
        package = output / "package"
        self.assertTrue((output / "COMPLETE.txt").is_file())
        self.assertEqual(p.digest(package / "neo.exe"), p.digest(neo))
        files = json.loads((package / "FILES.sha256.json").read_text())
        for kind in ("drawing", "blackboard"):
            self.assertIn(f"apps/{kind}/neo-{kind}.exe", files)
            self.assertIn(f"apps/{kind}/DirectML.dll", files)
        self.assertFalse(any("vcruntime" in name or name.endswith(".zip") for name in files))
        manifest = json.loads((package / "SOURCE.json").read_text())
        self.assertFalse(manifest["public_approved"])
        self.assertEqual(manifest["native_tts_removed"], "unknown")
        self.assertEqual(manifest["existing_neo_gpl_risk"]["fixed"], "unknown")
        self.assertFalse(manifest["runtime_build"]["reproducible_build_verified"])
        self.assertNotIn("NOT FIXED", (package / "PREREQUISITES.txt").read_text(encoding="utf-8"))
        self.assertEqual(manifest["distribution_review"], "unreviewed")
        self.assertEqual(manifest["neo_exe"], p.digest(neo))
        self.assertNotIn(str(self.root), (package / "SOURCE.json").read_text())

    def native_fixture(self):
        def record(path):
            value = p.digest(path)
            return {"path": path.relative_to(self.root).as_posix(), "sha256": value["sha256"], "size": value["bytes"]}
        def save(name, value):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            p.write_json(path, value)
            return record(path)
        (self.root / "Cargo.toml").write_text('[patch.crates-io]\nsherpa-onnx-sys = { path = "vendor/sherpa-onnx-sys" }\n')
        (self.root / "Cargo.lock").write_text("synthetic lock")
        self.native_neo = self.root / "neo.exe"
        self.native_neo.write_bytes(pe_fixture())
        (self.root / "neo.map").write_text("synthetic MAP; audit parser mocked only for these fixture tests")
        source = dict(commit=p.build_sherpa_asr.COMMIT, sha256=p.build_sherpa_asr.SOURCE_SHA256)
        graph = save("graph.json", [dict(name="sherpa-onnx-core", sources=[
            "offline-sense-voice-model.cc", "silero-vad-model.cc", "voice-activity-detector.cc"]),
            dict(name="sherpa-onnx-c-api", sources=["c-api.cc"])])
        libraries = {}
        libdir = self.root / "lib"
        libdir.mkdir()
        for name in p.build_sherpa_asr.INSTALLED_LIBS:
            path = libdir / (name + ".lib")
            path.write_bytes(b"!<arch>\nsynthetic archive")
            libraries[path.name] = {k: v for k, v in record(path).items() if k != "path"}
        receipt = save("lib/receipt.json", dict(source_lock=source, options=p.build_sherpa_asr.OPTIONS, graph=graph))
        manifest = save("lib/manifest.json", dict(schema=1, status="native-validated", version=p.build_sherpa_asr.VERSION,
            source_commit=source["commit"], source_sha256=source["sha256"], target="x86_64-pc-windows-msvc",
            configuration="Release", options=p.build_sherpa_asr.OPTIONS, receipt=receipt, libraries=libraries))
        self.native_report = dict(exe=record(self.native_neo), map=record(self.root / "neo.map"), errors=[],
            export_findings=[], map_findings=[], map_header_consistent=True, map_symbol_count=2, verdict="inconclusive",
            asr_vad=[dict(symbol=symbol, mapped=True, executable=True) for symbol in sorted(p.audit_native_link.ASR_VAD)])
        save("report.json", self.native_report)
        self.native_status = dict(source=source, native_manifest=manifest, native_receipt=receipt, native_graph=graph,
            native_libraries=libraries, neo_release=record(self.native_neo), map=record(self.root / "neo.map"),
            map_pe_audit=dict(path="report.json", errors=[], risk_findings=0, asr_vad=self.native_report["asr_vad"],
                              map_symbols=2, verdict="inconclusive"), root_patch_enabled=True,
            root_files={name: record(self.root / name) for name in ("Cargo.toml", "Cargo.lock")})
        save("status.json", self.native_status)
        self.native_status_path = self.root / "status.json"

    def validate_native_fixture(self):
        with patch.object(p, "ROOT", self.root), patch.object(p.audit_native_link, "audit", return_value=self.native_report):
            return p.validate_native_receipt(self.native_status_path, self.native_neo)

    def test_native_missing_evidence_is_unknown(self):
        result = p.validate_native_receipt(None, self.root / "never-executed.exe")
        self.assertEqual(result["native_tts_removed"], "unknown")
        self.assertFalse(result["legal_approved"])

    def test_native_actual_hashes_and_audit_are_technical_not_legal(self):
        self.native_fixture()
        result = self.validate_native_fixture()
        self.assertIs(result["native_tts_removed"], True)
        self.assertFalse(result["legal_approved"])
        self.assertFalse(result["reproducible_build_verified"])
        self.assertEqual(result["audit_verdict"], "inconclusive")
        self.assertEqual(result["evidence"]["map_pe_report"], p.digest(self.root / "report.json"))
        self.assertTrue(result["source_association"]["root_files_match_status"])
        self.assertTrue(result["source_association"]["root_patch_enabled_current"])

    def test_native_wrong_exe_and_tampered_evidence_rejected(self):
        self.native_fixture()
        for name in ("neo.exe", "neo.map", "lib/manifest.json", "lib/receipt.json", "graph.json", "lib/onnxruntime.lib"):
            path = self.root / name
            before = path.read_bytes()
            path.write_bytes(before + b"tampered")
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "mismatch"):
                self.validate_native_fixture()
            path.write_bytes(before)
        other = self.root / "other.exe"
        other.write_bytes(b"old binary")
        with patch.object(p, "ROOT", self.root), self.assertRaisesRegex(ValueError, "different Neo"):
            p.validate_native_receipt(self.native_status_path, other)

    def test_native_forged_report_and_nonzero_findings_rejected(self):
        self.native_fixture()
        original = copy.deepcopy(self.native_report)
        for key, value in (("map_findings", [{"family": "espeak"}]), ("errors", ["unpaired"]),
                           ("asr_vad", []), ("map_header_consistent", False), ("map_symbol_count", 0)):
            self.native_report = dict(original, **{key: value})
            p.write_json(self.root / "report.json", self.native_report)
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.validate_native_fixture()
        self.native_report = original
        p.write_json(self.root / "report.json", dict(original, map_symbol_count=999))
        with self.assertRaisesRegex(ValueError, "fresh EXE/MAP"):
            self.validate_native_fixture()

    def test_native_incomplete_status_and_extra_lib_rejected(self):
        self.native_fixture()
        (self.root / "lib/espeak.lib").write_bytes(b"!<arch>\nextra")
        with self.assertRaisesRegex(ValueError, "library set"):
            self.validate_native_fixture()
        (self.root / "lib/espeak.lib").unlink()
        for key in ("native_manifest", "native_graph", "map_pe_audit", "root_files"):
            status = dict(self.native_status)
            del status[key]
            p.write_json(self.native_status_path, status)
            with self.subTest(key=key), self.assertRaises((KeyError, ValueError)):
                self.validate_native_fixture()

    def test_native_graph_and_options_rejected_even_with_updated_hashes(self):
        self.native_fixture()
        graph_path = self.root / "graph.json"

        for graph in ([], [dict(name="sherpa-onnx-core", sources=["offline-tts.cc"])]):
            p.write_json(graph_path, graph)
            value = p.digest(graph_path)
            self.native_status["native_graph"].update(sha256=value["sha256"], size=value["bytes"])
            receipt_path = self.root / "lib/receipt.json"
            receipt = json.loads(receipt_path.read_text())
            receipt["graph"] = self.native_status["native_graph"]
            p.write_json(receipt_path, receipt)
            value = p.digest(receipt_path)
            self.native_status["native_receipt"].update(sha256=value["sha256"], size=value["bytes"])
            manifest_path = self.root / "lib/manifest.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["receipt"] = self.native_status["native_receipt"]
            p.write_json(manifest_path, manifest)
            value = p.digest(manifest_path)
            self.native_status["native_manifest"].update(sha256=value["sha256"], size=value["bytes"])
            p.write_json(self.native_status_path, self.native_status)
            with self.assertRaisesRegex(ValueError, "graph"):
                self.validate_native_fixture()
        manifest["options"]["SHERPA_ONNX_ENABLE_TTS"] = "ON"
        p.write_json(manifest_path, manifest)
        value = p.digest(manifest_path)
        self.native_status["native_manifest"].update(sha256=value["sha256"], size=value["bytes"])
        p.write_json(self.native_status_path, self.native_status)
        with self.assertRaisesRegex(ValueError, "options"):
            self.validate_native_fixture()

    def test_native_root_drift_not_mislabeled_source_binding(self):
        self.native_fixture()
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        result = self.validate_native_fixture()
        self.assertTrue(result["native_tts_removed"])
        self.assertFalse(result["source_association"]["root_files_match_status"])
        self.assertFalse(result["source_association"]["root_patch_enabled_current"])
        self.assertIn("NOT a complete", result["source_association"]["binary_source_binding"])

    def test_native_path_escape_rejected(self):
        with patch.object(p, "ROOT", self.root):
            for name in ("../outside", "C:\\outside", "/absolute", "..\\outside"):
                with self.subTest(name=name), self.assertRaises(ValueError):
                    p.native_evidence_path(name)

    def test_legal_is_exact_allowlist_not_recursive(self):
        files = ["LICENSE", "distribution/legal/app/private.md", "distribution/legal/app/native/source.zip"]
        for name in files:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("legal text", encoding="utf-8")
        destination = self.root / "selected"
        with patch.object(p, "pinned_names", return_value=files):
            report = p.collect_legal(self.root, self.root, self.lock, destination)
        self.assertEqual([x.relative_to(destination).as_posix() for x in destination.rglob("*") if x.is_file()], ["LICENSE"])
        self.assertTrue(report["missing_allowlisted_texts"])
        self.assertEqual(report["status"], "unreviewed")
        self.assertFalse(report["public_approved"])

    def test_hash_named_legal_text_verified(self):
        name = "distribution/legal/app/rust/texts/" + "a" * 64 + ".txt"
        path = self.root / name
        path.parent.mkdir(parents=True)
        path.write_text("wrong digest", encoding="utf-8")
        lock = dict(self.lock, legal_files=[])
        with patch.object(p, "pinned_names", return_value=[name]), self.assertRaises(ValueError):
            p.collect_legal(self.root, self.root, lock, self.root / "out")

    def test_snapshot_hash_changes_with_dirty_source(self):
        names = ["Cargo.toml", "Cargo.lock", "LICENSE", "drawing/src/main.rs", "PRIVATE.md"]
        for name in names:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name, encoding="utf-8")
        with patch.object(p, "pinned_names", return_value=names), patch.object(p, "revision", return_value={"commit": self.lock["commit"], "dirty": True}):
            _, before = p.source_snapshot(self.root, self.lock, self.root)
            (self.root / "drawing/src/main.rs").write_text("changed", encoding="utf-8")
            _, after = p.source_snapshot(self.root, self.lock, self.root)
        self.assertTrue(after["checkout"]["dirty"])
        self.assertNotIn("PRIVATE.md", after["files"])
        self.assertNotEqual(before["tree_sha256"], after["tree_sha256"])

    def make_bash_inventory(self):
        # Inventory helper tests isolate the fixed-policy verifier (tested separately).
        verifier = patch.object(p.distribution, 'verify_distribution', return_value={})
        verifier.start()
        self.addCleanup(verifier.stop)
        root = self.root / "selected/runtime/gitbash"
        names = ["LICENSE.txt", "cmd/git.exe", "usr/bin/bash.exe", "usr/bin/sh.exe", "etc/package-versions.txt",
                 "usr/libexec/getprocaddr32.exe", "usr/share/empty"]
        for name in names:
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"" if name.endswith("empty") else name.encode())
        files = [{"path": n, "size": (root / n).stat().st_size, "sha256": p.digest(root / n)["sha256"]} for n in names]
        inventory = self.root / "selected/MANIFEST.json"
        p.write_json(inventory, {"runtime": {"files": files, "package_versions_sha256": p.digest(root / "etc/package-versions.txt")["sha256"]}})
        return root, inventory

    def test_gitbash_inventory_exact_files_hashes_no_extras(self):
        root, inventory = self.make_bash_inventory()
        files, info = p.gitbash_plan(root, inventory)
        self.assertEqual(len(files), 7)
        self.assertFalse(info["official_archive_authentication"])
        self.assertFalse(info["executed"])
        extra = root / "usr/bin/personal.txt"
        extra.write_text("secret")
        with self.assertRaisesRegex(ValueError, "extra/missing"):
            p.gitbash_plan(root, inventory)
        extra.unlink()
        (root / "etc/gitconfig").write_text("personal")
        with self.assertRaises(ValueError):
            p.gitbash_plan(root, inventory)
        (root / "etc/gitconfig").unlink()
        (root / "usr/bin/bash.exe").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "content mismatch"):
            p.gitbash_plan(root, inventory)

    def test_gitbash_rejects_unsafe_manifest_paths_and_crt(self):
        root, inventory = self.make_bash_inventory()
        original = json.loads(inventory.read_text())
        for name in ("../outside", "/absolute", "usr/bin/NUL.txt", "usr/bin/a:stream", "usr/bin/end.",
                     "usr/bin/vcruntime140.dll", "home/user.txt", "usr/.ssh/id_rsa", "usr//file"):
            entry = dict(original["runtime"]["files"][0], path=name)
            modified = json.loads(json.dumps(original))
            modified["runtime"]["files"].append(entry)
            p.write_json(inventory, modified)
            with self.subTest(name=name), self.assertRaises(ValueError):
                p.gitbash_plan(root, inventory)

    def test_descriptive_readmes_are_not_resource_or_legal_inputs(self):
        for name in ("docs/README.md", "docs/distribution/README.md", "docs/native-build.md"):
            self.assertNotIn(name, p.LOCAL_RESOURCES)
            self.assertNotIn(name, p.LOCAL_RESOURCES.values())
        for name in ("README.md", "assets/README.md", "runtime/README.md", "models/README.md",
                     "supplemental/README.md", "runtime/gcm/README.md"):
            self.assertNotIn(name, p.PUBLIC_LEGAL)
        for name in ("cargo-notices.txt", "models/hi_neo-model.json", "models/hi_neo-MIT.txt",
                     "models/sensevoice-model-card.md", "models/piper-libritts-high-MODEL_CARD",
                     "models/sherpa-sense-README.md", "runtime/gitbash-SOURCE.md", "supplemental/index.json"):
            self.assertIn(name, p.PUBLIC_LEGAL)

    def test_public_legal_selection_excludes_private_and_archives(self):
        legal = self.root / "docs/licenses"
        for name in p.PUBLIC_LEGAL:
            path = legal / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("legal text", encoding="utf-8")
        text = b"synthetic license"
        hashed = hashlib.sha256(text).hexdigest()
        (legal / "supplemental" / (hashed + ".txt")).write_bytes(text)
        p.write_json(legal / "supplemental/index.json", {"packages": [{"files": [{"path": hashed + ".txt", "sha256": hashed}]}]})
        files_without_descriptions = p.public_legal_plan(self.root)
        excluded = ("private.md", "source.zip", "private.json", "supplemental/unlisted.txt",
                    "README.md", "assets/README.md", "runtime/README.md", "models/README.md",
                    "supplemental/README.md", "runtime/gcm/README.md")
        for name in excluded:
            path = legal / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("exclude")
        files = p.public_legal_plan(self.root)
        self.assertEqual(files, files_without_descriptions)
        for name in excluded:
            self.assertNotIn("docs/licenses/" + name, files)
        self.assertEqual(len(files), len(p.PUBLIC_LEGAL) + 1)
        self.assertIn("docs/licenses/models/FunASR-MODEL_LICENSE", files)
        self.assertFalse(any("private" in n or n.endswith(".zip") or "unlisted" in n for n in files))

    def test_resource_plan_copies_all_records_and_keeps_local_provenance(self):
        root, inventory = self.make_bash_inventory()
        for destination, source in p.LOCAL_RESOURCES.items():
            path = self.root / source
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('{"hello":"hello"}' if destination.endswith(".lang") else "synthetic")
        for name in (*p.distribution.DISTRIBUTION_DOCUMENTS, 'source-manifest.json', 'source-companion-record.json'):
            if name != 'MANIFEST.json':
                (inventory.parent / name).write_text('synthetic marker')
        with patch.object(p, "public_legal_plan", return_value={}), \
                patch.object(p, 'validate_hi_neo'), \
                patch.object(p.distribution, 'verify_companion', return_value={'path': 'source.zip', 'sha256': '0' * 64, 'size': 1}):
            plan, info = p.local_resource_plan(self.root, inventory.parent)
        self.assertNotIn("docs/README.md", plan)
        self.assertTrue(info['gitbash']['source_companion_binding_verified'])
        self.assertTrue(all('.cache/runtime' not in str(path) for path in plan.values()))
        package = self.root / "package"
        p.copy_local_resources(plan, info, package)
        self.assertEqual(len(p.tree_files(package)), len(plan))
        self.assertIn("NOT CI", info["model_provenance"])
        self.assertFalse(info["models_executed"])
        self.assertEqual(p.digest(package / "runtime/gitbash/usr/share/empty")["bytes"], 0)
        for name, record in info["records"].items():
            self.assertEqual(p.digest(package / name), record)
        with self.assertRaisesRegex(ValueError, "overwrite"):
            p.copy_local_resources(plan, info, package)

    def test_local_resources_require_explicit_filtered_distribution(self):
        with self.assertRaisesRegex(ValueError, 'no raw cache fallback'):
            p.local_resource_plan(self.root, None)
        with self.assertRaises(SystemExit):
            p.main(['--mode', 'evaluation', '--name', 'test', '--neo-exe', 'inert', '--include-local-resources'])

    def test_resource_copy_detects_changes_since_preflight(self):
        source = self.root / "model"
        source.write_bytes(b"original")
        info = {"records": {"resources/model.onnx": p.digest(source)}}
        source.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "after preflight"):
            p.copy_local_resources({"resources/model.onnx": source}, info, self.root / "out")

    def test_dependency_scope_excludes_mingit_x86(self):
        package = self.root / "package"
        (package / "runtime/gitbash/usr/libexec").mkdir(parents=True)
        (package / "runtime/gitbash/usr/libexec/getprocaddr32.exe").write_bytes(pe_fixture(machine=0x14c))
        (package / "runtime/onnx").mkdir()
        (package / "runtime/onnx/onnxruntime.dll").write_bytes(pe_fixture())
        report = p.audit_dependencies(package)
        self.assertEqual(set(report["images"]), {"runtime/onnx/onnxruntime.dll"})
        self.assertIn("NOT PE-audited", report["excluded"]["runtime/gitbash"])

    def test_full_file_manifest_detects_additions_and_modifications(self):
        package = self.root / "package"
        package.mkdir()
        (package / "data").write_bytes(b"test")
        p.write_json(package / "FILES.sha256.json", {"data": p.digest(package / "data")})
        summary = p.verify_file_manifest(package)
        self.assertEqual(summary["file_count"], 2)
        (package / "extra").write_text("private")
        with self.assertRaisesRegex(ValueError, "file set"):
            p.verify_file_manifest(package)
        (package / "extra").unlink()
        (package / "data").write_bytes(b"modified")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            p.verify_file_manifest(package)

    def test_unpinned_rust_source_rejected(self):
        for name in ("Cargo.toml", "Cargo.lock", "LICENSE"):
            (self.root / name).write_text("x")
        extra = self.root / "drawing/src/private.rs"
        extra.parent.mkdir(parents=True)
        extra.write_text("private")
        with patch.object(p, "pinned_names", return_value=["Cargo.toml", "Cargo.lock", "LICENSE"]), self.assertRaises(ValueError):
            p.source_snapshot(self.root, self.lock, self.root)


if __name__ == "__main__":
    unittest.main()
