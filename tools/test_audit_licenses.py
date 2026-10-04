"""Synthetic offline audit tests: no Cargo commands, network, or build scripts."""
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from tools.audit_licenses import (
    Source, build_candidates, command, digest, inspect_package, inventory,
    license_candidate, locate, main, review_flags, supplemental, tree_members, write_notices,
)


REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


class AuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "cargo-home"
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = []\n[workspace.package]\nlicense = "Apache-2.0"\nversion = "1.0.0"\n', encoding="utf-8")

    def package(self, name="sample", version="1.0.0", source=REGISTRY):
        return {"name": name, "version": version, "source": source, "checksum": "0" * 64}

    def manifest(self, package, license='license = "MIT"', files=None):
        directory = self.home / "registry/src/test" / (package["name"] + "-" + package["version"])
        directory.mkdir(parents=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{package["name"]}"\nversion = "{package["version"]}"\n{license}\n', encoding="utf-8")
        for name, content in (files or {}).items():
            path = directory / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
        return {**package, "id": package["name"] + "@" + package["version"],
                "manifest_path": str(directory / "Cargo.toml"), "targets": [{"kind": ["lib"]}]}

    def lock(self, packages):
        text = "version = 4\n"
        for p in packages:
            text += "\n[[package]]\n" + "\n".join(
                f'{k} = "{v}"' for k, v in p.items() if v is not None) + "\n"
        (self.root / "Cargo.lock").write_text(text, encoding="utf-8")

    def test_missing_metadata_and_cache_are_unknown_not_false_or_mit(self):
        self.lock([self.package()])
        report, texts = inventory(self.root, self.home)
        p = report["packages"][0]
        self.assertTrue(p["missing_cache"])
        self.assertFalse(p["metadata_available"])
        self.assertIsNone(p["license"])
        self.assertIsNone(p["scopes"]["windows_normal_build"])
        self.assertIsNone(report["counts"]["windows_runtime_candidate"])
        self.assertEqual(texts, [])

    def test_multiple_versions_remain_distinct(self):
        packages = [self.package(version="1.0.0"), self.package(version="2.0.0")]
        metas = [self.manifest(p, files={"LICENSE": b"license\r\n"}) for p in packages]
        self.lock(packages)
        report, texts = inventory(self.root, self.home, {"packages": metas, "workspace_members": []}, trees={
            "windows-normal-build": "0sample v2.0.0\n",
            "windows-runtime": "0sample v2.0.0\n",
            "windows-with-dev": "0sample v2.0.0\n",
        })
        self.assertEqual(len(texts), 2)
        self.assertFalse(report["packages"][0]["scopes"]["windows_normal_build"])
        self.assertTrue(report["packages"][1]["scopes"]["windows_normal_build"])
        self.assertEqual(report["counts"]["outside_windows_default_tree"], 1)

    def test_same_name_version_different_sources_is_not_guessed(self):
        packages = [self.package(), self.package(source="git+https://example.invalid/repo#abc")]
        members, issues = tree_members("0sample v1.0.0\n", packages)
        self.assertFalse(members)
        self.assertTrue(issues)

    def test_license_file_and_recursive_notices_hash_original_bytes(self):
        p = self.package()
        raw = b"Custom license\r\nCopyright \xc2\xa9\r\n"
        m = self.manifest(p, 'license-file = "legal/terms.txt"', {
            "legal/terms.txt": raw, "native/NOTICE": b"nested", "licenses/third-party.txt": b"third",
            "README.md": b"not a notice",
        })
        entry, texts = inspect_package(p, m, self.root, self.home, set())
        self.assertIn("license-file-only", entry["review_flags"])
        self.assertEqual(entry["license_text_count"], 3)
        evidence = next(e for e in entry["evidence"] if e["path"] == "legal/terms.txt")
        self.assertEqual(evidence["sha256"], digest(raw))
        entry["scopes"] = {"windows_normal_build": None}
        output = self.root / "notices.txt"
        write_notices(output, [(entry, ev, data) for ev, data in texts])
        self.assertIn(raw, output.read_bytes())

    def test_nonstandard_font_license_names_and_text(self):
        p = self.package()
        m = self.manifest(p, files={
            "fonts/emoji-icon-font-mit-license.txt": b"MIT text",
            "fonts/Hack-Regular.txt": b"BITSTREAM VERA LICENSE\nPermission is hereby granted",
            "README.md": b"regular readme",
        })
        entry, texts = inspect_package(p, m, self.root, self.home, set())
        self.assertEqual(entry["license_text_count"], 2)
        self.assertIn("Bitstream-Vera-in-text-review", entry["review_flags"])
        self.assertEqual(len(texts), 2)

    def test_missing_declared_file_is_reported(self):
        p = self.package()
        m = self.manifest(p, 'license-file = "absent.txt"')
        entry, texts = inspect_package(p, m, self.root, self.home, set())
        self.assertFalse(entry["missing_cache"])
        self.assertTrue(any("absent.txt" in issue for issue in entry["issues"]))
        self.assertEqual(texts, [])

    def test_archive_fallback_requires_lock_checksum(self):
        p = self.package()
        archive = self.home / "registry/cache/mirror/sample-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        with tarfile.open(archive, "w:gz") as tar:
            for name, raw in {
                "Cargo.toml": b'[package]\nname="sample"\nversion="1.0.0"\nlicense="MPL-2.0"\n',
                "LICENSE": b"MPL text",
            }.items():
                info = tarfile.TarInfo("sample-1.0.0/" + name)
                info.size = len(raw)
                tar.addfile(info, io.BytesIO(raw))
        source, issues = locate(p, None, self.root, self.home)
        self.assertIsNone(source)
        self.assertIn("checksum mismatch", issues[0])
        p["checksum"] = digest(archive.read_bytes())
        self.lock([p])
        report, texts = inventory(self.root, self.home)
        entry = report["packages"][0]
        self.assertFalse(entry["missing_cache"])
        self.assertFalse(entry["metadata_available"])
        self.assertEqual(entry["license"], "MPL-2.0")
        self.assertEqual(entry["archive_sha256"], p["checksum"])
        self.assertEqual(len(texts), 1)

    def test_unattributed_unpacked_registry_directory_not_trusted(self):
        p = self.package()
        m = self.manifest(p, files={"LICENSE": b"text"})
        source, _ = locate(p, None, self.root, self.home)
        self.assertIsNone(source)
        (Path(m["manifest_path"]).parent / ".cargo-checksum.json").write_text(
            json.dumps({"package": p["checksum"], "files": {}}), encoding="utf-8")
        source, _ = locate(p, None, self.root, self.home)
        self.assertIsNotNone(source)

    def test_build_dev_and_runtime_scopes(self):
        packages = [self.package(name=name) for name in ("app", "runtime", "builder", "macro", "shared", "test", "other")]
        metas = [self.manifest(p, files={"LICENSE": b"text"}) for p in packages]
        by_name = {p["name"]: p for p in metas}
        by_name["macro"]["targets"] = [{"kind": ["proc-macro"]}]
        def dep(name, kind=None):
            return {"pkg": by_name[name]["id"], "dep_kinds": [{"kind": kind, "target": None}]}
        metadata = {"packages": metas, "workspace_members": [by_name["app"]["id"]], "resolve": {"nodes": [
            {"id": by_name["app"]["id"], "deps": [dep("runtime"), dep("builder", "build"), dep("macro"), dep("test", "dev")]},
            {"id": by_name["builder"]["id"], "deps": [dep("shared")]},
            {"id": by_name["runtime"]["id"], "deps": [dep("shared")]},
        ]}}
        self.lock(packages)
        def tree(names):
            return "\n".join("0" + name + " v1.0.0" for name in names.split())
        report, _ = inventory(self.root, self.home, metadata, metadata, {
            "windows-normal-build": tree("app runtime builder macro shared"),
            "windows-runtime": tree("app runtime shared"),
            "windows-with-dev": tree("app runtime builder macro shared test"),
        })
        entries = {p["name"]: p["scopes"] for p in report["packages"]}
        self.assertTrue(entries["builder"]["windows_build_only"])
        self.assertTrue(entries["macro"]["windows_build_only"])
        self.assertTrue(entries["shared"]["windows_build_candidate"])
        self.assertTrue(entries["shared"]["windows_runtime_candidate"])
        self.assertFalse(entries["shared"]["windows_build_only"])
        self.assertTrue(entries["test"]["windows_dev_only"])
        self.assertTrue(entries["other"]["outside_windows_default_tree"])
        self.assertFalse(entries["other"]["windows_normal_build"])
        self.assertIsNone(build_candidates(None, set()))

    def test_path_traversal_is_not_collected(self):
        directory = self.root / "crate"
        directory.mkdir()
        (self.root / "secret").write_bytes(b"secret")
        self.assertIsNone(Source("test", directory=directory).read("../secret"))

    def test_expressions_preserved_and_no_legal_verdict(self):
        p = self.package()
        expression = "MIT OR Apache-2.0 OR LGPL-2.1-or-later"
        m = self.manifest(p, 'license = "' + expression + '"', {"LICENSE": b"text"})
        entry, _ = inspect_package(p, m, self.root, self.home, set())
        self.assertEqual(entry["license"], expression)
        self.assertIn("LGPL-alternative-review", entry["review_flags"])
        self.assertNotIn("GPL-alternative-review", entry["review_flags"])
        self.assertIn("license-unspecified-not-proof-of-unlicensed", review_flags(None, None))
        self.assertNotIn("license-unspecified-not-proof-of-unlicensed", review_flags("Unlicense", None))
        self.assertTrue(license_candidate("fonts/OFL.txt"))
        self.assertTrue(license_candidate("native/licenses/a.txt"))
        self.assertFalse(license_candidate("src/license.rs.bak/code.rs"))

    def test_main_splits_private_inventory_and_public_notices_reproducibly(self):
        p = self.package()
        raw = b"Synthetic license\r\nCopyright fixture\r\n"
        m = self.manifest(p, files={"LICENSE": raw})
        (Path(m["manifest_path"]).parent / ".cargo-checksum.json").write_text(
            json.dumps({"package": p["checksum"], "files": {}}), encoding="utf-8")
        self.lock([p])
        original_lock = (self.root / "Cargo.lock").read_bytes()
        commands = {name: {"returncode": 1, "stdout": f"target/license-audit/{name}.stdout",
                           "stderr": f"target/license-audit/{name}.stderr"}
                    for name in ("all-targets-metadata", "windows-metadata", "windows-normal-build",
                                 "windows-runtime", "windows-with-dev")}
        snapshots = []
        for _ in range(2):
            with patch("tools.audit_licenses.capture", return_value=commands), \
                    patch("sys.stdout", new_callable=io.StringIO):
                self.assertEqual(main(["--root", str(self.root), "--cargo-home", str(self.home)]), 0)
            snapshots.append({path.relative_to(self.root).as_posix(): path.read_bytes()
                              for directory in ("docs", "docs-pri")
                              for path in (self.root / directory).rglob("*") if path.is_file()})
        self.assertEqual(snapshots[0], snapshots[1])
        self.assertEqual(set(snapshots[0]), {"docs-pri/licenses/cargo-inventory.json",
                                           "docs-pri/licenses/cargo-inventory.md",
                                           "docs/licenses/cargo-notices.txt"})
        notices = snapshots[0]["docs/licenses/cargo-notices.txt"]
        self.assertIn(raw, notices)
        report = json.loads(snapshots[0]["docs-pri/licenses/cargo-inventory.json"])
        self.assertEqual(report["notices_sha256"], digest(notices))
        self.assertEqual((self.root / "Cargo.lock").read_bytes(), original_lock)

    def supplement(self, status="complete-text"):
        p = self.package()
        rev = "a" * 40
        vcs = json.dumps({"git": {"sha1": rev}, "path_in_vcs": "crate"}).encode()
        header = b"// Copyright fixture\r\n// Licensed under MIT\r\n"
        source = header + b"fn main() {}\r\n"
        archive = self.home / "registry/cache/test/sample-1.0.0.crate"
        archive.parent.mkdir(parents=True)
        with tarfile.open(archive, "w:gz") as tar:
            for name, raw in {
                "Cargo.toml": b'[package]\nname="sample"\nversion="1.0.0"\nlicense="MIT"\n',
                ".cargo_vcs_info.json": vcs, "src/lib.rs": source,
            }.items():
                info = tarfile.TarInfo("sample-1.0.0/" + name)
                info.size = len(raw)
                tar.addfile(info, io.BytesIO(raw))
        p["checksum"] = digest(archive.read_bytes())
        self.lock([p])
        directory = self.root / "docs/licenses/supplemental"
        directory.mkdir(parents=True)
        raw = b"Synthetic full license\r\n" if status == "complete-text" else header
        (directory / "LICENSE.txt").write_bytes(raw)
        file = {"path": "LICENSE.txt", "sha256": digest(raw), "exact_rev": rev,
                "kind": "upstream-license", "upstream": "https://example.invalid/repo/raw/" + rev + "/LICENSE"}
        if status == "pending":
            file.update(kind="source-header", source_path="src/lib.rs", source_sha256=digest(source),
                        line_start=1, line_end=2)
        index = {"schema_version": 1, "packages": [{**p, "verified": True, "status": status,
                 "exact_rev": rev, "vcs_sha256": digest(vcs), "files": [file]}]}
        path = directory / "index.json"
        path.write_text(json.dumps(index), encoding="utf-8")
        return p, path, index, raw

    def test_supplement_merge_preserves_bytes_and_is_reproducible(self):
        p, path, _, raw = self.supplement()
        snapshots = []
        for _ in range(2):
            report, texts = inventory(self.root, self.home)
            self.assertEqual(report["supplemental_index_sha256"], digest(path.read_bytes()))
            self.assertEqual(report["counts"]["no_license_text"], 0)
            self.assertEqual(report["counts"]["unresolved_text_packages"], 0)
            self.assertEqual(report["packages"][0]["issues"], [])
            output = self.root / "notices.txt"
            write_notices(output, texts)
            self.assertIn(raw, output.read_bytes())
            self.assertIn(b"Origin: https://example.invalid", output.read_bytes())
            snapshots.append((report, output.read_bytes()))
        self.assertEqual(snapshots[0], snapshots[1])

    def test_supplement_wrong_identity_checksum_revision_and_schema_rejected(self):
        p, path, index, _ = self.supplement()
        original = json.dumps(index)
        for field, value in (("name", "other"), ("version", "2.0.0"), ("source", "registry+other"),
                             ("checksum", "f" * 64), ("exact_rev", "b" * 40),
                             ("exact_rev", "main"), ("vcs_sha256", "0" * 64),
                             ("verified", False), ("status", "approved")):
            with self.subTest(field=field, value=value):
                bad = json.loads(original)
                bad["packages"][0][field] = value
                path.write_text(json.dumps(bad), encoding="utf-8")
                with self.assertRaises(ValueError):
                    inventory(self.root, self.home)
        for bad in ({**index, "schema_version": 99}, {**index, "packages": index["packages"] * 2}):
            path.write_text(json.dumps(bad), encoding="utf-8")
            with self.assertRaises(ValueError):
                inventory(self.root, self.home)

    def test_supplement_hash_missing_file_and_unsafe_path_rejected(self):
        _, path, index, raw = self.supplement()
        text = path.parent / "LICENSE.txt"
        text.write_bytes(raw + b"tampered")
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            inventory(self.root, self.home)
        text.unlink()
        with self.assertRaisesRegex(ValueError, "missing file"):
            inventory(self.root, self.home)
        for name in ("../LICENSE.txt", "C:/outside.txt", "nested\\LICENSE.txt"):
            index["packages"][0]["files"][0]["path"] = name
            path.write_text(json.dumps(index), encoding="utf-8")
            with self.assertRaises(ValueError):
                inventory(self.root, self.home)

    def test_supplement_archive_missing_or_tampered_rejected(self):
        self.supplement()
        archive = next(self.home.glob("registry/cache/*/*.crate"))
        archive.write_bytes(b"wrong archive")
        with self.assertRaisesRegex(ValueError, "checksum-verified archive"):
            inventory(self.root, self.home)
        archive.unlink()
        with self.assertRaisesRegex(ValueError, "checksum-verified archive"):
            inventory(self.root, self.home)

    def test_partial_header_remains_pending_and_checks_original_ranges(self):
        _, path, index, raw = self.supplement("pending")
        report, texts = inventory(self.root, self.home, trees={"windows-normal-build": "0sample v1.0.0\n"})
        self.assertEqual(texts[0][2], raw)
        self.assertEqual(report["counts"]["no_license_text"], 0)
        self.assertEqual(report["counts"]["unresolved_text_packages"], 1)
        self.assertEqual(report["counts"]["windows_normal_build_unresolved_text"], 1)
        self.assertTrue(report["packages"][0]["issues"])
        index["packages"][0]["files"][0]["line_end"] = 3
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "source header mismatch"):
            inventory(self.root, self.home)

    def test_upstream_must_be_pinned_and_notices_cannot_claim_full_text(self):
        _, path, index, _ = self.supplement()
        file = index["packages"][0]["files"][0]
        file["upstream"] = "https://example.invalid/repo/raw/main/LICENSE"
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "revision-pinned"):
            inventory(self.root, self.home)
        file["upstream"] = "https://example.invalid/repo/raw/" + file["exact_rev"] + "/LICENSE"
        file["kind"] = "upstream-notice"
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "full license"):
            inventory(self.root, self.home)

    def test_submodule_license_requires_published_source_binding(self):
        _, path, index, _ = self.supplement()
        file = index["packages"][0]["files"][0]
        rev = "b" * 40
        file.update(upstream_rev=rev, upstream="https://example.invalid/nested/raw/" + rev + "/LICENSE")
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "submodule source binding"):
            inventory(self.root, self.home)
        file["submodule"] = {
            "parent_tree": "https://example.invalid/repo/tree/" + file["exact_rev"] + "/nested",
            "source_path": "src/lib.rs",
            "source_sha256": digest(b"// Copyright fixture\r\n// Licensed under MIT\r\nfn main() {}\r\n"),
            "source_url": "https://example.invalid/nested/raw/" + rev + "/src/lib.rs",
        }
        path.write_text(json.dumps(index), encoding="utf-8")
        report, _ = inventory(self.root, self.home)
        self.assertEqual(report["counts"]["supplemental_packages"], 1)
        file["submodule"]["source_sha256"] = "0" * 64
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "submodule source binding"):
            inventory(self.root, self.home)

    def test_header_without_vcs_uses_archive_identity_and_stays_pending(self):
        _, path, index, _ = self.supplement("pending")
        item = index["packages"][0]
        item["exact_rev"] = item["vcs_sha256"] = item["files"][0]["exact_rev"] = None
        path.write_text(json.dumps(index), encoding="utf-8")
        report, _ = inventory(self.root, self.home)
        self.assertEqual(report["counts"]["supplemental_pending"], 1)
        item["files"][0]["kind"] = "upstream-license"
        path.write_text(json.dumps(index), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "revision-pinned"):
            inventory(self.root, self.home)

    def test_reuse_capture_checks_hashes_without_running_commands(self):
        self.supplement()
        names = ("all-targets-metadata", "windows-metadata", "windows-normal-build", "windows-runtime", "windows-with-dev")
        work = self.root / "target/license-audit"
        work.mkdir(parents=True)
        commands = {}
        for name in names:
            commands[name] = {"returncode": 1}
            for stream in ("stdout", "stderr"):
                path = work / (name + "." + stream)
                path.write_bytes(b"")
                commands[name][stream] = path.relative_to(self.root).as_posix()
                commands[name][stream + "_sha256"] = digest(b"")
        args = ["--root", str(self.root), "--cargo-home", str(self.home)]
        with patch("tools.audit_licenses.capture", return_value=commands), patch("sys.stdout", new_callable=io.StringIO):
            main(args)
        with patch("tools.audit_licenses.capture") as capture, patch("sys.stdout", new_callable=io.StringIO):
            main(args + ["--reuse-capture"])
            capture.assert_not_called()
            (work / "windows-metadata.stdout").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "evidence mismatch"):
                main(args + ["--reuse-capture"])

    def test_failed_command_preserves_output_and_error(self):
        work = self.root / "target/license-audit"
        work.mkdir(parents=True)
        with patch("tools.audit_licenses.subprocess.run", side_effect=OSError("cargo unavailable")):
            result = command(self.root, work, "metadata", ["cargo", "metadata", "--offline"], 1)
        self.assertIsNone(result["returncode"])
        self.assertIn("unavailable", result["error"])
        self.assertTrue((work / "metadata.stdout").exists())


if __name__ == "__main__":
    unittest.main()
