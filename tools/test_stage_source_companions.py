"""Offline synthetic archives and mocked HTTP/gh only; never publish real assets."""
from contextlib import contextmanager
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import zipfile

from tools import stage_source_companions as delivery


def setUpModule():
    (delivery.ROOT / "target/source-delivery-evaluation").mkdir(parents=True, exist_ok=True)


def write_json(path, value):
    path.write_text(json.dumps(value), encoding="utf-8")


@contextmanager
def mock_publication_review(version):
    # Only synthetic asset-flow tests substitute gate results; no policy is written.
    with patch.object(delivery.distribution_review, "check_review", return_value={"review": {"root_version": version}}) as parent, \
            patch.object(delivery.drawing, "load_lock", return_value={"synthetic": True}) as lock, \
            patch.object(delivery.drawing, "approval") as child, \
            patch.object(delivery.distribution_review, "git", return_value=b"a" * 40 + b"\n") as git:
        yield
        parent.assert_called_once_with(delivery.ROOT / delivery.distribution_review.POLICY)
        lock.assert_called_once_with()
        child.assert_called_once_with({"synthetic": True})
        if git.called:
            if git.call_count != 2:
                raise AssertionError("Expected both local tag and HEAD checks")


class SourceDeliveryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="test-source-delivery-", dir=delivery.ROOT / "target/source-delivery-evaluation")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / "package"
        (self.package / "docs/licenses").mkdir(parents=True)
        (self.package / "runtime/onnx").mkdir(parents=True)
        (self.package / "runtime/onnx/runtime.dll").write_bytes(b"synthetic DLL; never executed")
        self.output = self.root / "output"
        self.output.mkdir()
        self.inputs = self.root / "inputs"
        self.inputs.mkdir()
        self.lib = self.root / "lib"
        self.lib.mkdir()
        (self.lib / "synthetic.lib").write_bytes(b"synthetic static library")
        write_json(self.lib / "neo-asr-receipt.json", {"synthetic": True})
        write_json(self.lib / "neo-sherpa-asr.json", {
            "status": "native-validated", "options": {"SHERPA_ONNX_ENABLE_TTS": "OFF"},
            "libraries": {"synthetic.lib": delivery.file_record(self.lib / "synthetic.lib")}})
        self.lock = delivery.read_lock(delivery.LOCK, online=False)
        binding = {"manifest_sha256": delivery.file_record(self.lib / "neo-sherpa-asr.json")["sha256"],
                   "receipt_sha256": delivery.file_record(self.lib / "neo-asr-receipt.json")["sha256"]}
        self.addCleanup(patch.stopall)
        patch.object(delivery.native, "MANIFEST_SHA", binding["manifest_sha256"]).start()
        patch.object(delivery.native, "RECEIPT_SHA", binding["receipt_sha256"]).start()
        self.bundles = {}
        for source in self.lock["sources"]:
            kind = source["kind"]
            bundle = self.inputs / (kind + ".zip")
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("synthetic-source.txt", kind)
            self.bundles[kind] = bundle
            source.update(delivery.file_record(bundle))
            source["download_url"] = "https://sources.example.test/" + bundle.name
            if kind == "native":
                source["native_binding"] = binding
                source["expected_runtime_files_sha256"] = delivery.runtime.digest_json(
                    delivery.tree_files(self.package / "runtime/onnx"))
        self.lock_path = self.root / "lock.json"
        self.save_lock()
        self.distribution = self.root / "distribution"
        self.work = self.root / "work"
        self.stage_args = (self.lock_path, self.package, self.distribution, self.lib,
                           self.output, self.work, "owner/repo", "v1.2.3", "1.2.3")

    def save_lock(self):
        write_json(self.lock_path, self.lock)

    def fake_download(self, source, destination, deadline):
        shutil.copyfile(self.bundles[source["kind"]], destination)

    def stage(self):
        with patch.object(delivery, "download", side_effect=self.fake_download), \
                patch.object(delivery, "verify_gitbash") as gitbash, \
                patch.object(delivery.native, "verify") as native:
            result = delivery.stage(*self.stage_args)
        return result, gitbash, native

    def prepare_publish(self, version="1.2.3", variants=None):
        notice, _, _ = self.stage()
        if version != "1.2.3":
            notice = delivery.source_access(self.lock, "owner/repo", "v" + version, version)
            write_json(self.package / "docs/licenses/SOURCE-ACCESS.json", notice)
            for source in notice["sources"]:
                shutil.copyfile(self.bundles[source["kind"]], self.output / source["name"])
        prefixes = [f"neo-{version}"]
        if variants is not None:
            for variant in variants:
                shutil.copytree(self.package, self.package.with_name(f"{self.package.name}-{variant}"))
            prefixes = [f"neo-{version}-{variant}" for variant in variants]
        binary_names = [f"{prefix}-{suffix}" for prefix in prefixes
                        for suffix in ("portable-x64.zip", "installer-x64.exe")]
        for name in binary_names:
            (self.output / name).write_bytes(b"synthetic binary")
        notes = self.root / "notes.txt"
        notes.write_text("test notes", encoding="utf-8")
        names = [s["name"] for s in notice["sources"]] + binary_names
        assets = []
        for name in names:
            record = delivery.file_record(self.output / name)
            assets.append({"name": name, "size": record["size"], "digest": "sha256:" + record["sha256"], "state": "uploaded"})
        args = (self.lock_path, self.package, self.output, "owner/repo", "v" + version, version, notes, "-" in version)
        return args, assets

    def gh_results(self, assets, version="1.2.3"):
        return ["", json.dumps({"databaseId": 17, "tagName": "v" + version,
                                "isDraft": True, "isPrerelease": "-" in version}), json.dumps([assets]), ""]

    def test_null_url_blocks_before_network_or_output(self):
        self.lock["sources"][1]["download_url"] = None
        self.save_lock()
        with patch.object(delivery, "download") as download, self.assertRaisesRegex(ValueError, "BLOCKED: native download_url is null"):
            delivery.stage(*self.stage_args)
        download.assert_not_called()
        self.assertFalse(self.work.exists())
        self.assertEqual(list(self.output.iterdir()), [])

    def test_checked_in_lock_remains_blocked_in_actual_cli(self):
        result = subprocess.run([sys.executable, "-B", str(delivery.ROOT / "tools/stage_source_companions.py"),
                                 "stage", "--package", str(self.package), "--output", str(self.output),
                                 "--distribution", str(self.distribution), "--native-lib", str(self.lib),
                                 "--repository", "owner/repo", "--tag", "v1.2.3", "--version", "1.2.3"],
                                capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("download_url is null", result.stderr)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_stage_checks_both_validators_and_writes_only_planned_access(self):
        notice, gitbash, native = self.stage()
        gitbash.assert_called_once()
        native.assert_called_once()
        self.assertEqual(notice["status"], "planned-until-publication")
        self.assertEqual(delivery.load_json(self.package / "docs/licenses/SOURCE-ACCESS.json"), notice)
        for source in notice["sources"]:
            self.assertEqual(delivery.file_record(self.output / source["name"]), {k: source[k] for k in ("size", "sha256")})
            self.assertEqual(source["url"], "https://github.com/owner/repo/releases/download/v1.2.3/" + source["name"])
        self.assertEqual(list(self.work.iterdir()), [])
        self.assertEqual([p.name for p in (self.package / "docs/licenses").iterdir()], ["SOURCE-ACCESS.json"])

    def test_wrong_zip_hash_or_size_never_copies_outputs(self):
        for key, value in (("sha256", "0" * 64), ("size", 1)):
            with self.subTest(key=key):
                lock = copy.deepcopy(self.lock)
                lock["sources"][0][key] = value
                write_json(self.lock_path, lock)
                with patch.object(delivery, "download", side_effect=self.fake_download), self.assertRaisesRegex(ValueError, "Source ZIP size/SHA-256 mismatch"):
                    delivery.stage(*self.stage_args)
                self.assertEqual(list(self.output.iterdir()), [])

    def test_native_old_binding_does_not_validate_different_ci_build(self):
        (self.lib / "neo-asr-receipt.json").write_bytes(b"different CI receipt")
        with self.assertRaisesRegex(ValueError, "not valid for arbitrary CI"):
            self.stage()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_native_runtime_extra_file_and_static_library_tamper_fail(self):
        extra = self.package / "runtime/onnx/extra.dll"
        extra.write_bytes(b"extra")
        with self.assertRaisesRegex(ValueError, "Native runtime file set mismatch"):
            self.stage()
        extra.unlink()
        (self.lib / "synthetic.lib").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "Native library differs"):
            self.stage()
        self.assertEqual(list(self.output.iterdir()), [])

    def test_validator_failure_no_notice_or_output(self):
        with patch.object(delivery, "download", side_effect=self.fake_download), \
                patch.object(delivery, "verify_gitbash", side_effect=ValueError("binding mismatch")), \
                self.assertRaisesRegex(ValueError, "binding mismatch"):
            delivery.stage(*self.stage_args)
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertFalse((self.package / "docs/licenses/SOURCE-ACCESS.json").exists())

    def test_existing_output_not_overwritten_and_download_not_started(self):
        existing = self.output / "neo-1.2.3-native-sources.zip"
        existing.write_bytes(b"keep")
        with patch.object(delivery, "download") as download, self.assertRaisesRegex(ValueError, "overwrite"):
            delivery.stage(*self.stage_args)
        download.assert_not_called()
        self.assertEqual(existing.read_bytes(), b"keep")

    def test_aggregate_budget_and_identity_fail_before_network(self):
        with patch.object(delivery, "download") as download:
            with self.assertRaisesRegex(ValueError, "budget"):
                delivery.stage(*self.stage_args, max_download_mib=0.000001)
            for repo, tag, version in (("../repo", "v1.2.3", "1.2.3"), ("owner/repo", "wrong", "1.2.3")):
                with self.assertRaises(ValueError):
                    delivery.source_access(self.lock, repo, tag, version)
        download.assert_not_called()

    def test_fixed_https_only_and_binding_review_required(self):
        for url in ("http://example.test/a", "file:///a", "https://u:p@example.test/a", "https://example.test/a#x"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                delivery.https_url(url)
        self.lock["sources"][1]["native_binding"]["receipt_sha256"] = "0" * 64
        self.save_lock()
        with self.assertRaisesRegex(ValueError, "new binding needs explicit review"):
            delivery.read_lock(self.lock_path)

    def test_publish_creates_single_draft_with_all_assets_then_api_verifies_then_edits(self):
        for version in ("1.2.3", "1.2.3-rc.1"):
            with self.subTest(version=version):
                if version != "1.2.3":
                    for path in self.output.iterdir():
                        path.unlink()
                    (self.package / "docs/licenses/SOURCE-ACCESS.json").unlink()
                args, assets = self.prepare_publish(version)
                with mock_publication_review(version), patch.object(delivery, "run_gh", side_effect=self.gh_results(assets, version)) as gh:
                    delivery.publish(*args)
                calls = [call.args[0] for call in gh.call_args_list]
                self.assertEqual(len(calls), 4)
                self.assertEqual(calls[0][:3], ["release", "create", "v" + version])
                self.assertIn("--draft", calls[0])
                self.assertIn("--verify-tag", calls[0])
                for asset in assets:
                    self.assertIn(str(self.output / asset["name"]), calls[0])
                self.assertEqual("--prerelease" in calls[0], "-" in version)
                self.assertEqual(calls[1][:2], ["release", "view"])
                self.assertIn("--paginate", calls[2])
                self.assertIn("--slurp", calls[2])
                self.assertIn("repos/owner/repo/releases/17/assets?per_page=100", calls[2])
                self.assertEqual(calls[3][:2], ["release", "edit"])
                self.assertIn("--draft=false", calls[3])
                self.assertIn("--prerelease=" + str("-" in version).lower(), calls[3])

    def test_missing_changed_source_and_notice_fail_before_any_gh(self):
        args, assets = self.prepare_publish()
        source = self.output / assets[0]["name"]
        original = source.read_bytes()
        source.write_bytes(b"tamper")
        with mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, self.assertRaisesRegex(ValueError, "missing/changed"):
            delivery.publish(*args)
        gh.assert_not_called()
        source.write_bytes(original)
        notice = self.package / "docs/licenses/SOURCE-ACCESS.json"
        write_json(notice, {"status": "published"})
        with mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, self.assertRaisesRegex(ValueError, "notice differs"):
            delivery.publish(*args)
        gh.assert_not_called()

    def test_remote_missing_duplicate_extra_wrong_size_or_digest_never_publishes(self):
        args, assets = self.prepare_publish()
        variants = [assets[:-1], assets + [assets[0]], assets + [dict(assets[0], name="extra.zip")]]
        for changes in ({"size": 0}, {"digest": None}, {"digest": "sha256:" + "0" * 64}, {"state": "new"}, {"name": "wrong.zip"}):
            variant = copy.deepcopy(assets)
            variant[0].update(changes)
            variants.append(variant)
        for variant in variants:
            with self.subTest(variant=variant), mock_publication_review(args[5]), patch.object(delivery, "run_gh", side_effect=self.gh_results(variant)) as gh:
                with self.assertRaises(ValueError):
                    delivery.publish(*args)
                self.assertEqual(gh.call_count, 3)
                self.assertFalse(any(call.args[0][:2] == ["release", "edit"] for call in gh.call_args_list))

    def test_gh_failure_and_non_draft_leave_unpublished(self):
        args, assets = self.prepare_publish()
        results = self.gh_results(assets)
        results[1] = json.dumps({"databaseId": 17, "tagName": "v1.2.3", "isDraft": False, "isPrerelease": False})
        with mock_publication_review(args[5]), patch.object(delivery, "run_gh", side_effect=results) as gh, self.assertRaisesRegex(ValueError, "private draft"):
            delivery.publish(*args)
        self.assertEqual(gh.call_count, 2)
        with mock_publication_review(args[5]), patch.object(delivery, "run_gh", side_effect=subprocess.CalledProcessError(1, ["gh"])) as gh:
            with self.assertRaises(subprocess.CalledProcessError):
                delivery.publish(*args)
        self.assertEqual(gh.call_count, 1)

    def test_gh_subprocess_is_bounded_and_checked(self):
        with patch.object(delivery.subprocess, "run", return_value=Mock(stdout="ok")) as run:
            self.assertEqual(delivery.run_gh(["release", "view", "v1.2.3"]), "ok")
        self.assertEqual(run.call_args.args[0], ["gh", "release", "view", "v1.2.3"])
        self.assertTrue(run.call_args.kwargs["check"])
        self.assertEqual(run.call_args.kwargs["timeout"], 600)


class VariantPublicationTests(unittest.TestCase):
    setUp = SourceDeliveryTests.setUp
    save_lock = SourceDeliveryTests.save_lock
    fake_download = SourceDeliveryTests.fake_download
    stage = SourceDeliveryTests.stage
    prepare_publish = SourceDeliveryTests.prepare_publish
    gh_results = SourceDeliveryTests.gh_results

    def prepare_variants(self):
        # Exercise the real stage-once, copy-to-siblings layout with synthetic inputs.
        self.package.rename(self.output / "neo")
        self.package = self.output / "neo"
        self.stage_args = (self.lock_path, self.package, *self.stage_args[2:])
        (self.package / "docs/licenses/SOURCE-NOTICE.txt").write_text("synthetic source notice", encoding="utf-8")
        args, assets = self.prepare_publish("1.2.3-pre11", variants=("int8", "fp32"))
        for variant in ("int8", "fp32"):
            package = self.package.with_name("neo-" + variant)
            models = package / "resources/models/math"
            models.mkdir(parents=True)
            (models / "model.onnx").write_bytes(b"synthetic math model " + variant.encode())
            delivery.inventory(package, variant, args[5])
        return args, assets

    def cli_args(self, args):
        return ["source-delivery", "publish", "--lock", str(args[0]), "--package", str(args[1]),
                "--output", str(args[2]), "--repository", args[3], "--tag", args[4], "--version", args[5],
                "--notes", str(args[6]), "--prerelease", "true"]

    @contextmanager
    def offline(self):
        with patch.object(delivery.urllib.request, "build_opener", side_effect=AssertionError("Unexpected network")), \
                patch.object(delivery.subprocess, "run", side_effect=AssertionError("Unexpected subprocess")):
            yield

    def test_cli_publishes_both_variants_and_shared_sources_in_one_verified_draft(self):
        with self.offline():
            args, assets = self.prepare_variants()
            with mock_publication_review(args[5]), \
                    patch.object(sys, "argv", self.cli_args(args) + ["--variants", "int8", "fp32"]), \
                    patch.object(delivery, "run_gh", side_effect=self.gh_results(assets, args[5])) as gh:
                delivery.main()
        names = {f"neo-{args[5]}-{variant}-{suffix}" for variant in ("int8", "fp32")
                 for suffix in ("portable-x64.zip", "installer-x64.exe")}
        names.update(f"neo-{args[5]}-{kind}-sources.zip" for kind in ("gitbash", "native"))
        self.assertEqual({asset["name"] for asset in assets}, names)
        calls = [call.args[0] for call in gh.call_args_list]
        self.assertEqual(len(calls), 4)
        self.assertEqual(calls[0][:3], ["release", "create", args[4]])
        self.assertEqual(calls[0][3:9], [str(self.output / asset["name"]) for asset in assets])
        self.assertEqual(calls[0][9], "--repo")
        for flag in ("--draft", "--verify-tag", "--prerelease"):
            self.assertIn(flag, calls[0])
        self.assertEqual(calls[1][:2], ["release", "view"])
        self.assertIn("--paginate", calls[2])
        self.assertIn("--slurp", calls[2])
        self.assertIn("repos/owner/repo/releases/17/assets?per_page=100", calls[2])
        self.assertEqual(calls[3][:2], ["release", "edit"])
        self.assertIn("--draft=false", calls[3])
        self.assertIn("--prerelease=true", calls[3])

    def test_every_binary_and_shared_source_is_mandatory_before_any_gh(self):
        args, assets = self.prepare_variants()
        # Legacy binaries must never fill a missing variant slot.
        for suffix in ("portable-x64.zip", "installer-x64.exe"):
            (self.output / f"neo-{args[5]}-{suffix}").write_bytes(b"legacy")
        for asset in assets:
            path = self.output / asset["name"]
            original = path.read_bytes()
            for content in (None, b""):
                with self.subTest(name=path.name, content=content), self.offline(), \
                        mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh:
                    if content is None:
                        path.unlink()
                    else:
                        path.write_bytes(content)
                    with self.assertRaises(ValueError):
                        delivery.publish(*args, variants=["int8", "fp32"])
                    gh.assert_not_called()
                    path.write_bytes(original)

    def test_unknown_duplicate_empty_and_partial_variants_rejected_before_source_inputs(self):
        args, _ = self.prepare_variants()
        for variants in ([], ["int8"], ["fp32"], ["int8", "int8"], ["fp32", "fp32"],
                         ["int8", "fp16"], ["INT8", "fp32"], ["../int8", "fp32"],
                         ["int8", "fp32", "int8"]):
            with self.subTest(variants=variants), self.offline(), mock_publication_review(args[5]), \
                    patch.object(delivery, "read_lock") as lock, patch.object(delivery, "run_gh") as gh, \
                    self.assertRaisesRegex(ValueError, "exactly once"):
                delivery.publish(*args, variants=variants)
            lock.assert_not_called()
            gh.assert_not_called()

    def test_cli_rejects_bad_variant_and_stage_variants(self):
        args, _ = self.prepare_variants()
        for flags in (["--variants", "int8", "fp16"], ["--variants"]):
            with self.subTest(flags=flags), self.offline(), \
                    patch.object(sys, "argv", self.cli_args(args) + flags), \
                    patch.object(sys, "stderr", io.StringIO()), patch.object(delivery, "publish") as publish, \
                    self.assertRaises(SystemExit):
                delivery.main()
            publish.assert_not_called()
        argv = self.cli_args(args) + ["--variants", "int8", "fp32"]
        argv[1] = "stage"
        with self.offline(), patch.object(sys, "argv", argv), patch.object(sys, "stderr", io.StringIO()) as stderr, \
                patch.object(delivery, "stage") as stage, self.assertRaises(SystemExit):
            delivery.main()
        self.assertIn("publish-only", stderr.getvalue())
        stage.assert_not_called()

    def test_each_variant_requires_identical_delivery_manifest_and_notices(self):
        args, _ = self.prepare_variants()
        for variant in ("int8", "fp32"):
            directory = self.package.with_name("neo-" + variant)
            for name in ("SOURCE-ACCESS.json", "SOURCE-NOTICE.txt"):
                path = directory / "docs/licenses" / name
                original = path.read_bytes()
                for content in (None, b"{}", original + b"\n"):
                    with self.subTest(variant=variant, name=name, content=content), self.offline(), \
                            mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh:
                        if content is None:
                            path.unlink()
                        else:
                            path.write_bytes(content)
                        with self.assertRaises((ValueError, OSError)):
                            delivery.publish(*args, variants=["int8", "fp32"])
                        gh.assert_not_called()
                        path.write_bytes(original)
            extra = directory / "docs/licenses/extra.txt"
            extra.write_bytes(b"not in base")
            with self.offline(), mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, \
                    self.assertRaisesRegex(ValueError, "notices differ"):
                delivery.publish(*args, variants=["int8", "fp32"])
            gh.assert_not_called()
            extra.unlink()
            absent = directory.with_name(directory.name + "-absent")
            directory.rename(absent)
            with self.offline(), mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, \
                    self.assertRaises(OSError):
                delivery.publish(*args, variants=["int8", "fp32"])
            gh.assert_not_called()
            absent.rename(directory)

    def test_matching_variant_notices_cannot_override_lock_or_base(self):
        args, _ = self.prepare_variants()
        for package in (self.package, self.package.with_name("neo-int8"), self.package.with_name("neo-fp32")):
            path = package / "docs/licenses/SOURCE-ACCESS.json"
            notice = delivery.load_json(path)
            notice["sources"][0]["sha256"] = "0" * 64
            write_json(path, notice)
        with self.offline(), mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "notice differs from lock"):
            delivery.publish(*args, variants=["int8", "fp32"])
        gh.assert_not_called()

    def test_remote_partial_duplicate_extra_or_changed_assets_leave_variant_draft_unpublished(self):
        args, assets = self.prepare_variants()
        bad_assets = [assets[:i] + assets[i + 1:] for i in range(len(assets))]
        bad_assets += [assets + [assets[0]], assets + [dict(assets[0], name="extra.zip")]]
        for i in range(len(assets)):
            for change in ({"digest": None}, {"digest": "sha256:" + "0" * 64}, {"size": 0}, {"state": "new"}):
                changed = copy.deepcopy(assets)
                changed[i].update(change)
                bad_assets.append(changed)
        for remote in bad_assets:
            with self.subTest(remote=remote), self.offline(), mock_publication_review(args[5]), \
                    patch.object(delivery, "run_gh", side_effect=self.gh_results(remote, args[5])) as gh, \
                    self.assertRaises(ValueError):
                delivery.publish(*args, variants=["fp32", "int8"])
            self.assertEqual(gh.call_count, 3)
            self.assertFalse(any(call.args[0][:2] == ["release", "edit"] for call in gh.call_args_list))

    def test_changed_missing_and_extra_package_files_block_before_gh(self):
        args, _ = self.prepare_variants()
        for variant in ("int8", "fp32"):
            package = self.package.with_name("neo-" + variant)
            for name in ("resources/models/math/model.onnx", "runtime/onnx/runtime.dll"):
                path = package / name
                original = path.read_bytes()
                for content in (None, b"x" * len(original)):
                    with self.subTest(variant=variant, name=name, content=content), self.offline(), \
                            mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh:
                        if content is None:
                            path.unlink()
                        else:
                            path.write_bytes(content)
                        with self.assertRaisesRegex(ValueError, "package manifest differs"):
                            delivery.publish(*args, variants=["int8", "fp32"])
                        gh.assert_not_called()
                        path.write_bytes(original)
            extra = package / "resources/models/math/extra.onnx"
            extra.write_bytes(b"unexpected model")
            with self.offline(), mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh, \
                    self.assertRaisesRegex(ValueError, "package manifest differs"):
                delivery.publish(*args, variants=["int8", "fp32"])
            gh.assert_not_called()
            extra.unlink()

    def test_missing_wrong_identity_and_unsafe_inventory_block_before_gh(self):
        args, _ = self.prepare_variants()
        for variant in ("int8", "fp32"):
            path = self.package.with_name("neo-" + variant) / delivery.PACKAGE_MANIFEST
            original = delivery.load_json(path)
            unsafe = copy.deepcopy(original)
            unsafe["files"]["../outside"] = next(iter(unsafe["files"].values()))
            for manifest in (None, dict(original, version="9.9.9"),
                             dict(original, variant="fp32" if variant == "int8" else "int8"),
                             dict(original, schema=2), dict(original, files={}), unsafe):
                with self.subTest(variant=variant, manifest=manifest), self.offline(), \
                        mock_publication_review(args[5]), patch.object(delivery, "run_gh") as gh:
                    if manifest is None:
                        path.unlink()
                    else:
                        write_json(path, manifest)
                    with self.assertRaises((ValueError, OSError)):
                        delivery.publish(*args, variants=["int8", "fp32"])
                    gh.assert_not_called()
                    write_json(path, original)

    def test_variants_do_not_bypass_real_publication_gate(self):
        missing = delivery.ROOT / "target/source-delivery-gate-test-unused"
        with self.offline(), patch.object(delivery, "run_gh") as gh, \
                patch.object(delivery, "read_lock") as lock, \
                self.assertRaisesRegex(ValueError, "Outstanding distribution blockers|not APPROVED"):
            delivery.publish(delivery.LOCK, missing, missing, "owner/repo", "v1.2.3-pre11", "1.2.3-pre11",
                             missing, True, variants=["int8", "fp32"])
        gh.assert_not_called()
        lock.assert_not_called()


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="test-inventory-", dir=delivery.ROOT / "target/source-delivery-evaluation")
        self.addCleanup(self.temp.cleanup)
        self.package = Path(self.temp.name) / "neo-int8"
        self.package.mkdir()
        (self.package / "neo.exe").write_bytes(b"synthetic executable")
        models = self.package / "resources/models/math"
        models.mkdir(parents=True)
        (models / "model.onnx").write_bytes(b"synthetic model")
        (models / delivery.PACKAGE_MANIFEST).write_bytes(b"nested namesake is not excluded")
        self.destination = self.package / delivery.PACKAGE_MANIFEST

    offline = VariantPublicationTests.offline

    def test_cli_inventory_records_full_tree_without_repository_tag_or_output(self):
        paths = ("neo.exe", "resources/models/math/model.onnx", "resources/models/math/PACKAGE-MANIFEST.json")
        expected_files = {name: {"size": len((self.package / name).read_bytes()),
                                 "sha256": hashlib.sha256((self.package / name).read_bytes()).hexdigest()}
                          for name in paths}
        for variant in ("int8", "fp32"):
            with self.subTest(variant=variant), self.offline(), \
                    patch.object(sys, "argv", ["source-delivery", "inventory", "--package", str(self.package),
                                               "--variant", variant, "--version", "1.2.3-pre11"]), \
                    patch.object(sys, "stdout", io.StringIO()):
                delivery.main()
            manifest = delivery.load_json(self.destination)
            self.assertEqual(manifest, {"schema": 1, "version": "1.2.3-pre11", "variant": variant, "files": expected_files})
            self.assertEqual(manifest, delivery.package_manifest(self.package, variant, "1.2.3-pre11"))
            self.destination.unlink()

    def test_inventory_refuses_existing_output_and_invalid_identity(self):
        self.destination.write_bytes(b"preserve existing inventory")
        with self.assertRaisesRegex(ValueError, "overwrite"):
            delivery.inventory(self.package, "int8", "1.2.3-pre11")
        self.assertEqual(self.destination.read_bytes(), b"preserve existing inventory")
        self.destination.unlink()
        for variant, version in (("fp16", "1.2.3"), ("int8", "../1.2.3"), (None, "1.2.3")):
            with self.subTest(variant=variant, version=version), self.assertRaises(ValueError):
                delivery.inventory(self.package, variant, version)
            self.assertFalse(self.destination.exists())

    def test_inventory_rejects_links_before_output(self):
        original_lstat = Path.lstat
        for name in ("neo.exe", "resources/models", delivery.PACKAGE_MANIFEST):
            blocked = (self.package / name).absolute()

            def lstat(path, *args, **kwargs):
                if path.absolute() == blocked:
                    return Mock(st_mode=0o100644, st_file_attributes=0x400)
                return original_lstat(path, *args, **kwargs)

            with self.subTest(name=name), patch.object(Path, "lstat", lstat), \
                    self.assertRaisesRegex(ValueError, "reparse/symlink"):
                delivery.inventory(self.package, "int8", "1.2.3")
            self.assertFalse(self.destination.exists())
        linked = self.package / "linked.exe"
        os.link(self.package / "neo.exe", linked)
        with self.assertRaisesRegex(ValueError, "without links"):
            delivery.inventory(self.package, "int8", "1.2.3")
        self.assertFalse(self.destination.exists())

    def test_cli_command_specific_required_arguments(self):
        common = ["--package", str(self.package), "--version", "1.2.3"]
        cases = [("inventory", common, "requires --variant")]
        for command in ("stage", "publish"):
            flags = common + ["--output", str(self.package.parent), "--repository", "owner/repo", "--tag", "v1.2.3"]
            for flag in ("--output", "--repository", "--tag"):
                index = flags.index(flag)
                cases.append((command, flags[:index] + flags[index + 2:], "requires --output, --repository and --tag"))
        for command, flags, message in cases:
            with self.subTest(command=command, flags=flags), self.offline(), \
                    patch.object(sys, "argv", ["source-delivery", command, *flags]), \
                    patch.object(sys, "stderr", io.StringIO()) as stderr, \
                    patch.object(delivery, "inventory") as inventory, \
                    patch.object(delivery, "stage") as stage, patch.object(delivery, "publish") as publish, \
                    self.assertRaises(SystemExit):
                delivery.main()
            self.assertIn(message, stderr.getvalue())
            inventory.assert_not_called()
            stage.assert_not_called()
            publish.assert_not_called()


class LocalSourceTests(unittest.TestCase):
    setUp = SourceDeliveryTests.setUp
    save_lock = SourceDeliveryTests.save_lock

    def local_stage(self, **kwargs):
        for source in self.lock["sources"]:
            source["download_url"] = None
        self.save_lock()
        options = {"local_gitbash": self.bundles["gitbash"], "local_native": self.bundles["native"]}
        options.update(kwargs)
        return delivery.stage(*self.stage_args, **options)

    @contextmanager
    def offline_validators(self):
        with patch.object(delivery, "download", side_effect=AssertionError("No downloads")) as download, \
                patch.object(delivery.urllib.request, "build_opener", side_effect=AssertionError("No HTTP")), \
                patch.object(delivery, "run_gh", side_effect=AssertionError("No gh")), \
                patch.object(delivery, "verify_gitbash") as gitbash, \
                patch.object(delivery.native, "verify") as native:
            yield gitbash, native
            download.assert_not_called()

    def assert_no_delivery(self):
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertFalse((self.package / "docs/licenses/SOURCE-ACCESS.json").exists())
        if self.work.exists():
            self.assertEqual(list(self.work.iterdir()), [])

    def test_paired_local_null_urls_use_original_paths_and_both_validators(self):
        before = {k: delivery.file_record(p) for k, p in self.bundles.items()}
        with self.offline_validators() as (gitbash, native), \
                patch.object(delivery, "read_lock", wraps=delivery.read_lock) as read_lock, \
                patch.object(delivery.shutil, "copyfileobj", wraps=shutil.copyfileobj) as copy_file:
            notice = self.local_stage()
        read_lock.assert_called_once_with(self.lock_path, online=False)
        self.assertEqual(copy_file.call_count, 2)  # Only final staging, not workspace copies.
        self.assertEqual(gitbash.call_args.args[0], self.bundles["gitbash"])
        native.assert_called_once_with(self.bundles["native"])
        self.assertEqual(notice["status"], "planned-until-publication")
        self.assertEqual(delivery.load_json(self.package / "docs/licenses/SOURCE-ACCESS.json"), notice)
        for source in notice["sources"]:
            self.assertEqual(delivery.file_record(self.output / source["name"]), before[source["kind"]])
        self.assertEqual({k: delivery.file_record(p) for k, p in self.bundles.items()}, before)
        self.assertEqual(list(self.work.iterdir()), [])

    def test_each_unpaired_argument_rejected_before_reading_lock(self):
        for kind in ("gitbash", "native"):
            with self.subTest(kind=kind), patch.object(delivery, "read_lock") as read_lock, \
                    self.assertRaisesRegex(ValueError, "requires both"):
                delivery.stage(*self.stage_args, **{"local_" + kind: self.bundles[kind]})
            read_lock.assert_not_called()
        self.assert_no_delivery()

    def test_missing_and_tampered_local_sources_never_stage_partial_outputs(self):
        with self.offline_validators():
            for kind in ("gitbash", "native"):
                path = self.bundles[kind]
                original = path.read_bytes()
                path.unlink()
                with self.subTest(kind=kind, case="missing"), self.assertRaisesRegex(ValueError, "Missing local source"):
                    self.local_stage()
                self.assert_no_delivery()
                for data in (original + b"tamper", bytes([original[0] ^ 1]) + original[1:]):
                    path.write_bytes(data)
                    with self.subTest(kind=kind, case="tampered"), self.assertRaisesRegex(ValueError, "size/SHA-256 mismatch"):
                        self.local_stage()
                    self.assert_no_delivery()
                path.write_bytes(original)

    def test_untrusted_paths_rejected_before_lock_or_output(self):
        original = self.stage_args
        cases = [(0, delivery.ROOT.parent / "lock.json"),
                 (1, delivery.ROOT / "target/combined-evaluation/package"),
                 (2, delivery.ROOT.parent / "distribution"),
                 (3, delivery.ROOT.parent / "lib"),
                 (4, delivery.ROOT / "dist"),
                 (5, delivery.ROOT / "target/source-delivery"),
                 (4, self.root / ".." / self.root.name / "output")]
        with self.offline_validators(), patch.object(delivery, "read_lock") as read_lock:
            for index, path in cases:
                args = list(original)
                args[index] = path
                self.stage_args = tuple(args)
                with self.subTest(path=path), self.assertRaises(ValueError):
                    self.local_stage()
                self.assert_no_delivery()
            self.stage_args = original
            for kind in ("gitbash", "native"):
                with self.subTest(kind=kind), self.assertRaises(ValueError):
                    self.local_stage(**{"local_" + kind: delivery.ROOT.parent / "untrusted.zip"})
        read_lock.assert_not_called()

    def test_symlink_input_and_output_parent_rejected(self):
        link = self.root / "linked-inputs"
        try:
            link.symlink_to(self.inputs, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"Symlink creation unavailable: {error}")
        with self.offline_validators():
            with self.assertRaisesRegex(ValueError, "reparse/symlink"):
                self.local_stage(local_gitbash=link / "gitbash.zip")
            args = list(self.stage_args)
            args[4] = link / "output"
            self.stage_args = tuple(args)
            with self.assertRaisesRegex(ValueError, "reparse/symlink"):
                self.local_stage()
        self.assert_no_delivery()

    @unittest.skipUnless(os.name == "nt", "Windows junction test")
    def test_junction_input_and_output_parent_rejected(self):
        link = self.root / "junction"
        result = subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(link), str(self.inputs)],
                                capture_output=True, timeout=10)
        if result.returncode:
            self.skipTest("Host does not permit creating junctions")
        try:
            with self.offline_validators():
                with self.assertRaisesRegex(ValueError, "reparse/symlink"):
                    self.local_stage(local_gitbash=link / "gitbash.zip")
                args = list(self.stage_args)
                args[4] = link / "output"
                self.stage_args = tuple(args)
                with self.assertRaisesRegex(ValueError, "reparse/symlink"):
                    self.local_stage()
            self.assert_no_delivery()
        finally:
            link.rmdir()

    def test_reparse_point_input_rejected_even_if_regular_file(self):
        original_lstat = Path.lstat
        blocked = self.bundles["native"].absolute()

        def lstat(path, *args, **kwargs):
            if path == blocked:
                return Mock(st_mode=0o100644, st_file_attributes=0x400)
            return original_lstat(path, *args, **kwargs)

        with self.offline_validators(), patch.object(Path, "lstat", lstat), \
                self.assertRaisesRegex(ValueError, "reparse/symlink"):
            self.local_stage()
        self.assert_no_delivery()

    def test_full_validator_and_native_binding_failures_do_not_stage(self):
        with self.offline_validators() as (gitbash, native):
            for validator in (gitbash, native):
                validator.side_effect = ValueError("full validator failed")
                with self.assertRaisesRegex(ValueError, "full validator failed"):
                    self.local_stage()
                self.assert_no_delivery()
                validator.side_effect = None
            (self.lib / "synthetic.lib").write_bytes(b"tampered library")
            with self.assertRaisesRegex(ValueError, "Native library differs"):
                self.local_stage()
            self.assert_no_delivery()

    def test_copy_or_notice_failure_rolls_back_all_created_outputs(self):
        copy_file = shutil.copyfileobj
        calls = 0

        def fail_second(inp, out, length):
            nonlocal calls
            calls += 1
            if calls == 2:
                out.write(b"partial")
                raise OSError("copy failed")
            copy_file(inp, out, length)

        with self.offline_validators():
            with patch.object(delivery.shutil, "copyfileobj", side_effect=fail_second), \
                    self.assertRaisesRegex(OSError, "copy failed"):
                self.local_stage()
            self.assert_no_delivery()
            with patch.object(delivery.json, "dump", side_effect=OSError("notice failed")), \
                    self.assertRaisesRegex(OSError, "notice failed"):
                self.local_stage()
            self.assert_no_delivery()
            with patch.object(delivery.shutil, "copyfileobj", side_effect=lambda inp, out, length: out.write(b"changed")), \
                    self.assertRaisesRegex(ValueError, "Staged source changed"):
                self.local_stage()
            self.assert_no_delivery()

    def test_existing_notice_or_asset_preserved_before_validation(self):
        for path in (self.package / "docs/licenses/SOURCE-ACCESS.json",
                     self.output / delivery.asset_name("1.2.3", "gitbash"),
                     self.output / delivery.asset_name("1.2.3", "native")):
            path.write_bytes(b"keep")
            with self.offline_validators() as (gitbash, native), self.assertRaisesRegex(ValueError, "overwrite"):
                self.local_stage()
            gitbash.assert_not_called()
            native.assert_not_called()
            self.assertEqual(path.read_bytes(), b"keep")
            path.unlink()
            self.assert_no_delivery()

    def test_local_budget_is_still_enforced(self):
        with self.offline_validators(), self.assertRaisesRegex(ValueError, "budget"):
            self.local_stage(max_download_mib=0.000001)
        self.assert_no_delivery()

    def test_cli_paired_relative_paths_default_to_evaluation_work(self):
        args = ["source-delivery", "stage", "--package", str(self.package.relative_to(delivery.ROOT)),
                "--output", str(self.output.relative_to(delivery.ROOT)),
                "--distribution", str(self.distribution.relative_to(delivery.ROOT)),
                "--native-lib", str(self.lib.relative_to(delivery.ROOT)),
                "--lock", str(self.lock_path.relative_to(delivery.ROOT)),
                "--repository", "owner/repo", "--tag", "v1.2.3", "--version", "1.2.3",
                "--local-gitbash", str(self.bundles["gitbash"].relative_to(delivery.ROOT)),
                "--local-native", str(self.bundles["native"].relative_to(delivery.ROOT))]
        for source in self.lock["sources"]:
            source["download_url"] = None
        self.save_lock()
        with self.offline_validators(), patch.object(sys, "argv", args), \
                patch.object(sys, "stdout", io.StringIO()), \
                patch.object(delivery, "stage", wraps=delivery.stage) as stage:
            delivery.main()
        self.assertEqual(stage.call_args.args[5], delivery.ROOT / "target/source-delivery-evaluation/work")
        self.assertTrue((self.package / "docs/licenses/SOURCE-ACCESS.json").is_file())

    def test_cli_rejects_unpaired_and_publish_local_flags(self):
        common = ["source-delivery", "stage", "--package", str(self.package), "--output", str(self.output),
                  "--repository", "owner/repo", "--tag", "v1.2.3", "--version", "1.2.3"]
        for kind in ("gitbash", "native"):
            with patch.object(sys, "argv", common + ["--local-" + kind, str(self.bundles[kind])]), \
                    patch.object(sys, "stderr", io.StringIO()) as stderr, \
                    patch.object(delivery, "stage") as stage, self.assertRaises(SystemExit):
                delivery.main()
            self.assertIn("requires both", stderr.getvalue())
            stage.assert_not_called()
        common[1] = "publish"
        with patch.object(sys, "argv", common + ["--local-gitbash", str(self.bundles["gitbash"]),
                                                "--local-native", str(self.bundles["native"])]), \
                patch.object(sys, "stderr", io.StringIO()) as stderr, \
                patch.object(delivery, "publish") as publish, self.assertRaises(SystemExit):
            delivery.main()
        self.assertIn("stage-only", stderr.getvalue())
        publish.assert_not_called()


class PublicationGateTests(unittest.TestCase):
    def publish(self):
        # Gates must reject before touching any of these deliberately absent assets.
        missing = delivery.ROOT / "target/source-delivery-gate-test-unused"
        delivery.publish(delivery.LOCK, missing, missing, "owner/repo", "v1.2.3", "1.2.3", missing, False)

    def test_real_default_policy_blocks_direct_publish_without_any_gh(self):
        with patch.object(delivery.distribution_review, "check_review", wraps=delivery.distribution_review.check_review) as parent, \
                patch.object(delivery.drawing, "approval") as child, \
                patch.object(delivery, "read_lock") as source_lock, \
                patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "Outstanding distribution blockers|not APPROVED"):
            self.publish()
        parent.assert_called_once_with(delivery.ROOT / delivery.distribution_review.POLICY)
        child.assert_not_called()
        source_lock.assert_not_called()
        gh.assert_not_called()

    def test_real_drawing_lock_blocks_when_only_parent_gate_is_mocked(self):
        with patch.object(delivery.distribution_review, "check_review", return_value={"review": {"root_version": "1.2.3"}}), \
                patch.object(delivery.drawing, "approval", wraps=delivery.drawing.approval) as child, \
                patch.object(delivery.distribution_review, "git") as git, \
                patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "Drawing public distribution is not approved"):
            self.publish()
        child.assert_called_once()
        git.assert_not_called()
        gh.assert_not_called()

    def test_mismatched_or_missing_local_tag_and_git_errors_block_before_gh(self):
        for result in ([b"a" * 40, b"b" * 40], [b"", b""],
                       subprocess.CalledProcessError(128, ["git", "rev-parse"]),
                       subprocess.TimeoutExpired(["git", "rev-parse"], 15)):
            with self.subTest(result=result), \
                    patch.object(delivery.distribution_review, "check_review", return_value={"review": {"root_version": "1.2.3"}}), \
                    patch.object(delivery.drawing, "load_lock", return_value={"synthetic": True}), \
                    patch.object(delivery.drawing, "approval"), \
                    patch.object(delivery.distribution_review, "git", side_effect=result) as git, \
                    patch.object(delivery, "read_lock") as source_lock, \
                    patch.object(delivery, "run_gh") as gh:
                with self.assertRaises((ValueError, subprocess.SubprocessError)):
                    self.publish()
                self.assertEqual(git.call_args_list[0].args,
                                 (delivery.ROOT, "rev-parse", "--verify", "refs/tags/v1.2.3^{commit}"))
                if git.call_count == 2:
                    self.assertEqual(git.call_args_list[1].args, (delivery.ROOT, "rev-parse", "--verify", "HEAD"))
                source_lock.assert_not_called()
                gh.assert_not_called()

    def test_release_version_must_match_review_before_git_or_gh(self):
        with patch.object(delivery.distribution_review, "check_review", return_value={"review": {"root_version": "9.9.9"}}), \
                patch.object(delivery.drawing, "load_lock", return_value={"synthetic": True}), \
                patch.object(delivery.drawing, "approval"), \
                patch.object(delivery.distribution_review, "git") as git, \
                patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "reviewed root version"):
            self.publish()
        git.assert_not_called()
        gh.assert_not_called()

    def test_both_gates_and_matching_tag_still_do_not_bypass_null_source_url(self):
        with mock_publication_review("1.2.3"), patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "download_url is null"):
            self.publish()
        gh.assert_not_called()

    def test_gate_order_precedes_source_inputs_and_all_network(self):
        events = []

        def git(root, *args):
            events.append(args[-1])
            return b"a" * 40 + b"\n"

        def source_lock(path):
            events.append("source-lock")
            raise ValueError("stop before source inputs")

        with patch.object(delivery.distribution_review, "check_review", side_effect=lambda path: (events.append("parent") or {"review": {"root_version": "1.2.3"}})), \
                patch.object(delivery.drawing, "load_lock", side_effect=lambda: (events.append("drawing-lock") or {"synthetic": True})), \
                patch.object(delivery.drawing, "approval", side_effect=lambda lock: events.append("drawing-approval")), \
                patch.object(delivery.distribution_review, "git", side_effect=git), \
                patch.object(delivery, "read_lock", side_effect=source_lock), \
                patch.object(delivery, "run_gh") as gh, \
                self.assertRaisesRegex(ValueError, "stop before source inputs"):
            self.publish()
        self.assertEqual(events, ["parent", "drawing-lock", "drawing-approval", "refs/tags/v1.2.3^{commit}", "HEAD", "source-lock"])
        gh.assert_not_called()


class GitbashAdapterTests(unittest.TestCase):
    def test_embedded_metadata_validated_against_current_runtime_not_local_path_or_report_bytes(self):
        with tempfile.TemporaryDirectory(dir=delivery.ROOT / "target") as temporary:
            root = Path(temporary)
            bundle = root / "source.zip"
            state = {"files": [{"path": "bin/git.exe", "size": 3, "sha256": hashlib.sha256(b"git").hexdigest()}]}
            source = {"expected_runtime_files_sha256": delivery.runtime.digest_json(state["files"])}
            archive_data = b"synthetic source archive"
            source_manifest = {"schema": "neo-gitbash-no-gcm-source-v1", "runtime": state,
                               "corresponding_source_complete": True,
                               "packages": [{"name": "pkg" + str(i)} for i in range(57)],
                               "artifacts": [{"id": "toy", "path": "toy.tar", "size": len(archive_data),
                                              "sha256": hashlib.sha256(archive_data).hexdigest()}]}
            with zipfile.ZipFile(bundle, "w") as archive:
                archive.writestr("source-manifest.json", json.dumps(source_manifest))
                archive.writestr("MODIFICATIONS.md", "synthetic modifications")
                archive.writestr("distribution/MANIFEST.json", json.dumps({"runtime": state, "smoke": "historical"}))
                archive.writestr("distribution/gitbash-distribution-policy.json", "{}")
                archive.writestr("archives/toy.tar", archive_data)
            current = {"runtime": state, "smoke": "CI"}
            historical = {"runtime": state, "smoke": "historical"}
            work = root / "isolated"
            work.mkdir()
            before = bundle.read_bytes()
            with patch.object(delivery.runtime, "verify_distribution", side_effect=[current, historical]) as verify, \
                    patch.object(delivery.runtime, "load_policy", return_value={"source_companion": {"exclude_members": []}}), \
                    patch.object(delivery.runtime, "verify_companion", wraps=delivery.runtime.verify_companion) as companion:
                delivery.verify_gitbash(bundle, source, root / "ci-distribution", root / "packaged-runtime", work, 4096)
                self.assertEqual(verify.call_count, 2)
                self.assertEqual(verify.call_args_list[1].args, (work, root / "packaged-runtime"))
                companion.assert_called_once_with(work, current)
            self.assertEqual(delivery.load_json(work / "source-companion-record.json")["sha256"], delivery.file_record(bundle)["sha256"])
            self.assertFalse((work / "validated-source.zip").exists())
            self.assertEqual(bundle.read_bytes(), before)
            work = root / "failed-validation"
            work.mkdir()
            with patch.object(delivery.runtime, "verify_distribution", side_effect=[current, historical]), \
                    patch.object(delivery.runtime, "verify_companion", side_effect=ValueError("companion failed")), \
                    self.assertRaisesRegex(ValueError, "companion failed"):
                delivery.verify_gitbash(bundle, source, root, root, work, 4096)
            self.assertFalse((work / "validated-source.zip").exists())
            self.assertEqual(bundle.read_bytes(), before)
            with patch.object(delivery.runtime, "verify_distribution", side_effect=[current, historical]), \
                    patch.object(Path, "hardlink_to", side_effect=OSError("hard links unavailable")), \
                    patch.object(delivery.runtime, "verify_companion") as companion, \
                    patch.object(delivery.shutil, "copyfileobj") as copy_file, \
                    self.assertRaisesRegex(OSError, "hard links unavailable"):
                delivery.verify_gitbash(bundle, source, root, root, work, 4096)
            companion.assert_not_called()
            copy_file.assert_not_called()
            self.assertEqual(bundle.read_bytes(), before)

    def test_zip_paths_duplicates_links_expansion_and_runtime_mismatch(self):
        cases = [("../escape", b"x", None), ("same", b"x", "duplicate"),
                 ("link", b"x", "link"), ("large", b"x" * 100, "budget")]
        for name, data, variant in cases:
            with self.subTest(variant=variant), tempfile.TemporaryDirectory(dir=delivery.ROOT / "target") as temporary:
                root = Path(temporary)
                bundle = root / "source.zip"
                with zipfile.ZipFile(bundle, "w") as archive:
                    info = zipfile.ZipInfo(name)
                    if variant == "link":
                        info.external_attr = 0o120777 << 16
                    archive.writestr(info, data)
                    if variant == "duplicate":
                        archive.writestr("SAME", data)
                current = {"runtime": {"files": []}}
                source = {"expected_runtime_files_sha256": delivery.runtime.digest_json([])}
                with patch.object(delivery.runtime, "verify_distribution", return_value=current), self.assertRaises(ValueError):
                    delivery.verify_gitbash(bundle, source, root, root, root, 10)
        with patch.object(delivery.runtime, "verify_distribution", return_value={"runtime": {"files": []}}), \
                self.assertRaisesRegex(ValueError, "runtime file set mismatch"):
            delivery.verify_gitbash(Path("unused"), {"expected_runtime_files_sha256": "0" * 64}, Path("unused"), Path("unused"), Path("unused"), 10)


class DownloadTests(unittest.TestCase):
    def test_download_hash_length_overrun_and_cleanup(self):
        for payload, length, expected_sha, good in ((b"abc", "3", hashlib.sha256(b"abc").hexdigest(), True),
                                                   (b"abcd", None, "0" * 64, False),
                                                   (b"abc", "4", "0" * 64, False),
                                                   (b"ab", None, "0" * 64, False),
                                                   (b"abc", "3", "0" * 64, False)):
            with self.subTest(payload=payload, length=length, good=good), tempfile.TemporaryDirectory(dir=delivery.ROOT / "target") as temporary:
                destination = Path(temporary) / "source.zip"
                response = io.BytesIO(payload)
                response.status = 200
                response.headers = {} if length is None else {"Content-Length": length}
                response.geturl = lambda: "https://sources.example.test/source.zip"
                opener = Mock()
                opener.open.return_value = response
                source = {"download_url": "https://sources.example.test/source.zip", "sha256": expected_sha, "size": 3}
                with patch.object(delivery.urllib.request, "build_opener", return_value=opener), patch.object(delivery.time, "monotonic", return_value=1):
                    if good:
                        delivery.download(source, destination, 10)
                        self.assertEqual(destination.read_bytes(), payload)
                    else:
                        with self.assertRaises(ValueError):
                            delivery.download(source, destination, 10)
                        self.assertFalse(destination.exists())

    def test_existing_download_is_preserved(self):
        with tempfile.TemporaryDirectory(dir=delivery.ROOT / "target") as temporary:
            destination = Path(temporary) / "keep.zip"
            destination.write_bytes(b"keep")
            with patch.object(delivery.urllib.request, "build_opener") as opener, \
                    patch.object(delivery.time, "monotonic", return_value=1), \
                    self.assertRaisesRegex(ValueError, "overwrite"):
                delivery.download({"download_url": "https://sources.example.test/a"}, destination, 10)
            opener.return_value.open.assert_not_called()
            self.assertEqual(destination.read_bytes(), b"keep")

    def test_timeout_and_insecure_redirect_rejected(self):
        with patch.object(delivery.time, "monotonic", return_value=11), \
                patch.object(delivery.urllib.request, "build_opener") as opener, \
                self.assertRaisesRegex(ValueError, "time budget"):
            delivery.download({"download_url": "https://sources.example.test/a"}, Path("unused"), 10)
        opener.return_value.open.assert_not_called()
        with self.assertRaises(ValueError):
            delivery.HTTPSRedirects().redirect_request(None, None, 302, "", {}, "http://sources.example.test/a")


if __name__ == "__main__":
    unittest.main()
