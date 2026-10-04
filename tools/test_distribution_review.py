"""Offline synthetic approval records; no legal approval or native execution."""
from contextlib import redirect_stdout, redirect_stderr
from copy import deepcopy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from tools import check_distribution_review as review


class DistributionReviewTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="neo-review-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        (self.root / "tools").mkdir()
        self.policy = self.root / review.POLICY
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_bytes(b"synthetic lock\n")
        (self.root / "evidence.txt").write_bytes(b"Synthetic evidence, not a real legal determination.\n")
        self.record = {
            "schema": 1, "status": "APPROVED", "blockers": [],
            "review": {"reviewer": "Synthetic test reviewer", "date": "2026-01-01",
                       "commit": "a" * 40, "source_tree_sha256": "b" * 64,
                       "root_version": "1.2.3", "cargo_lock_sha256": review.sha256(b"synthetic lock\n")},
            "resolutions": {issue: {"summary": "Synthetic resolution only", "evidence": [
                {"path": "evidence.txt", "sha256": review.sha256((self.root / "evidence.txt").read_bytes())}
            ]} for issue in review.REQUIRED_ISSUES},
        }
        self.git = self.enterContext(patch.object(review, "git", side_effect=self.git_result))
        self.tree = self.enterContext(patch.object(review, "source_tree", return_value="b" * 64))

    def git_result(self, root, *args):
        if args == ("show", f"HEAD:{review.POLICY}"):
            return self.policy.read_bytes()
        return b""

    def check(self, record=None):
        self.policy.write_text(json.dumps(self.record if record is None else record), encoding="utf-8")
        return review.check_review(self.policy, self.root)

    def test_complete_record_only_verifies_consistency(self):
        self.assertEqual(self.check(), self.record)
        self.tree.assert_any_call(self.root, "a" * 40)
        self.tree.assert_any_call(self.root, "HEAD")
        self.git.assert_any_call(self.root, "merge-base", "--is-ancestor", "a" * 40, "HEAD")
        self.git.assert_any_call(self.root, "diff", "--exit-code", "HEAD", "--")
        self.git.assert_any_call(self.root, "ls-files", "--error-unmatch", "--", "evidence.txt")

    def test_actual_policy_is_blocked_and_has_no_invented_reviewer(self):
        path = review.ROOT / review.POLICY
        record = json.loads(path.read_text(encoding="utf-8"))
        self.assertEqual(record["status"], "BLOCKED")
        self.assertIsNone(record["review"])
        self.assertEqual(record["resolutions"], {})
        self.assertEqual({b["issue"] for b in record["blockers"]}, review.REQUIRED_ISSUES)
        with self.assertRaisesRegex(ValueError, "Outstanding distribution blockers"):
            review.check_review(path)
        self.git.assert_not_called()

    def test_remaining_blocker_overrides_approved(self):
        self.record["blockers"] = [{"issue": "new-issue", "outstanding": "Needs review"}]
        with self.assertRaisesRegex(ValueError, "new-issue.*Needs review"):
            self.check()
        self.git.assert_not_called()

    def test_removing_blockers_and_changing_status_is_not_approval(self):
        self.record["review"] = None
        with self.assertRaisesRegex(ValueError, "Invalid review fields"):
            self.check()

    def test_bad_schema_status_and_boolean_approvals_fail(self):
        for key, value in (("schema", True), ("schema", 2), ("status", True),
                           ("status", "approved"), ("status", "BLOCKED"),
                           ("blockers", False), ("review", True), ("resolutions", True)):
            with self.subTest(key=key, value=value):
                record = deepcopy(self.record)
                record[key] = value
                with self.assertRaises(ValueError):
                    self.check(record)
        self.record["force"] = True
        with self.assertRaises(ValueError):
            self.check()

    def test_review_identity_date_commit_and_pins_are_required(self):
        invalid = {"reviewer": [None, True, "", "  "], "date": [True, "2026-02-30", "9999-01-01", "20260101"],
                   "commit": [True, "HEAD", "a" * 39], "source_tree_sha256": [True, "no", "c" * 64],
                   "root_version": [True, "9.9.9"], "cargo_lock_sha256": [True, "c" * 64]}
        for key, values in invalid.items():
            for value in values:
                with self.subTest(key=key, value=value):
                    record = deepcopy(self.record)
                    record["review"][key] = value
                    with self.assertRaises(ValueError):
                        self.check(record)
        for key in self.record["review"]:
            record = deepcopy(self.record)
            del record["review"][key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                self.check(record)

    def test_all_issue_resolutions_require_summary_and_hashed_evidence(self):
        for issue in review.REQUIRED_ISSUES:
            for value in (None, True, {}, {"summary": "done", "evidence": []},
                          {"summary": "", "evidence": [{"path": "evidence.txt", "sha256": "c" * 64}]},
                          {"summary": "done", "evidence": [{"path": "evidence.txt", "sha256": "c" * 64}]}):
                record = deepcopy(self.record)
                if value is None:
                    del record["resolutions"][issue]
                else:
                    record["resolutions"][issue] = value
                with self.subTest(issue=issue, value=value), self.assertRaises(ValueError):
                    self.check(record)

    def test_missing_empty_directory_and_escaping_evidence_fail(self):
        (self.root / "empty.txt").touch()
        for name in ("../outside.txt", "/absolute.txt", "C:/evidence.txt", "tools/../evidence.txt",
                     "./evidence.txt", "tools\\evidence.txt", "https://example.invalid/evidence", "missing.txt",
                     "empty.txt", "tools", review.POLICY):
            record = deepcopy(self.record)
            record["resolutions"]["sherpa-native"]["evidence"][0]["path"] = name
            with self.subTest(path=name), self.assertRaises((ValueError, OSError)):
                self.check(record)

    def test_changed_inputs_fail(self):
        for name in ("Cargo.lock", "evidence.txt"):
            path = self.root / name
            old = path.read_bytes()
            path.write_bytes(b"changed\n")
            with self.subTest(path=name), self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                self.check()
            path.write_bytes(old)

    def test_changed_source_tree_and_missing_or_unrelated_commit_fail(self):
        self.tree.side_effect = ["b" * 64, "c" * 64]
        with self.assertRaisesRegex(ValueError, "Checkout differs"):
            self.check()
        self.tree.side_effect = None
        self.git.side_effect = subprocess.CalledProcessError(1, "git", stderr=b"missing/untracked/dirty")
        with self.assertRaises(subprocess.CalledProcessError):
            self.check()

    def test_committed_policy_must_match(self):
        self.git.side_effect = lambda root, *args: b"different" if args[0] == "show" else b""
        with self.assertRaisesRegex(ValueError, "Policy differs"):
            self.check()

    def test_source_tree_excludes_only_policy(self):

        entries = [b"100644 blob aaa\tCargo.lock", b"100644 blob bbb\ttools/distribution-review.json",
                   b"100644 blob ccc\tevidence.txt"]
        with patch.object(review, "git", return_value=b"\0".join(entries) + b"\0"):
            # Invoke the implementation despite the per-test source_tree mock.
            result = SOURCE_TREE(self.root, "HEAD")
        self.assertEqual(result, review.sha256(b"\0".join([entries[0], entries[2]])))

    def test_duplicate_keys_and_missing_policy_fail(self):
        self.policy.write_text('{"schema": 1, "schema": 1}', encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "Duplicate JSON key"):
            review.check_review(self.policy, self.root)
        self.policy.unlink()
        with self.assertRaises(OSError):
            review.check_review(self.policy, self.root)

    def test_cli_failure_is_clear_and_success_is_not_legal_judgment(self):
        output = io.StringIO()
        with patch.object(review, "check_review", side_effect=ValueError("outstanding")), redirect_stderr(output):
            self.assertEqual(review.main(["--policy", str(self.policy)]), 1)
        self.assertIn("Distribution release BLOCKED", output.getvalue())
        output = io.StringIO()
        with patch.object(review, "check_review", return_value=self.record), redirect_stdout(output):
            self.assertEqual(review.main(["--policy", str(self.policy)]), 0)
        self.assertIn("NOT an automated legal/compliance judgment", output.getvalue())
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            review.main(["--policy", str(self.policy), "--force"])
        self.assertNotEqual(error.exception.code, 0)


SOURCE_TREE = review.source_tree

if __name__ == "__main__":
    unittest.main()
