"""Local-only combined evaluation, NOT a release packager (Python 3.11+).

Example (existing binaries, no GUI, no downloads):
  python -B tools/package_combined.py --mode evaluation --name local-current \
    --neo-exe target/x86_64-pc-windows-msvc/release/neo.exe \
    --local-source ../NeoRuntime-drawing \
    --runtime-dir ../NeoRuntime-drawing/target/release --build-runtime

Later native-agent output can be supplied with --neo-exe and a NEW --name;
--build-receipt <previous evaluation>/build.local.json avoids a second build.
Without --local-source, sources are exported from the exact locked Git commit
into .cache/drawing-src (no network). --drawing-repository selects the local
Git object store, not an unpinned checkout. No ZIP, installer, release gate
bypass, download, installation, GUI, upload, or public approval is implemented.
--include-local-resources adds exact local wake/STT/language/ONNX/public legal
inputs and the existing MinGit tree matched against its prior local audit
inventory. Missing/extra/changed files fail closed. --native-receipt accepts a
native continuation-status JSON and revalidates its manifest, libraries, graph
and EXE/MAP audit without executing Neo. No evidence means unknown, not fixed
or cleared. Technical TTS removal is NOT legal approval or reproducibility.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import time
import tomllib

if __package__:
    from . import audit_native_link, build_sherpa_asr, prepare_runtime_distribution as distribution
    from .check_release import validate_hi_neo
else:
    import audit_native_link
    import build_sherpa_asr
    import prepare_runtime_distribution as distribution
    from check_release import validate_hi_neo

ROOT = Path(__file__).resolve().parents[1]
OUTPUT_ROOT = ROOT / "target/combined-evaluation"
CACHE_ROOT = ROOT / ".cache/drawing-src"
LOCK_PATH = ROOT / "tools/drawing-runtime.lock.json"
MARKER = "LOCAL EVALUATION — NOT_FOR_DISTRIBUTION\n"
LOCAL_RESOURCES = {
    **{f"resources/lang/{n}.lang": f"resources/lang/{n}.lang" for n in ("zh-CN", "en-US")},
    **{f"resources/models/wake/{n}": f"crates/neo-wake/assets/{n}" for n in
       ("melspectrogram.onnx", "embedding_model.onnx", "hi_neo.onnx")},
    **{f"runtime/onnx/{n}": f"crates/neo-wake/assets/{n}" for n in
       ("onnxruntime.dll", "onnxruntime_providers_shared.dll")},
    **{f"resources/models/stt/{n}": f"crates/neo-stt/assets/{n}" for n in
       ("sense-voice/model.int8.onnx", "sense-voice/tokens.txt", "vad/silero_vad.onnx")},

}
# Exact public legal text/model-card selection, NOT a recursive legal-directory copy.
PUBLIC_LEGAL = (
    "cargo-notices.txt",
    "assets/egui-toast-0.22.0-MIT.txt", "assets/egui_commonmark-0.25.0-Apache-2.0.txt",
    "assets/egui_commonmark-0.25.0-MIT.txt", "assets/egui_flex-0.8.0-MIT.txt",
    "assets/harness-primitives-0.0.1-rc.1-BSD-3-Clause.txt", "assets/katex-0.18.7-font-notices.txt",
    "assets/OFL-1.1-standard.txt", "assets/phosphor-web-2.1.2-MIT.txt",
    "models/ATTRIBUTIONS.txt", "models/CC-BY-4.0.txt", "models/CC-BY-NC-SA-4.0.txt",
    "models/hi_neo-model.json", "models/hi_neo-MIT.txt",
    "models/FunASR-MODEL_LICENSE", "models/livekit-0.2.1-LICENSE", "models/notice-sources.json",
    "models/openwakeword-LICENSE", "models/piper-generator-LICENSE", "models/piper-libritts-high-MODEL_CARD",
    "models/sensevoice-model-card.md", "models/sherpa-sense-LICENSE",
    "models/sherpa-sense-README.md", "models/silero-v5.1-LICENSE", "models/torchlibrosa-LICENSE",
    "runtime/gitbash-SOURCE.md", "runtime/MSVC-REDISTRIBUTION.md", "runtime/onnxruntime-LICENSE",
    "runtime/onnxruntime-ThirdPartyNotices.txt",
    "supplemental/index.json",
)
CRT_FILE = re.compile(r"(?:vcruntime|msvcp|msvcr|concrt|vccorlib|ucrtbase).*\.dll", re.I)
OMISSIONS = [
    "MSVC CRT: external official prerequisite; no CRT/System32 files copied or installed",
    "Optional drawing TexTeller/HWR model, personal fonts, real documents and user configuration",
    "Private/source audit notes, source ZIPs and complete corresponding-source companion (still unresolved)",
    "Neo/Bash/model execution, GUI, microphone/capture, full dependency closure and clean-machine acceptance",
    "ZIP, NSIS, release approval and upload",
]


def safe_relative(name):
    if not isinstance(name, str) or not name or "\\" in name:
        raise ValueError("Unsafe relative payload path")
    for part in name.split("/"):
        if (not part or part in (".", "..") or part.endswith((".", " "))
                or any(ord(c) < 32 or c in ':<>|\"?*' for c in part)
                or part.split(".")[0].upper() in {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"}
                or re.fullmatch(r"(?:COM|LPT)[0-9¹²³]", part.split(".")[0], re.I)):
            raise ValueError("Unsafe relative payload path: " + name)
    return name


def tree_files(root):
    """Check every directory before descending; never follow reparse points."""
    root = checked(root)
    pending, files, folded = [root], {}, set()
    while pending:
        for path in sorted(pending.pop().iterdir()):
            checked(path)
            name = safe_relative(path.relative_to(root).as_posix())
            if name.casefold() in folded:
                raise ValueError("Case-colliding payload path")
            folded.add(name.casefold())
            if len(folded) > 50000:
                raise ValueError("Payload tree entry budget exceeded")
            if path.is_dir():
                pending.append(path)
            elif path.is_file():
                files[name] = path
            else:
                raise ValueError("Non-regular payload entry")
    return files


def gitbash_plan(root, inventory):
    distribution.verify_distribution(inventory.parent, root)
    record = digest(inventory)
    manifest = json.loads(checked(inventory).read_text(encoding="utf-8"))
    entries = manifest.get("runtime", {}).get("files")
    if not isinstance(entries, list) or not 1 <= len(entries) <= 50000:
        raise ValueError("Missing bounded MinGit file inventory")
    expected, folded = {}, set()
    for entry in entries:
        name = safe_relative(entry["path"])
        if name.casefold() in folded or CRT_FILE.fullmatch(PurePosixPath(name).name):
            raise ValueError("Duplicate or forbidden CRT in MinGit inventory")
        if name != "LICENSE.txt" and name.split("/")[0] not in {"cmd", "etc", "usr", "mingw64"}:
            raise ValueError("Unexpected MinGit runtime root")
        if any(x.lower() in {"home", ".ssh", ".git", ".gitconfig", ".bash_history", "personal", "private"} for x in name.split("/")):
            raise ValueError("Personal data path in MinGit inventory")
        if not re.fullmatch(r"[a-f0-9]{64}", entry["sha256"]) or type(entry["size"]) is not int or entry["size"] < 0:
            raise ValueError("Invalid MinGit inventory hash/size")
        folded.add(name.casefold())
        expected[name] = {"sha256": entry["sha256"], "bytes": entry["size"]}
    if not {"LICENSE.txt", "cmd/git.exe", "usr/bin/bash.exe", "usr/bin/sh.exe", "etc/package-versions.txt"} <= expected.keys():
        raise ValueError("Incomplete MinGit runtime inventory")
    if sum(x["bytes"] for x in expected.values()) > 1024**3:
        raise ValueError("MinGit tree exceeds 1 GiB budget")
    actual = tree_files(root)
    if actual.keys() != expected.keys():
        raise ValueError("MinGit tree differs from inventory (extra/missing files); refusing personal extras")
    for name, path in actual.items():
        if digest(path) != expected[name]:
            raise ValueError("MinGit inventory content mismatch: " + name)
    if expected["etc/package-versions.txt"]["sha256"] != manifest["runtime"].get("package_versions_sha256"):
        raise ValueError("MinGit package-versions hash mismatch")
    return actual, {"inventory": record, "files": expected, "file_count": len(expected),
                    "bytes": sum(x["bytes"] for x in expected.values()), "executed": False,
                    "official_archive_authentication": False, "corresponding_source_complete": False,
                    "provenance": "Policy-filtered GCM-free 310-file runtime; NOT an official/CI authenticity attestation"}


def public_legal_plan(root):
    legal = root / "docs/licenses"
    files = {"docs/licenses/" + n: checked(legal / n) for n in PUBLIC_LEGAL}
    index = json.loads((legal / "supplemental/index.json").read_text(encoding="utf-8"))
    for package in index["packages"]:
        for entry in package["files"]:
            name = entry["path"]
            if not re.fullmatch(r"[a-f0-9]{64}\.txt", name) or entry["sha256"] != name[:-4]:
                raise ValueError("Supplemental text must have a content-addressed allowlisted name")
            path = checked(legal / "supplemental" / name)
            if digest(path)["sha256"] != entry["sha256"]:
                raise ValueError("Supplemental license content mismatch")
            files["docs/licenses/supplemental/" + name] = path
    for path in files.values():
        if path.stat().st_size > 16 * 1024**2 or not path.read_text(encoding="utf-8-sig").strip():
            raise ValueError("Empty/oversized/non-text public legal document")
    return files


def local_resource_plan(root, distribution_dir):
    if distribution_dir is None:
        raise ValueError("--include-local-resources requires explicit --gitbash-distribution-dir; no raw cache fallback")
    distribution_dir = checked(distribution_dir)
    report = distribution.verify_distribution(distribution_dir)
    companion = distribution.verify_companion(distribution_dir, report)
    files = {name: checked(root / source) for name, source in LOCAL_RESOURCES.items()}
    files.update(public_legal_plan(root))
    validate_hi_neo(files["resources/models/wake/hi_neo.onnx"], root / "docs/licenses/models")
    bash, bash_info = gitbash_plan(distribution_dir / "runtime/gitbash", distribution_dir / "MANIFEST.json")
    bash_info["source_companion"] = {k: companion[k] for k in ("path", "sha256", "size")}
    bash_info["source_companion_binding_verified"] = True
    bash_info["source_companion_included"] = False
    files.update({"runtime/gitbash/" + n: path for n, path in bash.items()})
    files.update({"docs/runtime-distribution/" + n: distribution_dir / n
                  for n in (*distribution.DISTRIBUTION_DOCUMENTS, 'source-manifest.json', 'source-companion-record.json')})
    records = {name: digest(path) for name, path in files.items()}
    for name, entry in records.items():
        safe_relative(name)
        if CRT_FILE.fullmatch(PurePosixPath(name).name) or entry["bytes"] > 1024**3:
            raise ValueError("Forbidden CRT or oversized local input")
        if entry["bytes"] == 0 and not name.startswith("runtime/gitbash/"):
            raise ValueError("Empty required resource")
    if sum(x["bytes"] for x in records.values()) > 2 * 1024**3:
        raise ValueError("Local resource payload exceeds 2 GiB")
    languages = [json.loads(files[f"resources/lang/{lang}.lang"].read_text(encoding="utf-8")) for lang in ("zh-CN", "en-US")]
    if (not all(isinstance(x, dict) and x and all(isinstance(k, str) and isinstance(v, str) for k, v in x.items()) for x in languages)
            or languages[0].keys() != languages[1].keys()):
        raise ValueError("Language catalog keys/types mismatch")
    return files, {"included": True, "records": records, "file_count": len(records),
                   "bytes": sum(x["bytes"] for x in records.values()), "gitbash": bash_info,
                   "model_provenance": "Local development assets only; hashes observed at packaging, NOT CI pinned/download-verified models",
                   "models_executed": False, "source_to_destination": {n: (p.relative_to(root).as_posix() if p.is_relative_to(root)
                                                                                  else "explicit-external-distribution/" + n) for n, p in files.items()},
                   "omissions": OMISSIONS}


def copy_local_resources(plan, info, package):
    for name, source in plan.items():
        destination = package / safe_relative(name)
        if destination.exists():
            raise ValueError("Resource would overwrite existing payload")
        copy_verified(source, destination, allow_empty=name.startswith("runtime/gitbash/"))
        if digest(destination) != info["records"][name]:
            raise ValueError("Resource changed after preflight: " + name)


def verify_file_manifest(package):
    actual = tree_files(package)
    manifest = json.loads((package / "FILES.sha256.json").read_text(encoding="utf-8"))
    if set(actual) != set(manifest) | {"FILES.sha256.json"}:
        raise ValueError("Final payload file set differs from manifest")
    for name, record in manifest.items():
        safe_relative(name)
        if digest(actual[name]) != record:
            raise ValueError("Final payload hash mismatch: " + name)
    return {"file_count": len(actual), "bytes": sum(p.stat().st_size for p in actual.values()),
            "all_files_verified": True, "manifest": digest(package / "FILES.sha256.json"),
            "manifest_self_hash": "Recorded here, outside package, to avoid self-reference"}


def checked(path):
    path = Path(path).absolute()
    if ".." in path.parts:
        # Resolve ordinary caller-relative input, then inspect every real ancestor.
        path = Path(os.path.abspath(path))
    for item in (path, *path.parents):
        try:
            info = item.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
            raise ValueError("Symlink/reparse input or output is forbidden")
    return path.resolve()


def confined(path, root=OUTPUT_ROOT):
    path, root = checked(path), checked(root)
    if path == root or not path.is_relative_to(root):
        raise ValueError("Output must be strictly below target/combined-evaluation")
    return path


def digest(path):
    path = checked(path)
    if not path.is_file():
        raise ValueError("Expected a regular file: " + str(path))
    before = path.stat()
    with path.open("rb") as stream:
        value = hashlib.file_digest(stream, "sha256").hexdigest()
    after = path.stat()
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise ValueError("Input changed while hashing")
    return {"sha256": value, "bytes": after.st_size}


def write_json(path, value):
    path.write_text(json.dumps(value, ensure_ascii=True, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def git(repo, *args, raw=False):
    result = subprocess.run(["git", "--no-pager", "--no-optional-locks", "-C", str(repo), *args],
                            capture_output=True, check=True, timeout=30)
    return result.stdout if raw else result.stdout.decode("utf-8").strip()


def revision(repo):
    commit = git(repo, "rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Expected full Git SHA")
    status = git(repo, "status", "--porcelain=v1", "-z", "--untracked-files=all", raw=True)
    return {"commit": commit, "dirty": bool(status), "status_sha256": hashlib.sha256(status).hexdigest()}


def source_allowed(name, lock):
    p = PurePosixPath(name)
    if p.is_absolute() or any(x in {"..", "."} or ":" in x or "\\" in x for x in p.parts):
        return False
    if name in lock["source_root_files"]:
        return True
    for package in lock["workspace_packages"]:
        if name == package + "/Cargo.toml" or name == package + "/build.rs":
            return True
        if any(name.startswith(package + "/" + folder + "/") for folder in ("src", "tests", "examples")):
            return p.suffix == ".rs"
    return False


def pinned_names(repo, lock):
    exact = git(repo, "rev-parse", lock["commit"] + "^{commit}")
    if exact != lock["commit"]:
        raise ValueError("Pinned commit unavailable")
    return git(repo, "ls-tree", "-r", "--name-only", "-z", exact, raw=True).decode("utf-8").strip("\0").split("\0")


def tree_digest(files):
    data = json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(data).hexdigest()


def source_snapshot(repo, lock, local=None):
    names = sorted(n for n in pinned_names(repo, lock) if source_allowed(n, lock))
    if not {"Cargo.toml", "Cargo.lock", "LICENSE"} <= set(names):
        raise ValueError("Incomplete pinned source tree")
    source = checked(local) if local else checked(CACHE_ROOT / lock["commit"])
    if not local:
        source.mkdir(parents=True, exist_ok=True)
    files = {}
    for name in names:
        path = checked(source / name)
        if not local:
            data = git(repo, "show", lock["commit"] + ":" + name, raw=True)
            if path.exists():
                if path.read_bytes() != data:
                    raise ValueError("Pinned source cache has been modified: " + name)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
        files[name] = digest(path)
    # Exact pinned names form the privacy boundary, even in local override mode.
    # Reject extra build-capable files rather than silently building unhashed code.
    for package in lock["workspace_packages"]:
        for folder in ("src", "tests", "examples"):
            base = checked(source / package / folder)
            if base.exists():
                for path in base.rglob("*.rs"):
                    checked(path)
                    if path.relative_to(source).as_posix() not in files:
                        raise ValueError("Unpinned Rust source requires an explicit lock update")
        build_script = source / package / "build.rs"
        if build_script.exists() and build_script.relative_to(source).as_posix() not in files:
            raise ValueError("Unpinned build script")
    info = revision(local if local else repo)
    return source, {"mode": "local-override" if local else "pinned-commit",
                    "pinned_commit": lock["commit"], "checkout": info,
                    "source_commit": info["commit"] if local else lock["commit"],
                    "files": files, "tree_sha256": tree_digest(files),
                    "scope": "Complete exact-name allowlisted project source snapshot; no dependencies/private/API/legal working notes"}


def neo_source_manifest():
    names = git(ROOT, "ls-files", "--cached", "--others", "--exclude-standard", "-z", raw=True).decode().strip("\0").split("\0")
    files = {}
    for name in sorted(set(names)):
        if name in {"Cargo.toml", "Cargo.lock", "LICENSE", "NOTICE"} or (
            name.startswith(("crates/", "vendor/")) and
            (name.endswith(".rs") or PurePosixPath(name).name == "Cargo.toml")
        ):
            path = ROOT / name
            if path.exists():
                files[name] = digest(path)
    return {**revision(ROOT), "files": files, "tree_sha256": tree_digest(files),
            "binary_source_binding": "unverified existing executable; checkout hashes are not a build attestation"}


class PEImports:
    """Minimal bounded x64 PE reader for normal AND delay DLL import names."""
    def __init__(self, data):
        self.data = data
        if self.take(0, 2) != b"MZ":
            raise ValueError("Not a PE image")
        pe = self.unpack("<I", 0x3c)[0]
        if self.take(pe, 4) != b"PE\0\0":
            raise ValueError("Bad PE signature")
        machine, count, _, _, _, size, _ = self.unpack("<HHIIIHH", pe + 4)
        opt = pe + 24
        if machine != 0x8664 or self.unpack("<H", opt)[0] != 0x20b or not 1 <= count <= 96 or size < 112:
            raise ValueError("Expected x64 PE32+")
        self.base = self.unpack("<Q", opt + 24)[0]
        n = self.unpack("<I", opt + 108)[0]
        self.dirs = [self.unpack("<II", opt + 112 + i * 8) for i in range(min(n, 16)) if 112 + (i + 1) * 8 <= size]
        self.sections = []
        for i in range(count):
            vs, va, rs, off = self.unpack("<IIII", opt + size + i * 40 + 8)
            self.sections.append((va, rs, off))

    def take(self, off, size):
        if off < 0 or size < 0 or off + size > len(self.data):
            raise ValueError("Truncated PE")
        return self.data[off:off + size]

    def unpack(self, fmt, off):
        return struct.unpack(fmt, self.take(off, struct.calcsize(fmt)))

    def rva(self, value, size):
        for va, rs, off in self.sections:
            if 0 <= value - va and value - va + size <= rs:
                return self.take(off + value - va, size)
        raise ValueError("Unmapped PE RVA")

    def name(self, value):
        data = bytearray()
        for i in range(260):
            char = self.rva(value + i, 1)
            if char == b"\0":
                name = data.decode("ascii").lower()
                if not re.fullmatch(r"[a-z0-9_.-]+\.dll", name):
                    raise ValueError("Unsafe DLL import")
                return name
            data.extend(char)
        raise ValueError("Unterminated import")

    def imports(self):
        names = set()
        for index, width in ((1, 20), (13, 32)):
            if index >= len(self.dirs):
                continue
            start, size = self.dirs[index]
            if not start:
                continue
            terminated = False
            for offset in range(0, min(size, 10000 * width), width):
                row = self.rva(start + offset, width)
                if not any(row):
                    terminated = True
                    break
                values = struct.unpack("<" + "I" * (width // 4), row)
                name = values[3] if index == 1 else values[1]
                if index == 13 and not values[0] & 1:
                    name -= self.base
                names.add(self.name(name))
            if not terminated:
                raise ValueError("Unterminated import table")
        return sorted(names)


def audit_dependencies(package):
    result = {}
    system = Path(os.environ.get("SystemRoot", "C:/Windows")) / "System32"
    # MinGit includes x86/managed helper images; it is explicitly outside this x64 audit.
    names = ("neo.exe", "apps/drawing/neo-drawing.exe", "apps/drawing/DirectML.dll",
             "apps/blackboard/neo-blackboard.exe", "apps/blackboard/DirectML.dll",
             "runtime/onnx/onnxruntime.dll", "runtime/onnx/onnxruntime_providers_shared.dll")
    for binary in sorted(package / name for name in names if (package / name).is_file()):
        imports = PEImports(binary.read_bytes()).imports()
        local = {p.name.lower(): p for p in binary.parent.iterdir() if p.is_file()}
        rows = []
        for name in imports:
            if name in local:
                kind = "same-directory"
            elif name.startswith(("api-ms-win-", "ext-ms-win-")):
                kind = "windows-api-set-unverified"
            elif (system / name).is_file():
                kind = "host-system32-only-NOT-clean-install"
            else:
                kind = "unresolved-on-host"
            rows.append({"name": name, "resolution": kind})
        result[binary.relative_to(package).as_posix()] = {"architecture": "x64", "imports": rows}
    return {"images": result, "clean_install_verified": False,
            "scope": "Only three application exes, two DirectML copies and two main ONNX DLLs: static normal/delay names, same-dir + host System32; NOT full symbol/loader/dynamic-load closure",
                        "excluded": {"runtime/gitbash": "Entire MinGit tree, including x86/managed helpers, NOT PE-audited or executed"}}


def runtime_hashes(directory, directml=None):
    return {name: digest(directml if name == "DirectML.dll" and directml else directory / name)
            for name in ("neo-drawing.exe", "neo-blackboard.exe", "DirectML.dll")}


def build_runtime(source, runtime_dir, snapshot, lock, output, directml=None):
    # Reuse this checkout's existing target; never silently build a second target.
    if runtime_dir != checked(source / "target/release"):
        raise ValueError("Build requires --runtime-dir <source>/target/release to avoid an accidental second build")
    versions = {}
    for tool in ("rustc", "cargo"):
        versions[tool] = subprocess.check_output([tool, "--version"], text=True, timeout=15).strip()
        if not versions[tool].startswith(tool + " " + lock["rust"] + " "):
            raise ValueError("Toolchain does not match lock")
    command = ["cargo", "build", "--offline", "--locked", "-p", "neo-drawing", "-p", "neo-blackboard", "--release", "--target-dir", str(source / "target")]
    env = dict(os.environ)
    env.pop("CARGO_BUILD_TARGET", None)
    started = time.monotonic()
    with (output / "build.local.log").open("wb") as log:
        result = subprocess.run(command, cwd=source, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=600)
    receipt = {"command": command, "versions": versions, "returncode": result.returncode,
               "elapsed_seconds": round(time.monotonic() - started, 3),
               "source_tree_sha256": snapshot["tree_sha256"], "source_commit": snapshot["source_commit"],
               "note": "Cargo offline does not sandbox native build-script networking"}
    # Preserve the command outcome even if artifact collection subsequently fails.
    write_json(output / "build.local.json", receipt)
    if result.returncode == 0:
        receipt["artifacts"] = runtime_hashes(runtime_dir, directml)
        write_json(output / "build.local.json", receipt)
    if result.returncode:
        raise ValueError("Runtime build failed; see build.local.log")
    return receipt


def validate_receipt(receipt, snapshot, runtime_dir, directml=None):
    if (receipt.get("returncode") != 0 or receipt.get("source_tree_sha256") != snapshot["tree_sha256"]
            or receipt.get("source_commit") != snapshot["source_commit"]
            or receipt.get("artifacts") != runtime_hashes(runtime_dir, directml)):
        raise ValueError("Build receipt does not match current sources/binaries")


def native_identity(record):
    if (not isinstance(record, dict) or not re.fullmatch(r"[a-f0-9]{64}", record.get("sha256", ""))
            or type(record.get("size")) is not int or record["size"] <= 0):
        raise ValueError("Invalid native evidence hash/size")
    return {"sha256": record["sha256"], "bytes": record["size"]}


def native_evidence_path(name):
    # Status uses both Windows and POSIX repository-relative paths.
    path = checked(ROOT / safe_relative(name.replace("\\", "/")))
    if not path.is_relative_to(ROOT.resolve()):
        raise ValueError("Native evidence path escapes repository")
    return path


def validate_native_receipt(status_path, neo):
    result = {"native_tts_removed": "unknown", "validation": "no-evidence",
              "legal_approved": False, "reproducible_build_verified": False,
              "scope": "Technical native TTS removal only; not GPL clearance, legal approval or runtime acceptance"}
    if status_path is None:
        return result
    status_path = checked(status_path)
    status_hash = digest(status_path)
    status = json.loads(status_path.read_text(encoding="utf-8"))
    evidence = {"status": status_hash}
    paths = {}
    for key in ("native_manifest", "native_receipt", "native_graph", "neo_release", "map"):
        record = status[key]
        path = native_evidence_path(record["path"])
        evidence[key] = digest(path)
        if evidence[key] != native_identity(record):
            raise ValueError("Native evidence hash mismatch: " + key)
        paths[key] = path
    if digest(neo) != evidence["neo_release"]:
        raise ValueError("Native receipt belongs to a different Neo executable")
    manifest = json.loads(paths["native_manifest"].read_text(encoding="utf-8"))
    receipt = json.loads(paths["native_receipt"].read_text(encoding="utf-8"))
    if (manifest.get("schema") != 1 or manifest.get("status") != "native-validated"
            or manifest.get("target") != "x86_64-pc-windows-msvc" or manifest.get("configuration") != "Release"
            or manifest.get("version") != build_sherpa_asr.VERSION
            or manifest.get("source_commit") != build_sherpa_asr.COMMIT
            or manifest.get("source_sha256") != build_sherpa_asr.SOURCE_SHA256
            or receipt.get("source_lock") != status.get("source")
            or receipt["source_lock"].get("commit") != manifest["source_commit"]
            or receipt["source_lock"].get("sha256") != manifest["source_sha256"]
            or native_identity(manifest["receipt"]) != evidence["native_receipt"]
            or native_identity(receipt["graph"]) != evidence["native_graph"]
            or any(manifest.get("options", {}).get(k) != v or receipt.get("options", {}).get(k) != v
                   for k, v in build_sherpa_asr.OPTIONS.items())):
        raise ValueError("Native manifest/receipt/options mismatch")
    libraries = manifest["libraries"]
    libdir = paths["native_manifest"].parent
    expected_names = {name + ".lib" for name in build_sherpa_asr.INSTALLED_LIBS}
    if (set(libraries) != expected_names or libraries != status["native_libraries"]
            or {p.name for p in libdir.glob("*.lib")} != expected_names):
        raise ValueError("Native library set mismatch")
    evidence["libraries"] = {}
    for name, record in libraries.items():
        path = checked(libdir / safe_relative(name))
        actual = digest(path)
        with path.open("rb") as stream:
            archive = stream.read(8) == b"!<arch>\n"
        if not archive or actual != native_identity(record):
            raise ValueError("Native library hash/archive mismatch: " + name)
        evidence["libraries"][name] = actual
    graph = json.loads(paths["native_graph"].read_text(encoding="utf-8"))
    if not isinstance(graph, list) or not graph:
        raise ValueError("Missing native graph evidence")
    graph_names = [name for target in graph for name in [target["name"], *target["sources"]]]
    if (any(build_sherpa_asr.RISK.search(name) for name in graph_names)
            or not {"sherpa-onnx-c-api", "sherpa-onnx-core"} <= {t["name"] for t in graph}
            or any(not any(Path(n).name == required for n in graph_names) for required in
                   ("offline-sense-voice-model.cc", "silero-vad-model.cc", "voice-activity-detector.cc"))):
        raise ValueError("Native graph has forbidden TTS or missing ASR/VAD inputs")
    audit_status = status["map_pe_audit"]
    report_path = native_evidence_path(audit_status["path"])
    evidence["map_pe_report"] = digest(report_path)
    report = json.loads(report_path.read_text(encoding="utf-8"))
    if (native_identity(report["exe"]) != evidence["neo_release"]
            or native_identity(report["map"]) != evidence["map"]):
        raise ValueError("Native audit EXE/MAP hash mismatch")
    # The status does not pin the report itself: record its observed hash AND
    # independently regenerate the static audit rather than trust a summary.
    actual_report = audit_native_link.audit(paths["neo_release"], paths["map"])
    if report != actual_report:
        raise ValueError("Native audit report differs from fresh EXE/MAP audit")
    rows = report["asr_vad"]
    if (report["errors"] != [] or report["export_findings"] != [] or report["map_findings"] != []
            or report.get("map_header_consistent") is not True or report.get("map_symbol_count", 0) <= 0
            or {r["symbol"] for r in rows} != audit_native_link.ASR_VAD
            or not all(r.get("mapped") is True and r.get("executable") is True for r in rows)
            or audit_status.get("errors") != [] or type(audit_status.get("risk_findings")) is not int
            or audit_status["risk_findings"] != 0 or audit_status.get("asr_vad") != rows
            or audit_status.get("map_symbols") != report["map_symbol_count"]
            or audit_status.get("verdict") != report["verdict"]):
        raise ValueError("Native audit lacks mapped ASR/VAD or zero-findings evidence")
    current_roots = {name: digest(ROOT / name) for name in ("Cargo.toml", "Cargo.lock")}
    recorded_roots = {name: native_identity(status["root_files"][name]) for name in current_roots}
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    patch_enabled = cargo.get("patch", {}).get("crates-io", {}).get("sherpa-onnx-sys", {}).get("path") == "vendor/sherpa-onnx-sys"
    association = {"status_root_files": recorded_roots, "current_root_files": current_roots,
                   "root_files_match_status": current_roots == recorded_roots,
                   "root_patch_enabled_in_status": status.get("root_patch_enabled"),
                   "root_patch_enabled_current": patch_enabled,
                   "binary_source_binding": "local status EXE/hash and root-file association only; NOT a complete before-build source attestation"}
    before = status_path.parent / "root-input-hashes.json"
    if before.is_file():
        association["before_root_record"] = digest(before)
        association["before_root_hashes"] = json.loads(before.read_text(encoding="utf-8"))
        association["before_root_note"] = "Earlier root context, not assumed to be the inputs of this EXE build"
    if digest(status_path) != status_hash or digest(report_path) != evidence["map_pe_report"]:
        raise ValueError("Native status/report changed during validation")
    result.update(native_tts_removed=True, validation="validated-local-evidence", evidence=evidence,
                  source_association=association, mapped_asr_vad=[r["symbol"] for r in rows],
                  risk_findings=0, map_symbols=report["map_symbol_count"], audit_verdict=report["verdict"],
                  report_hash_origin="Observed at packaging, independently re-audited; not pinned by continuation status")
    return result


def copy_verified(source, destination, *, allow_empty=False):
    expected = digest(source)
    if expected["bytes"] == 0 and not allow_empty:
        raise ValueError("Empty payload input")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(checked(source), destination)
    if digest(destination) != expected or digest(source) != expected:
        raise ValueError("Payload changed during copy")


def collect_legal(repo, local, lock, destination):
    names = pinned_names(repo, lock)
    hashed = [n for n in names if re.fullmatch(r"distribution/legal/app/rust/texts/[0-9a-f]{64}\.txt", n)]
    missing = []
    for name in [*lock["legal_files"], *hashed]:
        if local:
            path = checked(local / name)
            if not path.is_file():
                missing.append(name)
                continue
            data = path.read_bytes()
        else:
            if name not in names:
                missing.append(name)
                continue
            data = git(repo, "show", lock["commit"] + ":" + name, raw=True)
        if not data.decode("utf-8-sig").strip():
            raise ValueError("Empty legal text")
        if name in hashed and hashlib.sha256(data).hexdigest() != PurePosixPath(name).stem:
            raise ValueError("Rust license content hash mismatch")
        target = destination / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return {"status": "unreviewed", "public_approved": False, "missing_allowlisted_texts": missing,
            "reviewed_marker_present": bool(local and (local / "distribution/legal/app/REVIEWED.md").is_file()),
            "limits": "Text presence is not approval; native covered-source/provenance and package-specific reviews remain required; no private legal notes or source ZIPs included"}


def assemble(args):
    if args.mode != "evaluation":
        raise ValueError("Only explicit local evaluation is supported; use existing release gates separately")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,63}", args.name):
        raise ValueError("Unsafe evaluation name")
    output = confined(OUTPUT_ROOT / args.name)
    if output.exists():
        raise ValueError("Evaluation directory already exists; choose a new name")
    repo = checked(args.drawing_repository)
    local = checked(args.local_source) if args.local_source else None
    runtime_dir = checked(args.runtime_dir) if args.runtime_dir else None
    neo = checked(args.neo_exe)
    lock = json.loads(LOCK_PATH.read_text(encoding="utf-8"))
    source, snapshot = source_snapshot(repo, lock, local)
    runtime_dir = runtime_dir or checked(source / "target/release")
    if not local and runtime_dir != checked(source / "target/release"):
        raise ValueError("External runtime binaries require explicit --local-source (do not mislabel as pinned build)")
    directml = checked(args.directml) if args.directml else checked(runtime_dir / "DirectML.dll")
    include_resources = getattr(args, "include_local_resources", False)
    resource_plan, resource_info = ({}, {"included": False, "omissions": OMISSIONS + ["Neo speech/models/Bash/ONNX runtime/resources not included"]})
    if include_resources:
        resource_plan, resource_info = local_resource_plan(ROOT, getattr(args, "gitbash_distribution_dir", None))
    native = validate_native_receipt(getattr(args, "native_receipt", None), neo)
    output.mkdir(parents=True)
    write_json(output / "native-validation.local.json", native)
    (output / "NOT_FOR_DISTRIBUTION.txt").write_text(MARKER, encoding="utf-8")
    write_json(output / "source-before-build.local.json", snapshot)
    receipt = None
    if args.build_runtime:
        receipt = build_runtime(source, runtime_dir, snapshot, lock, output, directml)
        _, after = source_snapshot(repo, lock, local)
        if snapshot["tree_sha256"] != after["tree_sha256"]:
            raise ValueError("Sources changed during build")
    elif args.build_receipt:
        receipt = json.loads(checked(args.build_receipt).read_text(encoding="utf-8"))
        validate_receipt(receipt, snapshot, runtime_dir, directml)
    package = output / "package"
    package.mkdir()
    (package / "NOT_FOR_DISTRIBUTION.txt").write_text(MARKER + "UNREVIEWED. No release/upload permission. No clean-install claim.\n"
        "Native TTS removal: " + str(native["native_tts_removed"]) + ". Technical evidence only, NOT legal clearance.\n"
        "Do not treat this directory as approved to launch/distribute.\n", encoding="utf-8")
    copy_verified(neo, package / "neo.exe")
    for kind in ("drawing", "blackboard"):
        for name in (f"neo-{kind}.exe", "DirectML.dll"):
            copy_verified(directml if name == "DirectML.dll" else runtime_dir / name, package / "apps" / kind / name)
    for name in ("LICENSE", "NOTICE"):
        copy_verified(ROOT / name, package / name)
    legal = collect_legal(repo, local, lock, package / "docs/licenses/NeoRuntime-drawing")
    if include_resources:
        copy_local_resources(resource_plan, resource_info, package)
    write_json(package / "LOCAL-RESOURCES.json", resource_info)
    payload_scope = ("Local speech/models/language/ONNX/MinGit and selected public legal texts included; no runtime/model acceptance."
                     if include_resources else "Limited artifact set: Neo speech/models/Bash/ONNX runtime/resources are NOT included.")
    (package / "PREREQUISITES.txt").write_text(
        MARKER + lock["crt_policy"] + "\n" + lock["crt_information_url"] +
        "\nNo prerequisite was installed. DirectML remains app-local.\n" + payload_scope + "\n"
        "neo.exe/Bash/models NOT executed. Native technical status is in SOURCE.json; legal review remains unreviewed.\n"
        "Local-resource completeness is not legal, functional or clean-install approval.\n", encoding="utf-8")
    host_source = neo_source_manifest()
    if "source_association" in native:
        host_source["native_status_association"] = native["source_association"]
    native_summary = {k: v for k, v in native.items() if k != "source_association"}
    manifest = {"schema": 2, "mode": "LOCAL EVALUATION", "public_approved": False,
                "distribution_review": "unreviewed", "neo": host_source, "drawing": snapshot,
                "toolchain_lock": lock["rust"], "protocol": lock["protocol"], "legal": legal,
                "runtime_build": {"verified_against_receipt": receipt is not None,
                                  "evidence_kind": receipt.get("evidence_kind", "in-process-build") if receipt else "unverified-existing",
                                  "versions": receipt.get("versions") if receipt else None,
                                  "limitations": receipt.get("limitations", []) if receipt else [],
                                  "reproducible_build_verified": False,
                                  "source_binary_attestation": "unknown",
                                  "receipt": digest(args.build_receipt) if args.build_receipt else None},
                "native_tts_removed": native["native_tts_removed"], "native_validation": native_summary,
                "neo_exe": digest(package / "neo.exe"), "limited_artifacts": not include_resources,
                                "local_resources_included": include_resources, "payload_scope": payload_scope,
                                "omissions": resource_info["omissions"], "release_approved": False,
                                "existing_neo_gpl_risk": {"fixed": "unknown", "status": "Legal clearance unreviewed; native_tts_removed is a separate technical finding"},
                                "model_provenance": resource_info.get("model_provenance"),
                "not_tested": ["Neo GUI/native client", "capture", "microphone", "models", "clean install", "NSIS"],
                "lock": digest(LOCK_PATH)}
    write_json(package / "SOURCE.json", manifest)
    write_json(output / "dependencies.json", audit_dependencies(package))
    write_json(output / "inputs.local.json", {"neo_exe": str(neo), "runtime_dir": str(runtime_dir), "directml": str(directml), "source": str(source), "build_receipt": str(args.build_receipt) if args.build_receipt else None,
                                             "native_receipt": str(args.native_receipt) if getattr(args, "native_receipt", None) else None})
    if native["native_tts_removed"] is True and digest(package / "neo.exe") != native["evidence"]["neo_release"]:
        raise ValueError("Packaged Neo differs from validated native executable")
    files = {p.relative_to(package).as_posix(): digest(p) for p in sorted(package.rglob("*")) if p.is_file()}
    write_json(package / "FILES.sha256.json", files)
    summary = verify_file_manifest(package)
    summary.update({"local_resources_included": include_resources, "omissions": resource_info["omissions"],
                    "public_approved": False, "neo_gpl_risk_fixed": "unknown",
                    "native_tts_removed": native["native_tts_removed"], "neo_bash_models_executed": False})
    write_json(output / "payload-summary.local.json", summary)
    (output / "COMPLETE.txt").write_text("Local assembly complete; smoke is a separate command. NOT_FOR_DISTRIBUTION\n", encoding="utf-8")
    return output


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--mode", required=True, choices=["evaluation"])
    parser.add_argument("--name", required=True, help="New subdirectory of target/combined-evaluation")
    parser.add_argument("--neo-exe", required=True, type=Path, help="Existing or newly built native-agent exe; never executed")
    parser.add_argument("--native-receipt", type=Path,
                        help="Native continuation-status JSON; validate actual libraries/graph and re-audit EXE/MAP, NOT legal approval; omitted means unknown")
    parser.add_argument("--drawing-repository", type=Path, default=ROOT.parent / "NeoRuntime-drawing")
    parser.add_argument("--local-source", type=Path, help="Explicit dirty working-tree override; records complete allowlisted hashes")
    parser.add_argument("--runtime-dir", type=Path)
    parser.add_argument("--directml", type=Path, help="Explicit real DirectML.dll file if Cargo output is a symlink; never auto-follow links")
    parser.add_argument("--gitbash-distribution-dir", "--runtime-distribution", type=Path,
                        help="Explicit prepared GCM-free distribution with matching source companion; never raw cache")
    parser.add_argument("--include-local-resources", action="store_true",
                        help="Explicitly copy allowlisted local speech/language/ONNX/public docs and policy-filtered MinGit; no CI model provenance, execution or release approval")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--build-runtime", action="store_true", help="Exactly one locked offline release build, maximum 600 seconds")
    group.add_argument("--build-receipt", type=Path, help="Reuse a source/binary-bound successful build receipt")
    args = parser.parse_args(argv)
    if args.include_local_resources and args.gitbash_distribution_dir is None:
        parser.error("--include-local-resources requires --gitbash-distribution-dir; no raw cache fallback")
    if args.gitbash_distribution_dir is not None and not args.include_local_resources:
        parser.error("--gitbash-distribution-dir requires --include-local-resources")
    try:
        print(assemble(args))
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, struct.error) as exc:
        print("Local evaluation FAILED: " + str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
