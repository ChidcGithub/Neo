"""Pinned drawing CI builds and fail-closed release assembly (Python 3.11+).

No evaluation assembly, downloads, approval generation or legal conclusions.
Only build invokes Cargo and the two explicitly windowless --version commands.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import struct
import subprocess
import sys
import tomllib
from datetime import date, datetime, timezone
from pathlib import Path
from xml.etree import ElementTree

# Only stdlib-only primitives, never the evaluation build/assembly path.
if __package__:
    from .check_distribution_review import check_review, fields, relative_file, require, unique_object
    from .package_combined import PEImports, checked, copy_verified, digest, git, source_allowed, tree_digest, write_json
else:
    from check_distribution_review import check_review, fields, relative_file, require, unique_object
    from package_combined import PEImports, checked, copy_verified, digest, git, source_allowed, tree_digest, write_json

ROOT = Path(__file__).resolve().parents[1]
LOCK = ROOT / "tools/drawing-runtime.lock.json"
TARGET = "x86_64-pc-windows-msvc"
REVIEWED = "distribution/legal/app/REVIEWED.md"
RUST_TEXT = r"distribution/legal/app/rust/texts/[0-9a-f]{64}\.txt"
ARTIFACTS = ("neo-drawing.exe", "neo-blackboard.exe", "DirectML.dll")


def read_json(path):
    return json.loads(checked(path).read_text(encoding="utf-8"), object_pairs_hook=unique_object)


def load_lock(path=LOCK):
    lock = read_json(path)
    require(type(lock.get("schema")) is int and lock["schema"] == 1, "Unsupported drawing lock")
    require(isinstance(lock.get("commit"), str) and re.fullmatch(r"[0-9a-f]{40}", lock["commit"]), "Drawing ref must be a fixed 40hex SHA")
    require(lock.get("repository") == "https://github.com/ChidcGithub/NeoRuntime-drawing", "Unexpected drawing repository")
    require(isinstance(lock.get("rust"), str) and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", lock["rust"]), "Expected explicit Rust toolchain")
    require(lock.get("target") == TARGET, "Expected Windows x64 MSVC target")
    return lock


def approval(lock):
    require(lock.get("public_approved") is True and lock.get("distribution_review") == "reviewed",
            "Drawing public distribution is not approved (lock gate)")
    review = lock.get("review")
    fields(review, {"source_commit", "source_tree_sha256", "cargo_lock_sha256", "version", "reviewed_sha256", "legal_files"}, "drawing review binding")
    require(review["source_commit"] == lock["commit"], "Review source commit differs from lock")
    for key in ("source_tree_sha256", "cargo_lock_sha256", "reviewed_sha256"):
        require(isinstance(review[key], str) and re.fullmatch(r"[0-9a-f]{64}", review[key]), "Invalid review digest: " + key)
    require(isinstance(review["version"], str) and review["version"].strip(), "Missing reviewed version")
    require(isinstance(review["legal_files"], dict) and review["legal_files"], "Missing reviewed legal manifest")
    return review


def source_info(source, lock):
    source = checked(source)
    require(git(source, "rev-parse", "HEAD") == lock["commit"], "Drawing checkout differs from lock")
    require(not git(source, "status", "--porcelain=v1", "--untracked-files=all"), "Drawing checkout must be clean")
    tree = git(source, "ls-tree", "-rz", "--full-tree", lock["commit"], raw=True)
    if not isinstance(tree, bytes):
        raise TypeError("Expected raw Git tree bytes")
    names = []
    for entry in tree.split(b"\0"):
        if not entry:
            continue
        meta, name = entry.split(b"\t", 1)
        require(meta.split()[0] in (b"100644", b"100755"), "Submodules/symlinks are not release inputs")
        names.append(name.decode("utf-8"))
    files = {name: digest(relative_file(source, name)) for name in sorted(names) if source_allowed(name, lock)}
    require({"Cargo.toml", "Cargo.lock", "LICENSE"} <= files.keys(), "Incomplete source manifest")
    version = tomllib.loads((source / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]
    return {"source_commit": lock["commit"], "source_tree_sha256": hashlib.sha256(tree).hexdigest(),
            "cargo_lock_sha256": files["Cargo.lock"]["sha256"], "version": version,
            "files": files, "source_files_sha256": tree_digest(files)}, set(names)


def verify_review(source, lock):
    review = approval(lock)
    info, names = source_info(source, lock)
    for key in ("source_commit", "source_tree_sha256", "cargo_lock_sha256", "version"):
        require(review[key] == info[key], "Drawing review binding mismatch: " + key)
    require(REVIEWED in names, "Missing pinned REVIEWED.md; local legal notes are not approval")
    marker = relative_file(source, REVIEWED)
    require(digest(marker)["sha256"] == review["reviewed_sha256"], "REVIEWED.md hash mismatch")
    # A version-bound record plus a substantive human review, not a bool/empty marker.
    match = re.fullmatch(r"```json\s*\n(.*?)\n```\s*\n(.+)", marker.read_text(encoding="utf-8").strip(), re.DOTALL)
    if match is None or not match[2].strip():
        raise ValueError("REVIEWED.md needs JSON binding and human review body")
    record = json.loads(match[1], object_pairs_hook=unique_object)
    fields(record, {"schema", "status", "version", "cargo_lock_sha256", "reviewer", "date"}, "REVIEWED metadata")
    require(type(record["schema"]) is int and record["schema"] == 1 and record["status"] == "APPROVED", "REVIEWED metadata is not approved")
    for key in ("version", "cargo_lock_sha256"):
        require(record[key] == info[key], "REVIEWED version/lock mismatch")
    require(isinstance(record["reviewer"], str) and record["reviewer"].strip(), "Missing human reviewer")
    require(isinstance(record["date"], str) and re.fullmatch(r"\d{4}-\d{2}-\d{2}", record["date"])
            and date.fromisoformat(record["date"]) <= datetime.now(timezone.utc).date(), "Invalid review date")
    legal = set(lock["legal_files"]) | {name for name in names if re.fullmatch(RUST_TEXT, name)}
    require(set(review["legal_files"]) == legal, "Reviewed legal manifest must exactly match allowlist")
    for name in sorted(legal):
        require(name in names, "Missing pinned legal text: " + name)
        path = relative_file(source, name)
        value = digest(path)["sha256"]
        require(value == review["legal_files"][name], "Legal manifest hash mismatch: " + name)
        require(path.read_text(encoding="utf-8-sig").strip(), "Empty legal text: " + name)
        if re.fullmatch(RUST_TEXT, name):
            require(value == path.stem, "Rust legal body does not match its hash filename")
    return info, sorted(legal)


def directml_provenance(source, lock):
    """Read provenance from Git objects, never ignored/local legal supplements."""
    base = "distribution/legal/app/native/"
    evidence_name = base + "directml/nuget-evidence.json"
    nuspec_name = base + "directml/Microsoft.AI.DirectML.nuspec"
    dist_name = base + "ort-sys/dist.txt"
    inputs = {}

    def pinned(name):
        data = git(source, "show", lock["commit"] + ":" + name, raw=True)
        if not isinstance(data, bytes):
            raise TypeError("Expected pinned provenance bytes")
        inputs[name] = hashlib.sha256(data).hexdigest()
        return data

    evidence = json.loads(pinned(evidence_name), object_pairs_hook=unique_object)
    metadata = ElementTree.fromstring(pinned(nuspec_name))
    ns = {"n": "http://schemas.microsoft.com/packaging/2011/08/nuspec.xsd"}
    version = metadata.findtext("n:metadata/n:version", namespaces=ns)
    require(metadata.findtext("n:metadata/n:id", namespaces=ns) == "Microsoft.AI.DirectML"
            and isinstance(version, str) and re.fullmatch(r"\d+\.\d+\.\d+", version), "Invalid pinned DirectML version")
    url = f"https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/{version}/microsoft.ai.directml.{version}.nupkg"
    require(evidence["source_url"] == url and evidence["member"] == "bin/x64-win/DirectML.dll"
            and evidence["identical"] is True, "DirectML provenance version/member mismatch")
    for key in ("member_sha256", "package_sha256"):
        require(isinstance(evidence[key], str) and re.fullmatch(r"[0-9a-f]{64}", evidence[key]), "Invalid pinned DirectML digest")
    require(evidence["release_dll_sha256"] == evidence["member_sha256"], "DirectML provenance digest mismatch")
    rows = [line.split("\t") for line in pinned(dist_name).decode().splitlines()]
    rows = [row for row in rows if len(row) == 4 and row[:2] == ["none", TARGET]]
    require(len(rows) == 1 and re.fullmatch(r"[0-9A-Fa-f]{64}", rows[0][3]), "Missing exact pinned ORT distribution")
    return {"version": version, "sha256": evidence["member_sha256"], "source_url": url,
            "package_sha256": evidence["package_sha256"], "ort_archive_sha256": rows[0][3],
            "evidence_sha256": inputs}


def trusted_directml_target(link, runtime, provenance):
    """Allow only one file symlink; reject chains, junctions, UNC and arbitrary roots."""
    checked(link.parent)
    require(stat.S_ISLNK(link.lstat().st_mode), "Only a DirectML file symlink may be materialized")
    raw = os.readlink(link)
    # Windows readlink commonly returns the local extended-length prefix.
    raw = raw.removeprefix("\\\\?\\")
    target = Path(raw)
    if not target.is_absolute():
        target = link.parent / target
    require(not str(target).startswith(("\\\\", "//")) and ".." not in target.parts,
            "Unsafe DirectML symlink target")
    target = checked(target)  # checks every ancestor before resolving; no chained links
    allowed = False
    local = os.environ.get("LOCALAPPDATA")
    if local:
        expected = checked(Path(local) / "ort.pyke.io/dfbin" / TARGET / provenance["ort_archive_sha256"] / "onnxruntime/lib/DirectML.dll")
        allowed = target == expected
    # ort-sys falls back to OUT_DIR if its global extraction cache is unavailable.
    build_root = checked(runtime / "build")
    if target.is_relative_to(build_root):
        parts = target.relative_to(build_root).parts
        allowed = allowed or (len(parts) == 5 and re.fullmatch(r"ort-sys-[0-9a-f]+", parts[0]) is not None
                              and parts[1:] == ("out", "onnxruntime", "lib", "DirectML.dll"))
    require(allowed, "DirectML symlink target is outside the pinned ORT build/cache location")
    return target


def materialize_directml(runtime, provenance):
    """Validate and replace only the new Cargo output link before any EXE launch."""
    runtime = checked(runtime)
    dll = runtime / "DirectML.dll"
    linked = stat.S_ISLNK(dll.lstat().st_mode)
    path = trusted_directml_target(dll, runtime, provenance) if linked else relative_file(runtime, dll.name)
    expected = digest(path)
    require(expected["sha256"] == provenance["sha256"], "DirectML bytes differ from pinned version/provenance")
    PEImports(path.read_bytes()).imports()
    if linked:
        temporary = runtime / "DirectML.dll.verified"
        require(not os.path.lexists(temporary), "DirectML materialization staging must be new")
        checked(temporary)
        try:
            copy_verified(path, temporary)
            require(digest(temporary) == expected and trusted_directml_target(dll, runtime, provenance) == path,
                    "DirectML target changed during materialization")
            os.replace(temporary, dll)  # replaces the link itself, never writes through it
        finally:
            if temporary.exists():
                temporary.unlink()
    require(digest(dll) == expected, "Materialized DirectML hash mismatch")


def audit_runtime(runtime):
    result = {}
    for name in ARTIFACTS:
        path = relative_file(runtime, name)
        imports = PEImports(path.read_bytes()).imports()
        if name.endswith(".exe"):
            require("directml.dll" in imports, name + " must actually import DirectML.dll (normal or delay)")
        result[name] = {**digest(path), "architecture": "x64", "imports": imports}
    return result


def build_command(lock, target_dir):
    return ["cargo", "+" + lock["rust"], "build", "--locked", "--release", "-p", "neo-drawing", "-p", "neo-blackboard",
            "--target", TARGET, "--target-dir", str(target_dir)]


def validate_receipt(receipt, info, lock, lock_path, target_dir, artifacts):
    fields(receipt, {"schema", "source", "lock_sha256", "command", "toolchain", "artifacts", "version_output", "directml_provenance"}, "drawing build receipt")
    require(type(receipt["schema"]) is int and receipt["schema"] == 1, "Unsupported build receipt")
    require(receipt["source"] == info and receipt["lock_sha256"] == digest(lock_path)["sha256"], "Build receipt source/lock mismatch")
    require(receipt["command"] == build_command(lock, target_dir), "Build receipt command/target mismatch; evaluation builds are not release inputs")
    fields(receipt["toolchain"], {"rustc", "cargo"}, "build toolchain")
    for tool in ("rustc", "cargo"):
        require(isinstance(receipt["toolchain"][tool], str) and receipt["toolchain"][tool].startswith(tool + " " + lock["rust"] + " "), "Build receipt toolchain mismatch")
    fields(receipt["version_output"], ARTIFACTS[:2], "windowless version results")
    for output in receipt["version_output"].values():
        require(isinstance(output, str) and output.endswith(" " + info["version"]) and "\n" not in output, "Build receipt version mismatch")
    require(receipt["artifacts"] == artifacts, "Build receipt artifact mismatch")


def build(source, target_dir, lock_path=LOCK):
    lock = load_lock(lock_path)
    source, target_dir = checked(source), checked(target_dir)
    require(not target_dir.exists(), "Drawing target directory must be new; no evaluation/cache binary reuse")
    require(not target_dir.is_relative_to(source) and not source.is_relative_to(target_dir), "Source and target directories must be separate")
    before, _ = source_info(source, lock)
    provenance = directml_provenance(source, lock)
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["CARGO_BUILD_TARGET"] = TARGET
    toolchain = "+" + lock["rust"]
    versions = {}
    for tool in ("rustc", "cargo"):
        value = subprocess.check_output([tool, toolchain, "--version"], env=env, text=True, timeout=30).strip()
        require(value.startswith(tool + " " + lock["rust"] + " "), "Wrong drawing toolchain")
        versions[tool] = value
    command = build_command(lock, target_dir)
    subprocess.run(command, cwd=source, env=env, check=True, timeout=3600)
    runtime = target_dir / TARGET / "release"
    materialize_directml(runtime, provenance)
    artifacts = audit_runtime(runtime)
    version_output = {}
    for name in ARTIFACTS[:2]:
        result = subprocess.run([str(runtime / name), "--version"], cwd=runtime, capture_output=True, check=True, timeout=30)
        output = (result.stdout + result.stderr).decode("utf-8").strip()
        require(output.endswith(" " + before["version"]) and "\n" not in output, "Unexpected --version result: " + name)
        version_output[name] = output
    after, _ = source_info(source, lock)
    require(before == after, "Drawing sources changed during build")
    receipt = {"schema": 1, "source": before, "lock_sha256": digest(lock_path)["sha256"],
               "command": command, "toolchain": versions, "artifacts": artifacts, "version_output": version_output,
                              "directml_provenance": provenance}
    write_json(target_dir / "drawing-build.json", receipt)
    return receipt


def assemble(source, target_dir, package, lock_path=LOCK, root=ROOT):
    # Standalone invocation cannot bypass the main project's approval gate either.
    check_review(Path(root) / "tools/distribution-review.json", root=root)
    lock = load_lock(lock_path)
    source, target_dir, package = checked(source), checked(target_dir), checked(package)
    info, legal = verify_review(source, lock)
    receipt = read_json(target_dir / "drawing-build.json")
    runtime = target_dir / TARGET / "release"
    artifacts = audit_runtime(runtime)
    validate_receipt(receipt, info, lock, lock_path, target_dir, artifacts)
    provenance = directml_provenance(source, lock)
    require(receipt["directml_provenance"] == provenance and artifacts["DirectML.dll"]["sha256"] == provenance["sha256"],
            "DirectML receipt/pinned provenance mismatch")
    require(package.is_dir(), "Assemble the main package first")
    destinations = [package / "apps" / kind for kind in ("drawing", "blackboard")]
    legal_root = package / "docs/licenses/NeoRuntime-drawing"
    source_root = package / "sources/NeoRuntime-drawing"
    for path in [*destinations, legal_root, source_root]:
        require(not checked(path).exists(), "Drawing assembly destination must be new: " + str(path))
    for kind, destination in zip(("drawing", "blackboard"), destinations):
        for name in ("neo-" + kind + ".exe", "DirectML.dll"):
            copy_verified(runtime / name, destination / name)
        (destination / "MSVC-PREREQUISITE.txt").write_text(
            "Requires the official Microsoft Visual C++ x64 Redistributable.\n"
            "No app-local CRT is supplied in this subdirectory. Do not copy System32 or developer CRT DLLs.\n"
            "https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist\n"
            "Clean-machine runtime acceptance is not established by this package check.\n", encoding="utf-8")
    for name in legal:
        copy_verified(source / name, legal_root / name)
    copy_verified(source / REVIEWED, legal_root / "REVIEWED.md")
    for name in info["files"]:
        copy_verified(source / name, source_root / name)
    write_json(legal_root / "SOURCE.json", {**info, "repository": lock["repository"], "lock_sha256": digest(lock_path)["sha256"],
               "reviewed_sha256": lock["review"]["reviewed_sha256"], "legal_files": lock["review"]["legal_files"],
               "artifacts": artifacts, "directml_provenance": provenance, "crt_policy": "external-official-msvc-x64-prerequisite",
               "clean_install_verified": False, "scope": "Allowlisted project source, not complete third-party corresponding source; human review must cover delivery obligations"})
    hashes = {path.relative_to(package).as_posix(): digest(path) for base in [*destinations, legal_root, source_root]
              for path in sorted(base.rglob("*")) if path.is_file()}
    write_json(legal_root / "FILES.sha256.json", hashes)
    return hashes


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("lock-outputs", "check-approval", "verify-review", "build", "assemble"))
    parser.add_argument("--lock", type=Path, default=LOCK)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--target-dir", type=Path)
    parser.add_argument("--package", type=Path)
    args = parser.parse_args(argv)
    try:
        lock = load_lock(args.lock)
        if args.command == "lock-outputs":
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
                stream.writelines(f"{key}={value}\n" for key, value in (
                    ("commit", lock["commit"]),
                    ("repository", lock["repository"].removeprefix("https://github.com/")),
                    ("rust", lock["rust"]),
                ))
        elif args.command == "check-approval":
            approval(lock)
        else:
            require(args.source is not None, "--source is required")
            if args.command == "verify-review":
                verify_review(args.source, lock)
            else:
                require(args.target_dir is not None, "--target-dir is required")
                if args.command == "build":
                    build(args.source, args.target_dir, args.lock)
                else:
                    require(args.package is not None, "--package is required")
                    assemble(args.source, args.target_dir, args.package, args.lock)
    except (OSError, ValueError, KeyError, TypeError, struct.error, ElementTree.ParseError, subprocess.SubprocessError) as error:
        print(f"Drawing {args.command} BLOCKED: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
