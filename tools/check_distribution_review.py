"""Fail-closed distribution approval record checks (Python 3.11+, offline).

This verifies record/input consistency, NOT legal compliance or reviewer authority.
Never builds, downloads, runs payloads, or provides a force/skip switch.
"""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from datetime import date, datetime, timezone
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
POLICY = "tools/distribution-review.json"
REQUIRED_ISSUES = frozenset({"sherpa-native", "gitbash-source", "model-rights", "msvc-authorization"})


def require(condition, message):
    if not condition:
        raise ValueError(message)


def fields(value, names, label):
    require(isinstance(value, dict) and set(value) == set(names), f"Invalid {label} fields")


def text(value, label):
    require(isinstance(value, str) and bool(value.strip()), f"Missing {label}")


def digest(value, label):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), f"Invalid {label} SHA-256")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def relative_file(root, name):
    text(name, "relative evidence path")
    parts = PurePosixPath(name)
    require(not parts.is_absolute() and parts.as_posix() == name
            and not any(p in {".", ".."} for p in parts.parts)
            and "\\" not in name and ":" not in name,
            f"Expected repository-relative POSIX path: {name}")
    path = root / name
    require(path.resolve().is_relative_to(root.resolve()), f"Path escapes repository: {name}")
    for part in (path, *path.parents):
        if part == root:
            break
        stat = part.lstat()
        require(not part.is_symlink() and not getattr(stat, "st_file_attributes", 0) & 0x400,
                f"Symlink/reparse evidence is not accepted: {name}")
    require(path.is_file() and path.stat().st_size > 0, f"Missing/empty evidence file: {name}")
    return path


def git(root, *args):
    return subprocess.run(["git", "--no-pager", "--no-optional-locks", *args], cwd=root,
                          capture_output=True, check=True, timeout=15).stdout


def source_tree(root, commit):
    # Exclude only the approval record, avoiding a self-referential commit/hash.
    # All other tracked inputs, including evidence, must match the reviewed commit.
    entries = git(root, "ls-tree", "-rz", "--full-tree", commit).split(b"\0")
    return sha256(b"\0".join(entry for entry in entries
                            if entry and entry.split(b"\t", 1)[1] != POLICY.encode()))


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def check_review(policy, root=ROOT):
    root = Path(root).resolve()
    policy = Path(policy).resolve()
    require(policy == root / POLICY, f"Policy must be the tracked {POLICY}")
    record = json.loads(relative_file(root, POLICY).read_text(encoding="utf-8"),
                        object_pairs_hook=unique_object)
    fields(record, {"schema", "status", "blockers", "review", "resolutions"}, "policy")
    require(type(record["schema"]) is int and record["schema"] == 1, "Unsupported policy schema")
    require(record["status"] in ("BLOCKED", "APPROVED"), "Unknown review status")
    require(isinstance(record["blockers"], list), "blockers must be a list")
    for blocker in record["blockers"]:
        fields(blocker, {"issue", "outstanding"}, "blocker")
        text(blocker["issue"], "blocker issue")
        text(blocker["outstanding"], "blocker outstanding work")
    if record["blockers"]:
        raise ValueError("Outstanding distribution blockers:\n" + "\n".join(
            f"- {item['issue']}: {item['outstanding']}" for item in record["blockers"]))
    require(record["status"] == "APPROVED", "Distribution review status is not APPROVED")
    review = record["review"]
    fields(review, {"reviewer", "date", "commit", "source_tree_sha256", "root_version",
                    "cargo_lock_sha256"}, "review")
    text(review["reviewer"], "reviewer")
    require(isinstance(review["date"], str) and re.fullmatch(r"\d{4}-\d{2}-\d{2}", review["date"]),
            "Review date must be YYYY-MM-DD")
    require(date.fromisoformat(review["date"]) <= datetime.now(timezone.utc).date(), "Review date is in the future")
    require(isinstance(review["commit"], str) and re.fullmatch(r"[0-9a-f]{40}", review["commit"]),
            "Review commit must be a full Git SHA-1")
    for key in ("source_tree_sha256", "cargo_lock_sha256"):
        digest(review[key], key)
    version = tomllib.loads(relative_file(root, "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    require(isinstance(version, str) and review["root_version"] == version, "Root Cargo version mismatch")
    require(review["cargo_lock_sha256"] == sha256(relative_file(root, "Cargo.lock").read_bytes()),
            "Cargo.lock SHA-256 mismatch")
    resolutions = record["resolutions"]
    fields(resolutions, REQUIRED_ISSUES, "required issue resolutions")
    for issue, resolution in resolutions.items():
        fields(resolution, {"summary", "evidence"}, f"{issue} resolution")
        text(resolution["summary"], f"{issue} resolution summary")
        require(isinstance(resolution["evidence"], list) and resolution["evidence"],
                f"Missing evidence for {issue}")
        seen = set()
        for evidence in resolution["evidence"]:
            fields(evidence, {"path", "sha256"}, f"{issue} evidence")
            path = relative_file(root, evidence["path"])
            require(evidence["path"] != POLICY and evidence["path"] not in seen,
                    f"Duplicate/self-referential evidence for {issue}")
            seen.add(evidence["path"])
            digest(evidence["sha256"], f"{issue} evidence")
            require(sha256(path.read_bytes()) == evidence["sha256"], f"Evidence SHA-256 mismatch: {path}")
            git(root, "ls-files", "--error-unmatch", "--", evidence["path"])
    git(root, "ls-files", "--error-unmatch", "--", POLICY, "Cargo.toml", "Cargo.lock")
    git(root, "diff", "--exit-code", "HEAD", "--")
    # Also catches assume-unchanged/skip-worktree policy edits, not just ordinary dirtiness.
    require(git(root, "show", f"HEAD:{POLICY}").replace(b"\r\n", b"\n")
            == policy.read_bytes().replace(b"\r\n", b"\n"), "Policy differs from committed record")
    git(root, "merge-base", "--is-ancestor", review["commit"], "HEAD")
    expected = review["source_tree_sha256"]
    require(source_tree(root, review["commit"]) == expected, "Reviewed source tree SHA-256 mismatch")
    require(source_tree(root, "HEAD") == expected, "Checkout differs from reviewed source tree; re-review required")
    return record


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--policy", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        check_review(args.policy)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        detail = error.stderr.decode("utf-8", "replace").strip() if isinstance(error, subprocess.CalledProcessError) else str(error)
        print(f"Distribution release BLOCKED: {detail or str(error)}", file=sys.stderr)
        print("No build/download/package/upload is authorized. Resolve outstanding issues and obtain a reviewed, hash-bound approval record.", file=sys.stderr)
        return 1
    print("Distribution approval record and input hashes verified; NOT an automated legal/compliance judgment.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
