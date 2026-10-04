"""Isolated Redist evidence tests; synthetic PE metadata, no DLL execution/network."""
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.check_redist_evidence import collect, fingerprint, main, pe_versions, verify


class RedistEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.vs = self.root / "VS/18/BuildTools"
        self.redist = self.vs / "VC/Redist/MSVC/14.51.36231/x64/Microsoft.VC145.CRT"
        self.redist.mkdir(parents=True)
        self.dll = self.redist / "vcruntime140.dll"
        self.dll.write_bytes(b"synthetic DLL, never executed")
        self.instance = self.root / "_Instances/test"
        self.instance.mkdir(parents=True)
        self.product = "Microsoft.VisualStudio.Product.BuildTools"
        self.version = "18.8.12023.21"
        self.state = {"installationPath": str(self.vs), "installationVersion": self.version,
                      "product": {"id": self.product, "version": self.version, "installed": True}}
        self.catalog = {"info": {"buildVersion": self.version, "productMilestoneIsPreRelease": "False"},
                        "packages": [{"id": self.product, "version": self.version,
                                      "localizedResources": [{"language": "en-us", "title": "Visual Studio Build Tools 2026",
                                                              "license": "https://go.microsoft.com/fwlink/?LinkId=2327714"}]}]}
        self.write_metadata()
        self.policy = self.root / "policy.md"
        self.policy.write_text("Evidence only. No authorization asserted.", encoding="utf-8")
        self.manifest = self.root / "evidence.json"
        self.package = self.root / "package"
        self.package.mkdir()
        mock = patch("tools.check_redist_evidence.pe_versions", return_value={
            "machine": "x64", "file_version": "14.51.36231.0", "product_version": "14.51.36231.0", "version_flags": 0})
        self.version_mock = mock.start()
        self.addCleanup(mock.stop)

    def write_metadata(self):
        (self.instance / "state.json").write_text(json.dumps(self.state), encoding="utf-8")
        (self.instance / "catalog.json").write_text(json.dumps(self.catalog), encoding="utf-8")

    def collect(self):
        return collect(self.redist, self.instance, self.policy)

    def save(self):
        evidence = self.collect()
        self.manifest.write_text(json.dumps(evidence), encoding="utf-8")
        return evidence

    def test_exact_sources_versions_policy_and_no_approval(self):
        data = self.save()
        self.assertEqual(data["authorization"], "not_assessed")
        self.assertEqual(data["installation"]["product_id"], self.product)
        self.assertEqual(data["license_policy_ref"], fingerprint(self.policy))
        self.assertEqual(data["files"][0]["source"], fingerprint(self.dll))
        self.assertEqual(data["files"][0]["file_version"], "14.51.36231.0")
        self.assertEqual(verify(self.manifest), [])

    def test_source_mutation_fails(self):
        self.save()
        self.dll.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "Evidence changed"):
            verify(self.manifest)

    def test_policy_mutation_fails(self):
        self.save()
        self.policy.write_text("Changed scope", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Evidence changed"):
            verify(self.manifest)

    def test_catalog_or_manifest_mutation_fails(self):
        data = self.save()
        data["authorization"] = "approved"
        self.manifest.write_text(json.dumps(data), encoding="utf-8")
        with self.assertRaises(ValueError):
            verify(self.manifest)
        self.save()
        self.catalog["packages"][0]["localizedResources"][0]["license"] += "&changed=1"
        self.write_metadata()
        with self.assertRaises(ValueError):
            verify(self.manifest)

    def test_no_license_or_mismatched_version_or_preview_fails(self):
        self.catalog["packages"][0]["localizedResources"][0].pop("license")
        self.write_metadata()
        with self.assertRaisesRegex(ValueError, "license reference"):
            self.collect()
        self.catalog["packages"][0]["version"] = "17.0.0.0"
        self.write_metadata()
        with self.assertRaisesRegex(ValueError, "version mismatch"):
            self.collect()
        self.catalog["packages"][0]["version"] = self.version
        self.catalog["info"]["productMilestoneIsPreRelease"] = "True"
        self.write_metadata()
        with self.assertRaisesRegex(ValueError, "preview"):
            self.collect()

    def test_incomplete_install_recorded_not_hidden(self):
        self.state["product"]["installed"] = False
        self.state["properties"] = {"canceled": "1"}
        self.write_metadata()
        data = self.collect()
        self.assertEqual(len(data["warnings"]), 2)
        self.assertEqual(data["authorization"], "not_assessed")

    def test_system_and_wrong_sources_fail(self):
        for relative in ("System32/VC/Redist/MSVC/14.51.36231/x64/Microsoft.VC145.CRT",
                         "VC/Redist/MSVC/14.51.36231/onecore/x64/Microsoft.VC145.CRT",
                         "VC/Redist/MSVC/14.51.36231/debug_nonredist/x64/Microsoft.VC145.CRT",
                         "VC/Redist/MSVC/14.51.36231/x86/Microsoft.VC145.CRT",
                         "VC/Redist/MSVC/latest/x64/Microsoft.VC145.CRT"):
            path = self.vs / relative
            path.mkdir(parents=True)
            with self.subTest(path=path), self.assertRaises(ValueError):
                collect(path, self.instance, self.policy)
        self.state["installationPath"] = str(self.root)
        self.write_metadata()
        with self.assertRaises(ValueError):
            self.collect()

    def test_empty_missing_and_added_crt_fail(self):
        self.dll.write_bytes(b"")
        with self.assertRaises(ValueError):
            self.collect()
        self.dll.unlink()
        with self.assertRaisesRegex(ValueError, "No allowlisted"):
            self.collect()
        self.dll.write_bytes(b"restored")
        self.save()
        (self.redist / "msvcp140.dll").write_bytes(b"added")
        with self.assertRaisesRegex(ValueError, "Evidence changed"):
            verify(self.manifest)

    def test_missing_version_fails(self):
        self.version_mock.side_effect = ValueError("Missing version resource")
        with self.assertRaisesRegex(ValueError, "Missing version"):
            self.collect()

    def test_nonallowlisted_redist_recorded_not_approved(self):
        (self.redist / "vccorlib140.dll").write_bytes(b"not allowlisted")
        data = self.collect()
        self.assertEqual(data["excluded_directory_entries"], ["vccorlib140.dll"])
        self.assertEqual([f["name"] for f in data["files"]], ["vcruntime140.dll"])

    def test_package_subset_compared_and_unknown_crt_rejected(self):
        self.save()
        target = self.package / self.dll.name
        target.write_bytes(self.dll.read_bytes())
        self.assertEqual(verify(self.manifest, self.package), [self.dll.name])
        target.write_bytes(b"different")
        with self.assertRaisesRegex(ValueError, "differs"):
            verify(self.manifest, self.package)
        target.write_bytes(self.dll.read_bytes())
        (self.package / "msvcp140d.dll").write_bytes(b"debug")
        with self.assertRaisesRegex(ValueError, "no allowlisted source"):
            verify(self.manifest, self.package)

    def test_empty_package_fails_not_dependency_audit(self):
        self.save()
        with self.assertRaisesRegex(ValueError, "No app-local CRT"):
            verify(self.manifest, self.package)

    def test_hardlink_rejected(self):
        alias = self.root / "alias.dll"
        try:
            alias.hardlink_to(self.dll)
        except OSError as error:
            self.skipTest(str(error))
        with self.assertRaisesRegex(ValueError, "non-hardlinked"):
            self.collect()

    def test_reparse_attribute_rejected_without_symlink_privilege(self):
        original = Path.lstat
        class ReparseStat:
            st_file_attributes = 0x400
        def lstat(path, *args, **kwargs):
            if path == self.dll:
                return ReparseStat()
            return original(path, *args, **kwargs)
        with patch.object(Path, "lstat", lstat), patch.object(Path, "is_symlink", return_value=False):
            with self.assertRaisesRegex(ValueError, "Symlink/reparse"):
                self.collect()

    def test_symlink_rejected(self):
        source = self.root / "system.dll"
        source.write_bytes(b"outside")
        self.dll.unlink()
        try:
            self.dll.symlink_to(source)
        except OSError as error:
            self.skipTest(str(error))
        with self.assertRaisesRegex(ValueError, "Symlink/reparse"):
            self.collect()

    def test_cli_no_overwrite_and_verify(self):
        args = ["--manifest", str(self.manifest), "--redist-dir", str(self.redist),
                "--instance-dir", str(self.instance), "--license-policy-ref", str(self.policy)]
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.assertEqual(main(args), 0)
            original = self.manifest.read_bytes()
            self.assertEqual(main(args), 1)
            self.assertEqual(self.manifest.read_bytes(), original)
            self.assertEqual(main(["--verify", str(self.manifest)]), 0)

    def test_real_version_reader_rejects_non_pe_without_execution(self):
        # Imported reference is not the patched module attribute.
        with self.assertRaisesRegex(ValueError, "Not a PE"):
            pe_versions(self.dll)


if __name__ == "__main__":
    unittest.main()
