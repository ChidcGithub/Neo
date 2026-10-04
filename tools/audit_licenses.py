"""Offline Cargo license evidence inventory (Python 3.11+, no extra dependencies).

Never builds crates, executes build.rs, fetches the network, changes Cargo.lock,
chooses a license alternative, or issues a legal compliance verdict.
Inventories go to docs-pri/licenses; public license texts go to docs/licenses.
"""
from __future__ import annotations

import argparse
from collections import Counter, deque
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib

TARGET = "x86_64-pc-windows-msvc"
BASIC = {"MIT", "Apache-2.0", "MIT OR Apache-2.0", "Apache-2.0 OR MIT",
         "MIT/Apache-2.0", "Apache-2.0/MIT", "MIT / Apache-2.0", "Apache-2.0 / MIT"}
LICENSE_NAME = re.compile(r"^(licen[sc]e|copying|copyright|notice|unlicense|ofl|ufl)(?:[._-].*|$)", re.I)
TREE_LINE = re.compile(r"^\d+([^ ]+) v([^ ]+)(?: |$)")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def key(package):
    return package["name"], package["version"], package.get("source")


def read_json(path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def command(root, work, name, args, timeout):
    """Save even failed command evidence without flooding stdout."""
    stdout, stderr = work / (name + ".stdout"), work / (name + ".stderr")
    result = {"command": args, "stdout": stdout.relative_to(root).as_posix(),
              "stderr": stderr.relative_to(root).as_posix()}
    with stdout.open("wb") as out, stderr.open("wb") as err:
        try:
            completed = subprocess.run(args, cwd=root, stdout=out, stderr=err,
                                       timeout=timeout, check=False)
            result["returncode"] = completed.returncode
        except (OSError, subprocess.TimeoutExpired) as exc:
            result["returncode"] = None
            result["error"] = str(exc)
    result["stdout_sha256"] = digest(stdout.read_bytes())
    result["stderr_sha256"] = digest(stderr.read_bytes())
    if result["returncode"] != 0:
        result["error"] = result.get("error", stderr.read_text(encoding="utf-8", errors="replace").strip())
    return result


def capture(root, work, target, timeout):
    base = ["cargo", "metadata", "--locked", "--offline", "--format-version", "1"]
    commands = {}
    for name, args in (
        ("windows-metadata", base + ["--filter-platform", target]),
        ("all-targets-metadata", base),
        ("windows-normal-build", ["cargo", "tree", "--locked", "--offline", "--workspace",
                                  "--target", target, "--edges", "normal,build", "--prefix", "depth", "--format", "{p}"]),
        ("windows-runtime", ["cargo", "tree", "--locked", "--offline", "--workspace",
                             "--target", target, "--edges", "normal,no-proc-macro", "--prefix", "depth", "--format", "{p}"]),
        ("windows-with-dev", ["cargo", "tree", "--locked", "--offline", "--workspace",
                              "--target", target, "--edges", "normal,build,dev", "--prefix", "depth", "--format", "{p}"]),
        ("rustc-version", ["rustc", "-vV"]),
    ):
        commands[name] = command(root, work, name, args, timeout)
    return commands


def tree_members(text, packages):
    """Do not silently collapse two sources with the same name and version."""
    by_pair = {}
    for package in packages:
        by_pair.setdefault((package["name"], package["version"]), []).append(key(package))
    found, errors = set(), []
    for line in text.splitlines():
        match = TREE_LINE.match(line)
        if not match:
            if line.strip():
                errors.append("Unrecognized tree line: " + line)
            continue
        choices = by_pair.get(match.groups(), [])
        if len(choices) == 1:
            found.add(choices[0])
        else:
            errors.append("Ambiguous or missing lock identity: " + line)
    return found, errors


def build_candidates(metadata, allowed):
    """Host/build roles from filtered metadata, bounded by the actual NB tree.

    Metadata merges feature contexts: these roles are conservative, not a Cargo
    unit graph. The separate tree-derived build-only set does not rely on this.
    """
    if not metadata or not metadata.get("resolve") or allowed is None:
        return None
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    queue = deque((p, False) for p in metadata["workspace_members"])
    seen, result = set(), set()
    while queue:
        pid, host = queue.popleft()
        if (pid, host) in seen or pid not in packages:
            continue
        seen.add((pid, host))
        package = packages[pid]
        if key(package) not in allowed:
            continue
        if host:
            result.add(key(package))
        for dep in nodes.get(pid, {}).get("deps", []):
            target = packages.get(dep["pkg"])
            if not target:
                continue
            proc_macro = any("proc-macro" in t.get("kind", []) for t in target.get("targets", []))
            for kind in dep.get("dep_kinds", []):
                if kind["kind"] != "dev":
                    queue.append((dep["pkg"], host or kind["kind"] == "build" or proc_macro))
    return result


def license_candidate(name):
    path = PurePosixPath(name)
    return bool(LICENSE_NAME.match(path.name)) or bool(re.search(
        r"(?:^|[._-])(?:licen[sc]e|copying|copyright|notice)(?:[._-]|$)", path.name, re.I)) or any(
        part.lower() in {"licenses", "licences"} for part in path.parts[:-1])


class Source:
    def __init__(self, label, directory=None, archive=None):
        self.label, self.directory, self.archive = label, directory, archive
        self.files = None
        self.archive_sha256 = None
        if archive:
            raw = archive.read_bytes()
            self.archive_sha256 = digest(raw)
            self.files = {}
            with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as tar:
                for member in tar.getmembers():
                    path = PurePosixPath(member.name)
                    if member.isfile() and len(path.parts) > 1 and ".." not in path.parts:
                        name = PurePosixPath(*path.parts[1:]).as_posix()
                        # Keep all files addressable for arbitrary package.license-file.
                        self.files[name] = tar.extractfile(member).read()

    def read(self, name):
        if self.files is not None:
            return self.files.get(name)
        path = (self.directory / name).resolve()
        if not path.is_relative_to(self.directory.resolve()) or not path.is_file():
            return None
        return path.read_bytes()

    def license_names(self):
        if self.files is not None:
            names = self.files.keys()
        else:
            names = (p.relative_to(self.directory).as_posix()
                     for p in self.directory.rglob("*") if p.is_file() and not p.is_symlink())
        found = []
        for name in names:
            if license_candidate(name):
                found.append(name)
            elif PurePosixPath(name).suffix.lower() in {".txt", ".md"}:
                # Font packages sometimes ship full licenses under the font's name.
                raw = self.read(name)
                if raw and re.search(rb"permission is hereby granted|redistribution and use in source|SIL OPEN FONT LICENSE|BITSTREAM VERA LICENSE", raw, re.I):
                    found.append(name)
        return sorted(found)


def portable(path, root, cargo_home):
    path = path.resolve()
    if path.is_relative_to(root):
        return "project/" + path.relative_to(root).as_posix()
    if path.is_relative_to(cargo_home):
        return "CARGO_HOME/" + path.relative_to(cargo_home).as_posix()
    return path.as_posix()


def locate(package, metadata_package, root, cargo_home):
    issues = []
    if metadata_package:
        manifest = Path(metadata_package["manifest_path"])
        if manifest.is_file():
            return Source(portable(manifest.parent, root, cargo_home), directory=manifest.parent), issues
    stem = package["name"] + "-" + package["version"]
    if (package.get("source") or "").startswith("registry+") and package.get("checksum"):
        # The lock checksum identifies the archive even when a registry mirror is used.
        for archive in sorted((cargo_home / "registry" / "cache").glob("*/" + stem + ".crate")):
            if digest(archive.read_bytes()) == package["checksum"]:
                return Source(portable(archive, root, cargo_home), archive=archive), issues
            issues.append("Archive checksum mismatch: " + portable(archive, root, cargo_home))
        for manifest in sorted((cargo_home / "registry" / "src").glob("*/" + stem + "/Cargo.toml")):
            checksum = read_json(manifest.parent / ".cargo-checksum.json")
            if checksum and checksum.get("package") == package["checksum"]:
                return Source(portable(manifest.parent, root, cargo_home), directory=manifest.parent), issues
    if not package.get("source"):
        # Workspace/path dependencies can still be inspected when Cargo resolution fails.
        for directory in (root / "crates", root / "vendor"):
            for manifest in sorted(directory.glob("*/Cargo.toml")):
                data = tomllib.loads(manifest.read_text(encoding="utf-8"))
                pkg = data.get("package", {})
                version = pkg.get("version")
                if isinstance(version, dict) and version.get("workspace"):
                    version = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8")).get("workspace", {}).get("package", {}).get("version")
                if (pkg.get("name"), version) == (package["name"], package["version"]):
                    return Source(portable(manifest.parent, root, cargo_home), directory=manifest.parent), issues
    return None, issues


def review_flags(expression, license_file):
    flags = []
    if not expression:
        flags.append("license-file-only" if license_file else "license-unspecified-not-proof-of-unlicensed")
    elif expression not in BASIC:
        flags.append("non-basic-expression")
    for pattern, label in ((r"\bMPL-", "MPL"), (r"\bLGPL-", "LGPL-alternative-review"),
                           (r"\bGPL-", "GPL-alternative-review"), (r"\bBSL-", "BSL"),
                           (r"Unicode", "Unicode"), (r"OpenSSL", "OpenSSL"),
                           (r"\bAND\b", "cumulative-AND"), (r"\bWITH\b", "exception-WITH"),
                           (r"OFL-|Ubuntu-font", "fonts"), (r"CDLA-", "certificate-data")):
        if re.search(pattern, expression or "", re.I):
            flags.append(label)
    return flags


def inspect_package(package, meta, root, cargo_home, workspace_ids):
    source, issues = locate(package, meta, root, cargo_home)
    entry = {"name": package["name"], "version": package["version"], "source": package.get("source"),
             "lock_checksum": package.get("checksum"), "workspace_member": bool(meta and meta["id"] in workspace_ids),
             "metadata_available": meta is not None, "missing_cache": source is None,
             "license": None, "license_file": None, "evidence": [], "issues": issues}
    texts = []
    if not source:
        entry["issues"].append("No attributable local manifest/archive; license unknown")
        entry["review_flags"] = ["missing-cache", "license-unknown"]
        return entry, texts
    entry["evidence_origin"] = source.label
    entry["archive_sha256"] = source.archive_sha256
    manifest_bytes = source.read("Cargo.toml")
    data = tomllib.loads(manifest_bytes.decode("utf-8"))
    pkg = data.get("package", {})
    if pkg.get("name") != package["name"]:
        raise ValueError("Manifest identity mismatch: " + source.label)
    inherited = []
    for field in ("license", "license-file", "version"):
        value = pkg.get(field)
        if isinstance(value, dict) and value.get("workspace"):
            inherited.append(field)
            value = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8")).get("workspace", {}).get("package", {}).get(field)
        if field == "version":
            if value != package["version"]:
                raise ValueError("Manifest version mismatch: " + source.label)
        else:
            entry[field.replace("-", "_")] = value
    entry["evidence"].append({"path": "Cargo.toml", "kind": "manifest", "sha256": digest(manifest_bytes), "bytes": len(manifest_bytes)})
    if inherited:
        raw = (root / "Cargo.toml").read_bytes()
        entry["evidence"].append({"path": "project/Cargo.toml", "kind": "workspace-inheritance",
                                  "fields": inherited, "sha256": digest(raw), "bytes": len(raw)})
    names = set(source.license_names())
    declared = entry["license_file"]
    if declared:
        names.add(PurePosixPath(declared.replace("\\", "/")).as_posix())
    for name in sorted(names):
        raw = source.read(name)
        if raw is None:
            issues.append("License/notice file unavailable or outside package: " + name)
            continue
        item = {"path": name, "kind": "license-or-notice", "sha256": digest(raw), "bytes": len(raw)}
        entry["evidence"].append(item)
        texts.append((item, raw))
    # Workspace crates commonly inherit the license expression and use root LICENSE.
    if entry["workspace_member"] and (root / "LICENSE").is_file():
        raw = (root / "LICENSE").read_bytes()
        item = {"path": "project/LICENSE", "kind": "workspace-license", "sha256": digest(raw), "bytes": len(raw)}
        entry["evidence"].append(item)
        texts.append((item, raw))
    entry["license_text_count"] = len(texts)
    if not texts:
        issues.append("No conventionally named or declared local license/notice file found")
    entry["review_flags"] = review_flags(entry["license"], declared)
    if any(b"openssl" in raw.lower() for _, raw in texts):
        entry["review_flags"].append("OpenSSL-mentioned-in-text-review")
    if any(b"bitstream vera license" in raw.lower() for _, raw in texts):
        entry["review_flags"].append("Bitstream-Vera-in-text-review")
    return entry, texts


def inventory(root, cargo_home, metadata=None, windows_metadata=None, trees=None):
    lock_bytes = (root / "Cargo.lock").read_bytes()
    packages = tomllib.loads(lock_bytes.decode("utf-8"))["package"]
    by_key, workspace_ids = {}, set()
    for doc in (windows_metadata, metadata):
        if doc:
            by_key.update((key(p), p) for p in doc["packages"])
            workspace_ids.update(doc.get("workspace_members", []))
    sets, graph_issues = {}, []
    for name in ("windows-normal-build", "windows-runtime", "windows-with-dev"):
        text = (trees or {}).get(name)
        members, errors = tree_members(text, packages) if text is not None else (None, [])
        sets[name] = None if errors else members
        graph_issues.extend(errors)
    nb, runtime, dev = (sets[n] for n in ("windows-normal-build", "windows-runtime", "windows-with-dev"))
    host = build_candidates(windows_metadata, nb)
    entries, collected = [], []
    for package in sorted(packages, key=lambda p: (p["name"], p["version"], p.get("source") or "")):
        entry, texts = inspect_package(package, by_key.get(key(package)), root, cargo_home, workspace_ids)
        identity = key(package)
        entry["scopes"] = {
            "all_lock": True,
            "windows_normal_build": None if nb is None else identity in nb,
            "windows_runtime_candidate": None if runtime is None else identity in runtime,
            "windows_build_only": None if nb is None or runtime is None else identity in nb - runtime,
            "windows_build_candidate": None if host is None else identity in host,
            "windows_dev_only": None if nb is None or dev is None else identity in dev - nb,
            "outside_windows_default_tree": None if dev is None else identity not in dev,
        }
        entries.append(entry)
        for evidence, raw in texts:
            collected.append((entry, evidence, raw))
    counts = {"all_lock": len(entries), "workspace_members": sum(p["workspace_member"] for p in entries),
              "third_party": sum(not p["workspace_member"] for p in entries),
              "metadata_missing": sum(not p["metadata_available"] for p in entries),
              "missing_cache": sum(p["missing_cache"] for p in entries),
              "license_unspecified": sum(not p["license"] for p in entries),
              "no_license_text": sum(not p.get("license_text_count") for p in entries),
              "windows_normal_build_no_license_text": None if nb is None else sum(
                  p["scopes"]["windows_normal_build"] and not p.get("license_text_count") for p in entries),
              "license_notice_files": len(collected),
              "packages_with_review_flags": sum(bool(p["review_flags"]) for p in entries)}
    for scope in entries[0]["scopes"] if entries else []:
        counts[scope] = None if any(p["scopes"][scope] is None for p in entries) else sum(p["scopes"][scope] for p in entries)
    return {"schema_version": 1, "lock_sha256": digest(lock_bytes), "counts": counts,
            "graph_issues": graph_issues, "license_expression_counts": dict(sorted(Counter(p["license"] or "UNKNOWN" for p in entries).items())),
            "packages": entries}, collected


SCOPE = """This is a Cargo.lock/local-cache evidence inventory, not legal approval or a binary SBOM.
All-lock includes every locked package, including inactive platforms and optional features.
Windows commands use --workspace, default features, and the recorded target; no --all-features.
windows_normal_build is cargo tree --edges normal,build; it includes proc macros/host tools.
windows_runtime_candidate is cargo tree --edges normal,no-proc-macro: normal dependency
reachability only, NOT proof of linkage, execution, inclusion in the final executable or shipment.
windows_build_only is normal/build minus runtime candidates. Shared runtime/build packages
can occur: windows_build_candidate additionally follows build/proc-macro edges through
filtered metadata, bounded by the normal/build tree (conservative merged-feature roles).
windows_dev_only is the normal/build/dev tree minus normal/build, NOT every shared dev role.
Unknown graph membership is null, never false. outside_windows_default_tree is not a claim
that a crate cannot compile on Windows with different features or targets.
No builds or build.rs were executed. cargo metadata/tree are not an exact Cargo unit graph.
Manifest license expressions are preserved verbatim: AND is cumulative; OR permits an
alternative; WITH qualifies a license. No license choice or compatibility verdict is made.
BSD/ISC/Zlib/BSL/Unicode, copyleft alternatives, fonts, certificate data, AND and WITH need
human review. BSL-1.0 here is Boost Software License, not Business Source License.
MPL-2.0 has file-level source/notice obligations where applicable; Apache-2.0 for Neo does
not relicense dependency code. GPL/LGPL in an OR expression is not mandatory GPL/LGPL.
Unlicense is a license identifier; an absent license field is unknown, not proof of unlicensed code.
License/notice collection recursively matches LICENSE/LICENCE/COPYING/COPYRIGHT/NOTICE/
UNLICENSE/OFL/UFL names, delimited license/notice filename tokens, contents of licenses/
licences directories, explicit license-file, and .txt/.md files containing recognizable
permission/redistribution/SIL/Bitstream license wording (may include README excerpts).
It preserves local file bytes, including alternative licenses and nested vendor notices,
without deciding which files apply. Text mentions (such as OpenSSL in the Apache license
appendix) are review hints, NOT proof the named library or license is included.
SHA-256 hashes describe the inspected local evidence;
metadata-attributed unpacked sources are not independently authenticated against crates.io.
Archive fallback is accepted only when its SHA-256 matches Cargo.lock. Missing cache/files
are reported rather than fetched. Nonconventional in-source headers may not be collected.
This is NOT an exhaustive native-library/SDK/binary/runtime, bundled font/model/asset,
Git Bash, redistributable, or copied-source audit. Nested native notices found inside crates
are retained, but do not establish native configuration or distribution coverage.
"""


def scope_label(entry):
    scopes = entry["scopes"]
    if scopes["windows_normal_build"] is None:
        return "Windows unknown"
    labels = []
    if scopes["windows_runtime_candidate"]:
        labels.append("Win runtime candidate")
    if scopes["windows_build_only"]:
        labels.append("Win build-only")
    elif scopes["windows_build_candidate"]:
        labels.append("also build candidate")
    if scopes["windows_dev_only"]:
        labels.append("Win dev-only")
    if scopes["outside_windows_default_tree"]:
        labels.append("outside Win default tree")
    return "; ".join(labels) or "Win scope incomplete"


def markdown(report):
    lines = ["# Cargo license evidence inventory", "", "Generated by `python tools/audit_licenses.py` (offline; Python 3.11+).", "",
             "## Scope and limitations", "", SCOPE.strip(), "", "## Counts", "", "| Measure | Count |", "| --- | ---: |"]
    lines.extend(f"| {name} | {value if value is not None else 'unknown'} |" for name, value in report["counts"].items())
    lines += ["", "Counts include workspace packages unless explicitly stated; scope columns can overlap.", "",
              "## Command evidence", "", f"Target: `{report['target']}`. Lock SHA-256: `{report['lock_sha256']}`.", ""]
    for name, result in report["commands"].items():
        lines.append(f"- `{name}`: exit `{result['returncode']}`; `{result['stdout']}` / `{result['stderr']}`.")
        if result.get("error"):
            lines.append("  - " + result["error"].replace("\n", " "))
    lines += ["", "Raw command outputs are local target artifacts; hashes and commands are in JSON.", "",
              "## Expressions needing explicit review", "", "No automatic approval: this lists non-basic expressions and text flags, not a risk score.", "",
              "| Package | Declared license | Scope | Review flags |", "| --- | --- | --- | --- |"]
    for p in report["packages"]:
        if p["review_flags"]:
            lines.append(f"| {p['name']} {p['version']} | {p['license'] or 'UNKNOWN'} | {scope_label(p)} | {', '.join(p['review_flags'])} |")
    lines += ["", "## Missing evidence / manual follow-up", ""]
    missing = [p for p in report["packages"] if p["issues"]]
    if not missing:
        lines.append("No missing local manifests or conventionally named license/notice files detected. This is not proof that notices are legally complete.")
    for p in missing:
        lines.append(f"- **{p['name']} {p['version']}** ({scope_label(p)}): " + "; ".join(p["issues"]))
    lines.extend("- " + issue for issue in report["graph_issues"])
    lines += ["", "## Collected texts", "", "`cargo-notices.txt` contains every discovered local license/notice file for all-lock packages,",
              "not just the Windows runtime candidates. File bytes are preserved between labeled",
              "boundaries; duplicate texts and alternative licenses are deliberately retained.",
              "Use JSON evidence paths, byte lengths and hashes to locate and verify each file.",
              "Review applicability before using this as distribution notices; separately audit native/assets.", "",
              "## All locked packages", "", "| Package | Source | License | Scope | Text files |", "| --- | --- | --- | --- | ---: |"]
    for p in report["packages"]:
        lines.append(f"| {p['name']} {p['version']} | {p['source'] or 'workspace/path'} | {p['license'] or 'UNKNOWN'} | {scope_label(p)} | {p.get('license_text_count', 0)} |")
    return "\n".join(lines) + "\n"


def write_notices(path, collected):
    with path.open("wb") as stream:
        stream.write(("Cargo local license/notice collection — all-lock scope\n\n" + SCOPE + "\n").encode("utf-8"))
        for entry, evidence, raw in collected:
            heading = (f"\n===== BEGIN {entry['name']} {entry['version']} :: {evidence['path']} =====\n"
                       f"Source: {entry['source'] or 'workspace/path'}\nOrigin: {entry['evidence_origin']}\n"
                       f"Declared license: {entry['license'] or 'UNKNOWN'}\nScope: {scope_label(entry)}\n"
                       f"SHA-256: {evidence['sha256']}\nBytes: {len(raw)}\n----- ORIGINAL FILE BYTES -----\n")
            stream.write(heading.encode("utf-8"))
            stream.write(raw)
            stream.write(f"\n===== END {entry['name']} {entry['version']} :: {evidence['path']} =====\n".encode("utf-8"))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--cargo-home", type=Path, default=Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")))
    parser.add_argument("--target", default=TARGET)
    parser.add_argument("--timeout", type=int, default=120, help="Per-command timeout in seconds")
    args = parser.parse_args(argv)
    root, cargo_home = args.root.resolve(), args.cargo_home.resolve()
    work, output = root / "target/license-audit", root / "docs-pri/licenses"
    notices = root / "docs/licenses/cargo-notices.txt"
    work.mkdir(parents=True, exist_ok=True)
    output.mkdir(parents=True, exist_ok=True)
    notices.parent.mkdir(parents=True, exist_ok=True)
    original_lock = (root / "Cargo.lock").read_bytes()
    commands = capture(root, work, args.target, args.timeout)
    if (root / "Cargo.lock").read_bytes() != original_lock:
        raise RuntimeError("Cargo.lock changed during audit; refusing to publish a mixed snapshot")
    def successful(name):
        return commands[name]["returncode"] == 0
    metadata = read_json(work / "all-targets-metadata.stdout") if successful("all-targets-metadata") else None
    windows = read_json(work / "windows-metadata.stdout") if successful("windows-metadata") else None
    trees = {name: (work / (name + ".stdout")).read_text(encoding="utf-8")
             for name in ("windows-normal-build", "windows-runtime", "windows-with-dev") if successful(name)}
    report, collected = inventory(root, cargo_home, metadata, windows, trees)
    report.update({"target": args.target, "commands": commands, "scope": SCOPE.strip()})
    write_notices(notices, collected)
    report["notices_sha256"] = digest(notices.read_bytes())
    (output / "cargo-inventory.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (output / "cargo-inventory.md").write_text(markdown(report), encoding="utf-8")
    print(json.dumps(report["counts"], indent=2))
    print("Inventories written to docs-pri/licenses; public notices to docs/licenses/cargo-notices.txt; no legal compliance verdict.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
