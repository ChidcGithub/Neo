"""Bounded local fixtures only; no network, real native inputs or approvals."""
import copy
import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from tools import verify_ort_sources as ort


def digest(data):
    return hashlib.sha256(data).hexdigest()


def put(root, name, content):
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    return path


def put_json(root, name, value):
    return put(root, name, (json.dumps(value, indent=2) + "\n").encode())


def fixture(root, package):
    """Return a complete schema-1 example; callers substitute only the Eigen pin."""
    eigen = b"synthetic original Eigen archive"
    put(root, ort.NOTICE, b"Public ORT Eigen source notice; no final binary attestation.\n")
    index = {"schema": 1, "texts": {}}
    patches = []
    payload = {"sources/" + ort.EIGEN: eigen}
    for name in ort.PATCHES:
        content = ("synthetic patch " + name).encode()
        path = ort.PUBLIC_NATIVE + "/" + name
        put(root, path, content)
        index["texts"][name] = {"sha256": digest(content), "size": len(content)}
        patches.append({"path": path, "sha256": digest(content), "member": "notices/" + name})
        payload["notices/" + name] = content
    for name in (*ort.EIGEN_TEXTS, *ort.STATIC_ORT_TEXTS):
        content = ("synthetic indexed license " + name).encode()
        put(root, ort.PUBLIC_NATIVE + "/" + name, content)
        index["texts"][name] = {"sha256": digest(content), "size": len(content)}
    for name in ort.MAIN_ORT_TEXTS:
        put(root, name, ("synthetic main runtime notice " + name).encode())
    put_json(root, ort.PUBLIC_NATIVE + "/index.json", index)
    legal_files = {}
    for name in ort.DRAWING_NOTICES:
        content = ("synthetic drawing notice " + name).encode()
        put(package, ort.DRAWING_LEGAL_ROOT + name, content)
        legal_files[name] = digest(content)
    drawing = {"schema": 1, "commit": ort.DRAWING_COMMIT,
               "repository": "https://github.com/ChidcGithub/NeoRuntime-drawing",
               "legal_files": list(legal_files), "source_binding": {"legal_files": legal_files}}
    drawing_path = put_json(root, ort.DRAWING_LOCK, drawing)
    components = []
    for id, name in ort.WAKE.items():
        content = ("synthetic DLL " + name).encode()
        put(root, "crates/neo-wake/assets/" + name, content)
        put(package, "runtime/onnx/" + name, content)
        components.append({"id": id, "artifact_sha256": digest(content)})
    components.extend([
        {"id": "sherpa-static-onnxruntime", "artifact_sha256": ort.build.DEPS["onnxruntime"][1]},
        {"id": "drawing-static-onnxruntime", "artifact_sha256": ort.DRAWING_ORT_SHA256,
         "drawing_commit": ort.DRAWING_COMMIT}])
    for component in components:
        component.update(eigen_archive=ort.EIGEN, eigen_sha256=digest(eigen),
                         patches=patches if component["id"] == "sherpa-static-onnxruntime" else [])
    bundle = root / "native.zip"
    with zipfile.ZipFile(bundle, "w") as archive:
        for name, content in payload.items():
            archive.writestr(name, content)
    source = {"kind": "native", "download_url": "https://example.test/native-v1.zip", **ort.file_record(bundle)}
    record = {"schema": 1, "final_binary_identity_verified": False, "components": components,
              "source_delivery": {"kind": "native", "url": source["download_url"],
                                  **ort.file_record(bundle), "eigen_member": "sources/" + ort.EIGEN}}
    put_json(root, ort.RECORD, record)
    shutil.copytree(root / "docs", package / "docs", dirs_exist_ok=True)
    put_json(package, ort.DRAWING_SOURCE, {"source_commit": drawing["commit"], "repository": drawing["repository"],
             "lock_sha256": ort.file_record(drawing_path)["sha256"], "legal_files": legal_files,
             "directml_provenance": {"ort_archive_sha256": ort.DRAWING_ORT_SHA256.upper()}})
    lock = {"schema": 1, "sources": [source], "ort_eigen_notice": {
        "path": ort.NOTICE, "sha256": ort.file_record(root / ort.NOTICE)["sha256"],
        "record_path": ort.RECORD, "record_sha256": ort.file_record(root / ort.RECORD)["sha256"]}}
    return lock, record, bundle, payload


class VerifyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="ort-source-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / "repo"
        self.package = Path(temporary.name) / "package"
        self.lock, self.record, self.bundle, self.payload = fixture(self.root, self.package)
        self.tracked = patch.object(ort, "tracked").start()
        self.addCleanup(patch.stopall)
        patch.dict(ort.native.SOURCES, {ort.EIGEN: {**ort.native.SOURCES[ort.EIGEN],
                   "sha256": digest(self.payload["sources/" + ort.EIGEN])}}).start()

    def verify(self, package=True, bundle=True):
        return ort.verify(self.lock, root=self.root, package=self.package if package else None,
                          bundle=self.bundle if bundle else None)

    def save_record(self):
        put_json(self.root, ort.RECORD, self.record)
        shutil.copyfile(self.root / ort.RECORD, self.package / ort.RECORD)
        self.lock["ort_eigen_notice"]["record_sha256"] = ort.file_record(self.root / ort.RECORD)["sha256"]

    def rewrite_bundle(self, payload):
        with zipfile.ZipFile(self.bundle, "w") as archive:
            for name, content in payload.items():
                if isinstance(name, str):
                    info = zipfile.ZipInfo()
                    # Preserve malicious raw names even on Windows, where the
                    # ZipInfo constructor normally canonicalizes separators.
                    info.filename = name
                else:
                    info = name
                archive.writestr(info, content)
        pin = ort.file_record(self.bundle)
        self.lock["sources"][0].update(pin)
        self.record["source_delivery"].update(pin)
        self.save_record()

    def test_valid_default_and_full_checks_report_limits(self):
        self.assertFalse(self.verify()["final_binary_identity_verified"])
        self.assertTrue(self.verify()["native_zip_checked"])
        result = self.verify(package=False, bundle=False)
        self.assertFalse(result["package_checked"])
        self.assertFalse(result["native_zip_checked"])
        self.tracked.assert_called_with(self.root, [ort.NOTICE, ort.RECORD, ort.PUBLIC_NATIVE + "/index.json",
            ort.DRAWING_LOCK, *(ort.PUBLIC_NATIVE + "/" + name for name in (*ort.PATCHES, *ort.EIGEN_TEXTS, *ort.STATIC_ORT_TEXTS)),
            *ort.MAIN_ORT_TEXTS])

    def test_optional_absent_but_partial_entry_always_fails(self):
        self.assertIsNone(ort.verify({}, required=False))
        with self.assertRaisesRegex(ValueError, "Missing ort_eigen_notice"):
            ort.verify({})
        entry = self.lock["ort_eigen_notice"]
        for key in entry:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "Incomplete"):
                ort.check_lock_entry({"ort_eigen_notice": {k: v for k, v in entry.items() if k != key}})
        for value in (None, {}, False, []):
            with self.subTest(value=value), self.assertRaises(ValueError):
                ort.check_lock_entry({"ort_eigen_notice": value})

    def test_missing_and_stale_public_and_package_notices(self):
        for root in (self.root, self.package):
            for name in (ort.NOTICE, ort.RECORD):
                path = root / name
                original = path.read_bytes()
                with self.subTest(root=root, name=name):
                    path.unlink()
                    with self.assertRaisesRegex(ValueError, "Missing regular"):
                        self.verify()
                    path.write_bytes(original + b"stale")
                    with self.assertRaisesRegex(ValueError, "hash mismatch|differs from tracked"):
                        self.verify()
                    path.write_bytes(original)

    def test_drawing_source_binding_needs_no_review_or_duplicate_eigen_texts(self):
        drawing = ort.read_json(self.root / ort.DRAWING_LOCK)
        self.assertNotIn("review", drawing)
        expected = {"distribution/legal/app/native/" + name for name in (
            "onnxruntime/LICENSE", "onnxruntime/ThirdPartyNotices.txt",
            "ort-sys/LICENSE-APACHE", "ort-sys/LICENSE-MIT",
            "directml/LICENSE-CODE.txt", "directml/LICENSE.txt", "directml/ThirdPartyNotices.txt")}
        self.assertEqual(set(ort.DRAWING_NOTICES), expected)
        self.assertEqual(set(drawing["source_binding"]["legal_files"]), expected)
        for suffix in ("APACHE", "BSD", "MINPACK", "MPL2", "README"):
            name = "distribution/legal/app/native/ort-eigen/eigen-COPYING." + suffix
            self.assertNotIn(name, drawing["legal_files"])
            self.assertFalse((self.package / (ort.DRAWING_LEGAL_ROOT + name)).exists())
        self.assertTrue(self.verify()["package_checked"])

    def test_package_requires_full_mpl_not_just_source_notice(self):
        path = self.package / ort.PUBLIC_NATIVE / "eigen-1d8b82b-COPYING.MPL2"
        path.unlink()
        with self.assertRaisesRegex(ValueError, "Missing regular file: .*COPYING.MPL2"):
            self.verify()
        path.write_text("See the source ZIP for MPL", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Package license text mismatch: .*COPYING.MPL2"):
            self.verify()

    def test_all_indexed_license_texts_require_original_root_and_package_bytes(self):
        for root in (self.root, self.package):
            for name in (*ort.EIGEN_TEXTS, *ort.STATIC_ORT_TEXTS):
                path = root / ort.PUBLIC_NATIVE / name
                original = path.read_bytes()
                with self.subTest(root=root, name=name):
                    path.unlink()
                    with self.assertRaisesRegex(ValueError, "Missing regular file"):
                        self.verify()
                    path.write_bytes(b"truncated license")
                    with self.assertRaisesRegex(ValueError, "license text mismatch"):
                        self.verify()
                    path.write_bytes(original)
        index = ort.read_json(self.root / ort.PUBLIC_NATIVE / "index.json")
        del index["texts"]["eigen-1d8b82b-COPYING.MPL2"]
        put_json(self.root, ort.PUBLIC_NATIVE + "/index.json", index)
        with self.assertRaisesRegex(ValueError, "Missing indexed license text"):
            self.verify()

    def test_main_runtime_notices_must_be_retained(self):
        for name in ort.MAIN_ORT_TEXTS:
            path = self.package / name
            original = path.read_bytes()
            with self.subTest(name=name):
                path.unlink()
                with self.assertRaisesRegex(ValueError, "Missing regular file"):
                    self.verify()
                path.write_bytes(b"changed runtime notice")
                with self.assertRaisesRegex(ValueError, "main ORT notice mismatch"):
                    self.verify()
                path.write_bytes(original)

    def test_drawing_native_notices_must_match_source_binding(self):
        for name in ort.DRAWING_NOTICES:
            path = self.package / (ort.DRAWING_LEGAL_ROOT + name)
            original = path.read_bytes()
            with self.subTest(name=name):
                path.unlink()
                with self.assertRaisesRegex(ValueError, "Missing regular file"):
                    self.verify()
                path.write_bytes(b"changed drawing notice")
                with self.assertRaisesRegex(ValueError, "drawing native notice mismatch"):
                    self.verify()
                path.write_bytes(original)
        name = ort.DRAWING_NOTICES[0]
        put(self.package, ort.DRAWING_LEGAL_ROOT + name, b"self-approved")
        provenance = ort.read_json(self.package / ort.DRAWING_SOURCE)
        provenance["legal_files"][name] = digest(b"self-approved")
        put_json(self.package, ort.DRAWING_SOURCE, provenance)
        with self.assertRaisesRegex(ValueError, "legal manifest differs from source binding"):
            self.verify()

    def test_drawing_missing_source_binding_cannot_fall_back_to_review(self):
        original = ort.read_json(self.root / ort.DRAWING_LOCK)
        for legacy_review in (False, True):
            drawing = copy.deepcopy(original)
            binding = drawing.pop("source_binding")
            if legacy_review:
                drawing["review"] = binding
            path = put_json(self.root, ort.DRAWING_LOCK, drawing)
            provenance = ort.read_json(self.package / ort.DRAWING_SOURCE)
            provenance["lock_sha256"] = ort.file_record(path)["sha256"]
            put_json(self.package, ort.DRAWING_SOURCE, provenance)
            with self.subTest(legacy_review=legacy_review), self.assertRaisesRegex(ValueError, "source binding"):
                self.verify()

    def test_drawing_missing_required_allowlist_entry_fails(self):
        original = ort.read_json(self.root / ort.DRAWING_LOCK)
        for name in ort.DRAWING_NOTICES:
            drawing = copy.deepcopy(original)
            drawing["legal_files"].remove(name)
            path = put_json(self.root, ort.DRAWING_LOCK, drawing)
            provenance = ort.read_json(self.package / ort.DRAWING_SOURCE)
            provenance["lock_sha256"] = ort.file_record(path)["sha256"]
            put_json(self.package, ort.DRAWING_SOURCE, provenance)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "omits required"):
                self.verify()

    def test_drawing_source_binding_manifest_must_match_in_full(self):
        original = ort.read_json(self.package / ort.DRAWING_SOURCE)
        name = ort.DRAWING_NOTICES[0]
        for legal in (None, {}, {key: value for key, value in original["legal_files"].items() if key != name},
                      {**original["legal_files"], "extra.txt": "0" * 64}):
            provenance = {**original, "legal_files": legal}
            put_json(self.package, ort.DRAWING_SOURCE, provenance)
            with self.subTest(legal=legal), self.assertRaisesRegex(ValueError, "source binding"):
                self.verify()

    def test_lock_path_bypasses_and_bad_hashes(self):
        for value in ("../" + ort.NOTICE, ort.NOTICE.upper(), "other/ORT-EIGEN-SOURCE.txt", "C:/notice", ort.NOTICE + ":stream"):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, "exact public root"):
                ort.check_lock_entry({"ort_eigen_notice": {**self.lock["ort_eigen_notice"], "path": value}})
        for key in ("sha256", "record_sha256"):
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "SHA-256"):
                ort.check_lock_entry({"ort_eigen_notice": {**self.lock["ort_eigen_notice"], key: "invalid"}})

    def test_current_dlls_and_package_dlls_must_match(self):
        for root, prefix in ((self.root, "crates/neo-wake/assets/"), (self.package, "runtime/onnx/")):
            for name in ort.WAKE.values():
                path = root / (prefix + name)
                original = path.read_bytes()
                with self.subTest(path=path), self.assertRaisesRegex(ValueError, "wake DLL hash"):
                    path.write_bytes(b"changed")
                    self.verify()
                path.write_bytes(original)

    def test_schema_components_and_final_identity_claim(self):
        original = copy.deepcopy(self.record)
        for key, value in (("schema", 2), ("schema", True), ("components", []),
                           ("components", original["components"] * 2), ("final_binary_identity_verified", True)):
            self.record = {**original, key: value}
            self.save_record()
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                self.verify()

    def test_wrong_component_hash_eigen_and_patches(self):
        original = copy.deepcopy(self.record)
        for index in range(4):
            for key, value in (("artifact_sha256", "0" * 64), ("eigen_sha256", "0" * 64),
                               ("eigen_archive", "../" + ort.EIGEN), ("patches", [{}])):
                self.record = copy.deepcopy(original)
                self.record["components"][index][key] = value
                self.save_record()
                with self.subTest(index=index, key=key), self.assertRaises(ValueError):
                    self.verify()
        self.record = copy.deepcopy(original)
        self.record["components"][2]["patches"] = []
        self.save_record()
        with self.assertRaisesRegex(ValueError, "patches"):
            self.verify()

    def test_patch_hash_and_member_path_not_self_authorized(self):
        for key, value in (("sha256", "0" * 64), ("member", "notices/../patch"), ("path", "other.patch")):
            original = copy.deepcopy(self.record)
            self.record["components"][2]["patches"][0][key] = value
            self.save_record()
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "patches"):
                self.verify()
            self.record = original
        self.save_record()
        for root in (self.root, self.package):
            path = root / ort.PUBLIC_NATIVE / ort.PATCHES[0]
            original = path.read_bytes()
            path.write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "patch hash"):
                self.verify()
            path.write_bytes(original)

    def test_source_delivery_pins_and_url_must_equal_lock(self):
        original = copy.deepcopy(self.record)
        for key, value in (("sha256", "0" * 64), ("size", 1), ("url", "https://example.test/other.zip"),
                           ("url", "http://example.test/native.zip"), ("eigen_member", "sources/../" + ort.EIGEN),
                           ("kind", "gitbash"), ("size", True)):
            self.record = copy.deepcopy(original)
            self.record["source_delivery"][key] = value
            self.save_record()
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.verify()

    def test_build_deps_change_is_detected(self):
        url, _, archive = ort.build.DEPS["onnxruntime"]
        with patch.dict(ort.build.DEPS, {"onnxruntime": (url, "0" * 64, archive)}), \
                self.assertRaisesRegex(ValueError, "build.DEPS"):
            self.verify()

    def test_drawing_pin_and_package_provenance_change(self):
        path = self.root / ort.DRAWING_LOCK
        original = path.read_bytes()
        value = json.loads(original)
        value["commit"] = "0" * 40
        put_json(self.root, ort.DRAWING_LOCK, value)
        with self.assertRaisesRegex(ValueError, "Drawing pin changed"):
            self.verify()
        path.write_bytes(original)
        provenance = json.loads((self.package / ort.DRAWING_SOURCE).read_bytes())
        for key in ("source_commit", "lock_sha256", "repository", "directml_provenance"):
            value = {**provenance, key: {} if key == "directml_provenance" else "changed"}
            put_json(self.package, ort.DRAWING_SOURCE, value)
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "drawing"):
                self.verify()

    def test_changed_zip_and_missing_or_changed_members(self):
        with self.bundle.open("ab") as stream:
            stream.write(b"changed")
        with self.assertRaisesRegex(ValueError, "ZIP size/hash"):
            self.verify()
        for name in self.payload:
            for missing in (True, False):
                payload = dict(self.payload)
                if missing:
                    del payload[name]
                else:
                    payload[name] = b"changed member"
                self.rewrite_bundle(payload)
                with self.subTest(name=name, missing=missing), self.assertRaisesRegex(ValueError, "Missing source member|member hash"):
                    self.verify()

    def test_unsafe_duplicate_special_and_new_notice_members(self):
        for name in ("../escape", "/absolute", "C:/escape", "sources\\escape", "foo:stream", "CON", "a/./b",
                     "a//b", "notices/ORT-EIGEN-SOURCE.txt", "notices/ort-eigen-correspondence.json",
                     ("sources/" + ort.EIGEN).upper()):
            self.rewrite_bundle({**self.payload, name: b"bad"})
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.verify()
        for mode in (stat.S_IFLNK, stat.S_IFDIR):
            info = zipfile.ZipInfo("special")
            info.create_system = 3
            info.external_attr = (mode | 0o777) << 16
            self.rewrite_bundle({**self.payload, info: b"bad"})
            with self.assertRaisesRegex(ValueError, "Non-regular"):
                self.verify()

    def test_zip_expansion_budget(self):
        with patch.object(ort.native, "MAX_FILES", 1), self.assertRaisesRegex(ValueError, "budget"):
            self.verify()

    def test_path_links_and_parent_reparse_are_rejected(self):
        path = self.root / ort.NOTICE
        original = Path.lstat

        def lstat(p, *args, **kwargs):
            info = original(p, *args, **kwargs)
            if p == path.parent:
                from types import SimpleNamespace
                return SimpleNamespace(st_mode=info.st_mode, st_file_attributes=0x400)
            return info

        with patch.object(Path, "lstat", lstat), self.assertRaisesRegex(ValueError, "reparse"):
            self.verify()
        for name in ("../file", "C:/file", "foo:bar", "a\\b"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                ort.relative_file(self.root, name)

    def test_json_duplicate_keys_rejected(self):
        path = put(self.root, "duplicate.json", b'{"schema": 1, "schema": 2}')
        with self.assertRaisesRegex(ValueError, "Duplicate JSON"):
            ort.read_json(path)


class TrackedAndCliTests(unittest.TestCase):
    def test_exact_git_index_paths_required(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "--quiet", str(root)], check=True, capture_output=True, timeout=15)
            put(root, ort.NOTICE, b"tracked notice")
            with self.assertRaisesRegex(ValueError, "must be tracked"):
                ort.tracked(root, [ort.NOTICE])
            subprocess.run(["git", "add", "--", ort.NOTICE], cwd=root, check=True, capture_output=True, timeout=15)
            ort.tracked(root, [ort.NOTICE])
            with self.assertRaisesRegex(ValueError, "must be tracked"):
                ort.tracked(root, [ort.NOTICE.upper()])

    def test_cli_blocks_missing_opt_in_and_has_no_network(self):
        with tempfile.TemporaryDirectory() as temporary:
            lock = put_json(Path(temporary), "lock.json", {"schema": 1, "sources": []})
            result = subprocess.run([sys.executable, "-B", str(ort.ROOT / "tools/verify_ort_sources.py"), "--lock", str(lock)],
                                    capture_output=True, text=True, timeout=15)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Missing ort_eigen_notice", result.stderr)


if __name__ == "__main__":
    unittest.main()
