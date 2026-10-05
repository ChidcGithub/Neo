"""Offline synthetic release regressions; no Cargo/network/GUI or real approvals."""
import copy

import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from . import assemble_drawing_release as a

else:
    import assemble_drawing_release as a



def pe_fixture(delay=False, machine=0x8664):
    # Local fixture avoids importing the unrelated evaluation test module.
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


def sha(data):
    return hashlib.sha256(data).hexdigest()


class LockTests(unittest.TestCase):
    def test_package_and_direct_entrypoints_without_pythonpath(self):
        env = dict(os.environ)
        env.pop("PYTHONPATH", None)
        for args in (["-m", "tools.assemble_drawing_release"], ["tools/assemble_drawing_release.py"]):
            result = subprocess.run([sys.executable, "-B", *args, "verify-source"], cwd=a.ROOT,
                                    env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 1)
            self.assertIn("--source is required", result.stderr)
            self.assertNotIn("ImportError", result.stderr)
        for args in (["-m", "unittest", "tools.test_assemble_drawing_release.LockTests.test_real_lock_stays_pinned_without_approval_fields"],
                     ["tools/test_assemble_drawing_release.py", "LockTests.test_real_lock_stays_pinned_without_approval_fields"]):
            result = subprocess.run([sys.executable, "-B", *args], cwd=a.ROOT, env=env,
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_real_lock_stays_pinned_without_approval_fields(self):
        lock = a.load_lock()
        self.assertEqual(lock["commit"], "8c32db0d2e5aa55dca774cf1c06547710ce8d2c2")
        self.assertEqual(lock["rust"], "1.97.1")
        for key in ("public_approved", "distribution_review", "review"):
            self.assertNotIn(key, lock)
        self.assertEqual(set(lock["source_binding"]), {
            "source_commit", "source_tree_sha256", "cargo_lock_sha256", "version", "legal_files"})
        self.assertEqual(lock["source_binding"]["source_commit"], lock["commit"])
        legal = lock["source_binding"]["legal_files"]
        self.assertEqual(len(legal), 312)
        self.assertLessEqual(set(lock["legal_files"]), legal.keys())
        for suffix in ("APACHE", "BSD", "MINPACK", "MPL2", "README"):
            name = "distribution/legal/app/native/ort-eigen/eigen-COPYING." + suffix
            self.assertNotIn(name, lock["legal_files"])
            self.assertNotIn(name, legal)
        self.assertTrue(any("docs/licenses/runtime/native-sources/eigen-1d8b82b-COPYING.*" in text
                            for text in lock["limitations"]))

    def test_lock_outputs_validate_before_writing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            lock_path, output = root / "lock.json", root / "outputs"
            for commit in ("main", "ea1ecc", "a" * 40 + "\ninjected=true"):
                a.write_json(lock_path, dict(a.load_lock(), commit=commit))
                with patch.dict(os.environ, GITHUB_OUTPUT=str(output)):
                    self.assertEqual(a.main(["lock-outputs", "--lock", str(lock_path)]), 1)
                self.assertFalse(output.exists())
            a.write_json(lock_path, a.load_lock())
            with patch.dict(os.environ, GITHUB_OUTPUT=str(output)):
                self.assertEqual(a.main(["lock-outputs", "--lock", str(lock_path)]), 0)
            self.assertEqual(output.read_text().splitlines(), [
                "commit=8c32db0d2e5aa55dca774cf1c06547710ce8d2c2",
                "repository=ChidcGithub/NeoRuntime-drawing", "rust=1.97.1"])

    def test_duplicate_json_keys_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "lock.json"
            path.write_text('{"schema": 1, "schema": 1}')
            with self.assertRaisesRegex(ValueError, "Duplicate"):
                a.load_lock(path)


@unittest.skipUnless(shutil.which("git"), "git is not installed")
class AssemblyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.target = self.root / "build"
        self.runtime = self.target / a.TARGET / "release"
        self.package = self.root / "package"
        self.package.mkdir()
        self.lock_path = self.root / "lock.json"
        self.lock = a.load_lock()

        self.put("Cargo.toml", b'[workspace.package]\nversion = "0.0.1"\n')
        self.put("Cargo.lock", b"# synthetic dependency lock\n")
        self.put("drawing/src/main.rs", b"fn main() {}\n")
        self.put(".gitignore", b"/target/\n/dist/\n/.cache/\n")
        native = "distribution/legal/app/native/"
        evidence = {"source_url": "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg",
                    "package_sha256": "a" * 64, "member": "bin/x64-win/DirectML.dll",
                    "member_sha256": sha(pe_fixture()), "release_dll_sha256": sha(pe_fixture()), "identical": True}
        self.put(native + "directml/nuget-evidence.json", json.dumps(evidence).encode())
        self.put(native + "directml/Microsoft.AI.DirectML.nuspec",
                 b'<package xmlns="http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd"><metadata><id>Microsoft.AI.DirectML</id><version>1.15.4</version></metadata></package>')
        self.put(native + "ort-sys/dist.txt", ("none\t" + a.TARGET + "\thttps://example.invalid/ort.tgz\t" + "B" * 64 + "\n").encode())
        self.legal = b"SYNTHETIC legal body, not a real approval\r\n"
        for name in self.lock["legal_files"]:
            self.put(name, self.legal)
        self.rust_text = "distribution/legal/app/rust/texts/" + sha(self.legal) + ".txt"
        self.put(self.rust_text, self.legal)

        for name in ("PRIVATE.md", "models/private.onnx", "distribution/legal/app/native/private-evidence.json",
                     "distribution/legal/model/REVIEWED.md", "target/secret.exe"):
            self.put(name, b"must never ship")
        self.git("init", "-q")
        self.git("config", "user.name", "Synthetic Tests")
        self.git("config", "user.email", "synthetic@example.invalid")
        self.git("config", "core.autocrlf", "false")
        self.commit()
        self.runtime.mkdir(parents=True)
        for name in a.ARTIFACTS:
            (self.runtime / name).write_bytes(pe_fixture())
        self.bind()

    def git(self, *args):
        env = dict(os.environ, GIT_EDITOR="true", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        return subprocess.check_output(["git", "--no-pager", "-C", str(self.source), *args], env=env, stderr=subprocess.STDOUT).decode().strip()

    def put(self, name, data):
        path = self.source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def commit(self):
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", "Synthetic fixture")
        self.lock["commit"] = self.git("rev-parse", "HEAD")

    def bind(self):
        info, names = a.source_info(self.source, self.lock)
        legal = set(self.lock["legal_files"]) | {n for n in names if n == self.rust_text}
        self.lock["source_binding"] = {key: info[key] for key in ("source_commit", "source_tree_sha256", "cargo_lock_sha256", "version")}
        self.lock["source_binding"]["legal_files"] = {
            name: sha(a.pinned_bytes(self.source, self.lock, name)) for name in sorted(legal & names)}
        a.write_json(self.lock_path, self.lock)
        a.write_json(self.target / "drawing-build.json", {
            "schema": 1, "source": info, "lock_sha256": a.digest(self.lock_path)["sha256"],
            "command": a.build_command(self.lock, self.target),
            "toolchain": {tool: tool + " 1.97.1 (synthetic)" for tool in ("rustc", "cargo")},
            "version_output": {name: "Synthetic app 0.0.1" for name in a.ARTIFACTS[:2]},
            "artifacts": a.audit_runtime(self.runtime),
            "directml_provenance": a.directml_provenance(self.source, self.lock),
        })

    def assemble(self):
        return a.assemble(self.source, self.target, self.package, self.lock_path, self.root)

    def test_synthetic_assembly_exact_layout_hashes_and_legal_bodies(self):
        hashes = self.assemble()
        for kind in ("drawing", "blackboard"):
            folder = self.package / "apps" / kind
            self.assertEqual({p.name for p in folder.iterdir()}, {"neo-" + kind + ".exe", "DirectML.dll", "MSVC-PREREQUISITE.txt"})
            self.assertIn("official Microsoft Visual C++ x64", (folder / "MSVC-PREREQUISITE.txt").read_text())
        legal_root = self.package / "docs/licenses/NeoRuntime-drawing"
        for name in [*self.lock["legal_files"], self.rust_text]:
            self.assertEqual((legal_root / name).read_bytes(), self.legal)
        manifest = a.read_json(legal_root / "SOURCE.json")
        self.assertEqual(manifest["source_commit"], self.lock["commit"])
        self.assertEqual(manifest["lock_sha256"], a.digest(self.lock_path)["sha256"])
        self.assertFalse(manifest["clean_install_verified"])
        self.assertEqual(manifest["legal_files"], self.lock["source_binding"]["legal_files"])
        for key in ("review", "reviewed_sha256", "public_approved", "distribution_review"):
            self.assertNotIn(key, manifest)
        self.assertEqual(a.read_json(legal_root / "FILES.sha256.json"), hashes)
        for name, value in hashes.items():
            self.assertEqual(a.digest(self.package / name), value)
        for path in self.package.rglob("*"):
            self.assertNotIn("private", path.name.lower())
            self.assertNotEqual(path.name, "secret.exe")
            self.assertFalse(path.name.lower().startswith(("vcruntime", "msvcp")))
        with self.assertRaisesRegex(ValueError, "must be new"):
            self.assemble()

    def test_assembly_without_main_policy_or_reviewed_marker(self):
        self.assertFalse((self.root / "tools/distribution-review.json").exists())
        self.assertFalse((self.source / "distribution/legal/app/REVIEWED.md").exists())
        self.assemble()
        self.assertFalse(any(p.name == "REVIEWED.md" for p in self.package.rglob("*")))

    def test_legacy_approval_flags_do_not_gate_assembly(self):
        self.lock.update(public_approved=False, distribution_review="unreviewed")
        self.bind()
        self.assemble()

    def test_missing_pinned_legal_cannot_be_supplied_by_local_file(self):
        name = self.lock["legal_files"][-1]
        (self.source / name).unlink()
        self.commit()
        self.bind()
        with self.assertRaisesRegex(ValueError, "Missing pinned legal text") as error:
            a.verify_source(self.source, self.lock)
        self.assertIn(name, str(error.exception))
        self.put(name, self.legal)
        with self.assertRaisesRegex(ValueError, "must be clean"):
            a.verify_source(self.source, self.lock)
        self.git("config", "core.excludesFile", str(self.root / "ignored-legal"))
        (self.root / "ignored-legal").write_text(name + "\n")
        self.assertEqual(self.git("status", "--porcelain=v1", "--untracked-files=all"), "")
        with self.assertRaisesRegex(ValueError, "Missing pinned legal text"):
            self.assemble()
        self.assertEqual(list(self.package.iterdir()), [])

    def test_source_binding_schema_is_required(self):
        for binding in (None, True, {}, {"status": "APPROVED"}):
            with self.subTest(binding=binding), self.assertRaisesRegex(ValueError, "source binding"):
                a.verify_source(self.source, dict(self.lock, source_binding=binding))

    def test_source_binding_and_legal_digest_tampering(self):
        for key in ("source_commit", "source_tree_sha256", "cargo_lock_sha256", "version"):
            lock = copy.deepcopy(self.lock)
            lock["source_binding"][key] = "0" * (40 if key == "source_commit" else 64)
            with self.subTest(key=key), self.assertRaises(ValueError):
                a.verify_source(self.source, lock)
        lock = copy.deepcopy(self.lock)
        lock["source_binding"]["legal_files"]["LICENSE"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "Legal manifest hash"):
            a.verify_source(self.source, lock)
        for extra in (False, True):
            lock = copy.deepcopy(self.lock)
            if extra:
                lock["source_binding"]["legal_files"]["PRIVATE.md"] = "0" * 64
            else:
                del lock["source_binding"]["legal_files"]["LICENSE"]
            with self.subTest(extra=extra), self.assertRaisesRegex(ValueError, "exactly match allowlist"):
                a.verify_source(self.source, lock)

    def test_wrong_pinned_commit_is_rejected(self):
        lock = dict(self.lock, commit="0" * 40)
        with self.assertRaisesRegex(ValueError, "checkout differs from lock"):
            a.source_info(self.source, lock)

    def test_empty_or_misnamed_rust_legal_blob_is_rejected(self):
        for body, message in ((b" \r\n", "Empty legal text"), (b"changed legal text\r\n", "hash filename")):
            self.put(self.rust_text, body)
            self.commit()
            self.bind()
            with self.subTest(body=body), self.assertRaisesRegex(ValueError, message):
                self.assemble()
            self.assertEqual(list(self.package.iterdir()), [])

    def test_dirty_source_or_changed_binary_blocks(self):
        self.put("drawing/src/main.rs", b"changed")
        with self.assertRaisesRegex(ValueError, "clean"):
            self.assemble()
        self.put("drawing/src/main.rs", b"fn main() {}\n")
        (self.runtime / "neo-drawing.exe").write_bytes(pe_fixture(delay=True))
        with self.assertRaisesRegex(ValueError, "artifact mismatch"):
            self.assemble()
        self.assertEqual(list(self.package.iterdir()), [])

    def test_tracked_legal_edit_and_stale_source_receipt_are_rejected(self):
        name = self.lock["legal_files"][-1]
        self.put(name, b"changed legal body")
        with self.assertRaisesRegex(ValueError, "must be clean"):
            self.assemble()
        self.put(name, self.legal)
        self.put("drawing/src/main.rs", b"fn main() { /* changed source */ }\n")
        self.commit()
        info, _ = a.source_info(self.source, self.lock)
        for key in ("source_commit", "source_tree_sha256", "cargo_lock_sha256", "version"):
            self.lock["source_binding"][key] = info[key]
        a.write_json(self.lock_path, self.lock)
        with self.assertRaisesRegex(ValueError, "receipt source/lock mismatch"):
            self.assemble()
        self.assertEqual(list(self.package.iterdir()), [])

    def test_evaluation_or_wrong_toolchain_receipt_is_rejected(self):
        path = self.target / "drawing-build.json"
        original = a.read_json(path)
        for key, value in (("schema", True), ("source", {}), ("lock_sha256", "0" * 64),
                           ("command", ["cargo", "build", "--offline"]),
                           ("toolchain", {"rustc": "rustc 1.96.0 (wrong)", "cargo": "cargo 1.97.1 (synthetic)"}),
                           ("version_output", {"neo-drawing.exe": "Synthetic app 0.0.1"})):
            a.write_json(path, {**original, key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.assemble()
            self.assertEqual(list(self.package.iterdir()), [])

    def test_pe_rejects_missing_directml_wrong_arch_and_missing_import(self):
        dll = self.runtime / "DirectML.dll"
        dll.unlink()
        with self.assertRaises((ValueError, OSError)):
            a.audit_runtime(self.runtime)
        dll.write_bytes(pe_fixture(machine=0x14c))
        with self.assertRaisesRegex(ValueError, "x64"):
            a.audit_runtime(self.runtime)
        dll.write_bytes(pe_fixture())
        exe = self.runtime / "neo-drawing.exe"
        data = bytearray(pe_fixture())
        struct.pack_into("<II", data, 0x98 + 112 + 8, 0, 0)
        exe.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "actually import"):
            a.audit_runtime(self.runtime)
        exe.write_bytes(pe_fixture(delay=True))
        self.assertIn("directml.dll", a.audit_runtime(self.runtime)[exe.name]["imports"])

    def test_symlink_payload_rejected(self):
        dll = self.runtime / "DirectML.dll"
        content = dll.read_bytes()
        dll.unlink()
        for parent, message in ((self.runtime, "Symlink"),
                                (self.root, "Path escapes repository")):
            with self.subTest(target_parent=parent):
                other = parent / "other.dll"
                other.write_bytes(content)
                try:
                    dll.symlink_to(other)
                except OSError:
                    self.skipTest("Symlink privilege unavailable")
                try:
                    with self.assertRaisesRegex(ValueError, message):
                        a.audit_runtime(self.runtime)
                finally:
                    dll.unlink()

    def test_source_binding_has_no_commit_self_reference_and_ignored_output_is_clean(self):
        before, _ = a.verify_source(self.source, self.lock)
        self.put("target/package/generated.txt", b"ignored")
        self.put("dist/generated.txt", b"ignored")
        self.put(".cache/generated.txt", b"ignored")
        self.assertEqual(a.verify_source(self.source, self.lock)[0], before)
        self.put("untracked-source.rs", b"not ignored")
        with self.assertRaisesRegex(ValueError, "clean"):
            a.source_info(self.source, self.lock)

    def test_pinned_provenance_ignores_uncommitted_evidence_but_checks_version(self):
        expected = a.directml_provenance(self.source, self.lock)
        path = "distribution/legal/app/native/directml/nuget-evidence.json"
        self.put(path, b"local invalid evidence")
        self.assertEqual(a.directml_provenance(self.source, self.lock), expected)
        with self.assertRaisesRegex(ValueError, "clean"):
            a.source_info(self.source, self.lock)
        data = json.loads(self.git("show", "HEAD:" + path))
        data["source_url"] = data["source_url"].replace("1.15.4", "1.99.0")
        self.put(path, json.dumps(data).encode())
        self.commit()
        with self.assertRaisesRegex(ValueError, "version/member"):
            a.directml_provenance(self.source, self.lock)

    def test_directml_regular_copy_requires_pinned_hash_and_x64(self):
        provenance = a.directml_provenance(self.source, self.lock)
        a.materialize_directml(self.runtime, provenance)
        (self.runtime / "DirectML.dll").write_bytes(pe_fixture(delay=True))
        with self.assertRaisesRegex(ValueError, "pinned version"):
            a.materialize_directml(self.runtime, provenance)
        data = pe_fixture(machine=0x14c)
        (self.runtime / "DirectML.dll").write_bytes(data)
        with self.assertRaisesRegex(ValueError, "x64"):
            a.materialize_directml(self.runtime, {**provenance, "sha256": sha(data)})

    def test_directml_receipt_provenance_cannot_be_relabelled(self):
        path = self.target / "drawing-build.json"
        receipt = a.read_json(path)
        receipt["directml_provenance"]["version"] = "1.99.0"
        a.write_json(path, receipt)
        with self.assertRaisesRegex(ValueError, "provenance mismatch"):
            self.assemble()
        self.assertEqual(list(self.package.iterdir()), [])

    def test_directml_link_materializes_only_exact_trusted_target(self):
        provenance = a.directml_provenance(self.source, self.lock)
        cache = self.root / "local-app-data"
        trusted = cache / "ort.pyke.io/dfbin" / a.TARGET / provenance["ort_archive_sha256"] / "onnxruntime/lib/DirectML.dll"
        trusted.parent.mkdir(parents=True)
        trusted.write_bytes(pe_fixture())
        dll = self.runtime / "DirectML.dll"
        dll.unlink()
        try:
            dll.symlink_to(trusted)
        except OSError:
            self.skipTest("Symlink privilege unavailable; mocked resolver tests still run")
        with patch.dict(os.environ, LOCALAPPDATA=str(cache)):
            a.materialize_directml(self.runtime, provenance)
        self.assertFalse(dll.is_symlink())
        self.assertEqual(dll.read_bytes(), trusted.read_bytes())
        self.assemble()
        self.assertFalse((self.package / "apps/drawing/DirectML.dll").is_symlink())

    def test_materialization_copy_and_swap_without_link_privilege(self):
        provenance = a.directml_provenance(self.source, self.lock)
        cache = self.root / "local-app-data"
        trusted = cache / "ort.pyke.io/dfbin" / a.TARGET / provenance["ort_archive_sha256"] / "onnxruntime/lib/DirectML.dll"
        trusted.parent.mkdir(parents=True)
        trusted.write_bytes(pe_fixture())
        dll = self.runtime / "DirectML.dll"
        dll.write_bytes(b"synthetic link placeholder")
        original_lstat = Path.lstat
        linked = True

        def lstat(path, *args, **kwargs):
            if path == dll and linked:
                return os.stat_result((a.stat.S_IFLNK | 0o777, 0, 0, 1, 0, 0, 0, 0, 0, 0))
            return original_lstat(path, *args, **kwargs)

        original_replace = os.replace

        def replace(source, destination):
            nonlocal linked
            self.assertEqual(destination, dll)
            self.assertEqual(source.read_bytes(), pe_fixture())
            original_replace(source, destination)
            linked = False

        with patch.object(Path, "lstat", lstat), patch.dict(os.environ, LOCALAPPDATA=str(cache)), \
             patch.object(a.os, "readlink", return_value=str(trusted)), patch.object(a.os, "replace", side_effect=replace) as swap:
            with self.assertRaisesRegex(ValueError, "pinned version"):
                a.materialize_directml(self.runtime, {**provenance, "sha256": "0" * 64})
            swap.assert_not_called()
            a.materialize_directml(self.runtime, provenance)
            swap.assert_called_once()
        self.assertEqual(dll.read_bytes(), pe_fixture())
        self.assertEqual(trusted.read_bytes(), pe_fixture())
        self.assertFalse((self.runtime / "DirectML.dll.verified").exists())
        self.assemble()

    def test_directml_resolver_rejects_unsafe_targets_without_link_privilege(self):
        provenance = a.directml_provenance(self.source, self.lock)
        cache = self.root / "local-app-data"
        trusted = cache / "ort.pyke.io/dfbin" / a.TARGET / provenance["ort_archive_sha256"] / "onnxruntime/lib/DirectML.dll"
        trusted.parent.mkdir(parents=True)
        trusted.write_bytes(pe_fixture())
        dll = self.runtime / "DirectML.dll"
        original_lstat = Path.lstat

        def lstat(path, *args, **kwargs):
            if path == dll:
                return os.stat_result((a.stat.S_IFLNK | 0o777, 0, 0, 1, 0, 0, 0, 0, 0, 0))
            return original_lstat(path, *args, **kwargs)

        with patch.object(Path, "lstat", lstat), patch.dict(os.environ, LOCALAPPDATA=str(cache)):
            with patch.object(a.os, "readlink", return_value=str(trusted)):
                self.assertEqual(a.trusted_directml_target(dll, self.runtime, provenance), trusted)
            fallback = self.runtime / "build/ort-sys-a123/out/onnxruntime/lib/DirectML.dll"
            fallback.parent.mkdir(parents=True)
            fallback.write_bytes(pe_fixture())
            with patch.object(a.os, "readlink", return_value=str(fallback)):
                self.assertEqual(a.trusted_directml_target(dll, self.runtime, provenance), fallback)
            for target in (self.root / "arbitrary.dll", trusted.parent / "../DirectML.dll", "//server/share/DirectML.dll",
                           self.runtime / "build/other-a123/out/onnxruntime/lib/DirectML.dll"):
                with self.subTest(target=target), patch.object(a.os, "readlink", return_value=str(target)), self.assertRaises(ValueError):
                    a.trusted_directml_target(dll, self.runtime, provenance)
            # Chained links/reparse targets are rejected before any bytes are read.
            with patch.object(a.os, "readlink", return_value=str(dll)), self.assertRaisesRegex(ValueError, "Symlink"):
                a.trusted_directml_target(dll, self.runtime, provenance)

    def test_main_policy_is_not_read_by_drawing_assembly(self):
        policy = self.root / "tools/distribution-review.json"
        policy.parent.mkdir()
        policy.write_bytes(b"not a review record")
        self.assemble()
        self.assertEqual(policy.read_bytes(), b"not a review record")

    def test_legal_blobs_preserve_crlf_and_ignore_checkout_conversion(self):
        # Preserve a CRLF blob explicitly, while Git expands other LF blobs.
        self.put(".gitattributes", b"* text=auto\nLICENSE -text\n")
        lf_body = b"SYNTHETIC legal text with pinned LF\n"
        old_rust = self.rust_text
        (self.source / old_rust).unlink()
        self.rust_text = "distribution/legal/app/rust/texts/" + sha(lf_body) + ".txt"
        self.put(self.rust_text, lf_body)
        self.commit()
        self.git("config", "core.autocrlf", "true")
        for name in ("Cargo.lock", self.rust_text):
            (self.source / name).unlink()
            self.git("checkout", "--", name)
        self.assertIn(b"\r\n", (self.source / self.rust_text).read_bytes())
        self.assertEqual(self.git("status", "--porcelain=v1", "--untracked-files=all"), "")
        self.bind()
        self.assertNotEqual(a.digest(self.source / "Cargo.lock")["sha256"], self.lock["source_binding"]["cargo_lock_sha256"])
        self.assemble()
        legal_root = self.package / "docs/licenses/NeoRuntime-drawing"
        self.assertEqual((legal_root / self.rust_text).read_bytes(), lf_body)
        self.assertEqual((legal_root / "LICENSE").read_bytes(), self.legal)
        manifest = a.read_json(legal_root / "SOURCE.json")
        for name, value in manifest["legal_files"].items():
            self.assertEqual(a.digest(legal_root / name)["sha256"], value)

    def test_verify_source_cli_succeeds_without_reviewed(self):
        self.assertEqual(a.main(["verify-source", "--source", str(self.source), "--lock", str(self.lock_path)]), 0)

    def test_build_rejects_wrong_directml_before_running_any_executable(self):
        target = self.root / "rejected-build"
        real_run = subprocess.run
        real_check_output = subprocess.check_output
        executed = []

        def run(command, **kwargs):
            if command[0] == "git":
                return real_run(command, **kwargs)
            executed.append(command)
            self.assertEqual(command[0], "cargo", "No EXE may run before DirectML verification")
            runtime = target / a.TARGET / "release"
            runtime.mkdir(parents=True)
            for name in a.ARTIFACTS:
                (runtime / name).write_bytes(pe_fixture(delay=name == "DirectML.dll"))
            return subprocess.CompletedProcess(command, 0)

        def output(command, **kwargs):
            if command[0] in ("rustc", "cargo"):
                return command[0] + " 1.97.1 (synthetic)"
            return real_check_output(command, **kwargs)

        with patch.object(a.subprocess, "run", side_effect=run), patch.object(a.subprocess, "check_output", side_effect=output):
            with self.assertRaisesRegex(ValueError, "pinned version"):
                a.build(self.source, target, self.lock_path)
        self.assertEqual(len(executed), 1)
        self.assertFalse((target / "drawing-build.json").exists())

    def test_build_is_explicit_pinned_and_only_executes_versions(self):
        target = self.root / "fresh-build"
        commands = []

        def run(command, **kwargs):
            commands.append(command)
            if command[0] == "cargo":
                runtime = target / a.TARGET / "release"
                runtime.mkdir(parents=True)
                for name in a.ARTIFACTS:
                    (runtime / name).write_bytes(pe_fixture())
                return subprocess.CompletedProcess(command, 0)
            self.assertEqual(command[1:], ["--version"])
            return subprocess.CompletedProcess(command, 0, b"", b"Synthetic app 0.0.1\n")

        real_check_output = subprocess.check_output

        def output(command, **kwargs):
            if command[0] in ("rustc", "cargo"):
                self.assertEqual(command[1:], ["+1.97.1", "--version"])
                return command[0] + " 1.97.1 (synthetic)"
            return real_check_output(command, **kwargs)

        # Keep real Git checks; only intercept build and executable processes.
        real_run = subprocess.run

        def dispatch(command, **kwargs):
            if command[0] == "git":
                return real_run(command, **kwargs)
            return run(command, **kwargs)

        with patch.object(a.subprocess, "run", side_effect=dispatch), patch.object(a.subprocess, "check_output", side_effect=output):
            receipt = a.build(self.source, target, self.lock_path)
        self.assertEqual(commands[0], ["cargo", "+1.97.1", "build", "--locked", "--release", "-p", "neo-drawing", "-p", "neo-blackboard",
                                       "--target", a.TARGET, "--target-dir", str(target)])
        self.assertEqual(len(commands), 3)
        self.assertEqual(set(receipt["version_output"]), set(a.ARTIFACTS[:2]))
        self.assertTrue((target / "drawing-build.json").is_file())
        self.assertEqual(receipt["directml_provenance"], a.directml_provenance(self.source, self.lock))
        with self.assertRaisesRegex(ValueError, "must be new"):
            a.build(self.source, target, self.lock_path)


if __name__ == "__main__":
    unittest.main()
