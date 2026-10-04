"""Offline Eigen source companion; no network, builds, upstream script execution or approval.

Build checks the recorded local no-TTS source tree; verify checks a delivered ZIP.
Original source archives remain byte-for-byte intact. Public text allowlist/hashes
live in docs/licenses/runtime/native-sources/index.json; private evidence stays out.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import stat
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PUBLIC = ROOT / "docs/licenses/runtime/native-sources"
WORK = ROOT / "target/native-source"
LIMIT = 100 * 1024**2
MAX_FILES = 10000
SOURCES = {
    "eigen-5.0.1.tar.gz": {
        "sha256": "e9c326dc8c05cd1e044c71f30f1b2e34a6161a3b6ecf445d56b53ff1669e3dec",
        "root": "eigen-5.0.1", "files": 1913,
        "binding": "verified-local-no-TTS-source-input",
    },
    "eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip": {
        "sha256": "6a60d76351f97132669daeeb721d6bf14b008101883ad2d687a3201c5c461eb0",
        "root": "eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33", "files": 1891,
        "binding": "verified-upstream-source; inferred-ORT-default; exact-producer-binding-unconfirmed",
    },
}
MANIFEST_SHA = "042914d1d3c427986d34343e40926e782694ba5cf8f06049783b103127239d5e"
RECEIPT_SHA = "5fef57cc8591730386b76fa7d1b7c78439258066026e030459f714dc2f9c81bb"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def safe_name(name):
    p = PurePosixPath(name)
    if (not name or p.is_absolute() or "\\" in name or ":" in name
            or any(part in ("", ".", "..") for part in name.rstrip("/").split("/"))
            or any(ord(c) < 32 for c in name)):
        raise ValueError(f"Unsafe member: {name!r}")
    return p


def archive_inventory(data, filename, expected_sha, root, max_bytes=LIMIT):
    """Inspect without extraction; reject links, duplicates, traversal and bombs."""
    if len(data) > max_bytes or sha(data) != expected_sha:
        raise ValueError(f"Source size/hash mismatch: {filename}")
    result, seen = {}, set()
    total = 0

    def add(name, size, directory, regular, read):
        nonlocal total
        p = safe_name(name)
        key = p.as_posix().casefold()
        if p.parts[0] != root or key in seen or not regular:
            raise ValueError(f"Wrong root, duplicate or special member: {name}")
        seen.add(key)
        if len(seen) > MAX_FILES:
            raise ValueError("Archive member budget exceeded")
        if directory:
            return
        if len(p.parts) < 2:
            raise ValueError("File at archive root")
        total += size
        if size < 0 or total > max_bytes:
            raise ValueError("Expanded source budget exceeded")
        content = read()
        if len(content) != size:
            raise ValueError("Truncated archive member")
        result[PurePosixPath(*p.parts[1:]).as_posix()] = {
            "size": size, "sha256": sha(content),
        }

    if filename.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            for member in archive.infolist():
                mode = stat.S_IFMT(member.external_attr >> 16)
                add(member.filename, member.file_size, member.is_dir(),
                    mode in (0, stat.S_IFREG, stat.S_IFDIR),
                    lambda m=member: archive.read(m))
    else:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            for member in archive:
                add(member.name, member.size, member.isdir(),
                    member.isfile() or member.isdir(),
                    lambda m=member: archive.extractfile(m).read())
    if not result:
        raise ValueError("Empty source archive")
    return result


def compare_tree(tree, inventory):
    actual = {}
    for path in tree.rglob("*"):
        if path.is_symlink() or path.is_junction():
            raise ValueError(f"Source link: {path}")
        if path.is_file():
            actual[path.relative_to(tree).as_posix()] = {
                "size": path.stat().st_size, "sha256": digest(path),
            }
    if actual != inventory:
        raise ValueError("Local source differs from complete pinned archive")


def check_native(native, inventory):
    manifest_path = native / "install/lib/neo-sherpa-asr.json"
    receipt_path = native / "install/lib/neo-asr-receipt.json"
    if digest(manifest_path) != MANIFEST_SHA or digest(receipt_path) != RECEIPT_SHA:
        raise ValueError("Not the recorded no-TTS build; review a new build explicitly")
    receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    if receipt["dependency_archives"]["eigen"]["sha256"] != SOURCES["eigen-5.0.1.tar.gz"]["sha256"]:
        raise ValueError("Eigen receipt mismatch")
    if receipt["options"]["SHERPA_ONNX_ENABLE_TTS"] != "OFF":
        raise ValueError("Not a no-TTS receipt")
    for stage in ("configure", "build", "install"):
        command = receipt["commands"][stage + ".command.json"]
        if command.get("returncode") != 0 or command.get("timed_out"):
            raise ValueError("Unsuccessful build stage")
    if digest(native / "graph.json") != receipt["graph"]["sha256"]:
        raise ValueError("Native graph changed")
    compare_tree(native / "deps/eigen/eigen-5.0.1", inventory)
    return {"manifest_sha256": MANIFEST_SHA, "receipt_sha256": RECEIPT_SHA,
            "eigen_files_compared": len(inventory),
            "limit": "Current source equality and recorded build inputs, not a signed historical attestation"}


def public_texts(public=PUBLIC):
    index_bytes = (public / "index.json").read_bytes()
    index = json.loads(index_bytes)
    result = {"notices/index.json": index_bytes}
    for name, expected in index["texts"].items():
        p = safe_name(name)
        path = public.joinpath(*p.parts)
        if path.is_symlink() or not path.resolve().is_relative_to(public.resolve()):
            raise ValueError("Public text escapes directory")
        if path.stat().st_size > LIMIT:
            raise ValueError("Text budget exceeded")
        data = path.read_bytes()
        if {"size": len(data), "sha256": sha(data)} != expected:
            raise ValueError(f"Public text changed: {name}")
        data.decode("utf-8")
        result["notices/" + name] = data
    if sum(map(len, result.values())) > LIMIT:
        raise ValueError("Text budget exceeded")
    return result


def write_bundle(output, payload):
    if output.exists():
        raise ValueError("Refusing to overwrite companion")
    if sum(map(len, payload.values())) > LIMIT:
        raise ValueError("Companion budget exceeded")
    payload = dict(payload)
    payload["SHA256SUMS"] = "".join(
        f"{sha(data)}  {name}\n" for name, data in sorted(payload.items())
    ).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    # Stored original archives avoid needless recompression and retain upstream bytes.
    with zipfile.ZipFile(output, "x", compression=zipfile.ZIP_STORED) as archive:
        for name, data in sorted(payload.items()):
            safe_name(name)
            info = zipfile.ZipInfo(name, date_time=(2026, 10, 4, 0, 0, 0))
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            archive.writestr(info, data)


def build(eigen, ort_eigen, native, output, public=PUBLIC):
    payload = public_texts(public)
    inventories = {}
    for path, (filename, spec) in zip((eigen, ort_eigen), SOURCES.items()):
        if path.stat().st_size > LIMIT:
            raise ValueError("Source budget exceeded")
        data = path.read_bytes()
        inventory = archive_inventory(data, filename, spec["sha256"], spec["root"])
        if len(inventory) != spec["files"]:
            raise ValueError("Source inventory count mismatch")
        inventories[filename] = inventory
        payload["sources/" + filename] = data
    native_check = check_native(native, inventories["eigen-5.0.1.tar.gz"])
    payload["source-records.json"] = (json.dumps({
        "schema": 1, "sources": SOURCES, "no_tts_check": native_check,
        "publication_performed": False,
        "scope": "Source supply only; not approval or proof of ORT producer source correspondence",
    }, indent=2) + "\n").encode()
    write_bundle(output, payload)
    return verify(output, public)


def verify(path, public=PUBLIC):
    if path.stat().st_size > LIMIT:
        raise ValueError("Companion exceeds budget")
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        names = [i.filename for i in infos]
        if (len(names) > MAX_FILES or len(set(n.casefold() for n in names)) != len(names)
                or sum(i.file_size for i in infos) > LIMIT):
            raise ValueError("Duplicate member or expanded companion budget exceeded")
        for i in infos:
            safe_name(i.filename)
            if stat.S_IFMT(i.external_attr >> 16) not in (0, stat.S_IFREG):
                raise ValueError("Non-regular companion member")
        payload = {i.filename: archive.read(i) for i in infos}
    expected_texts = public_texts(public)
    expected_names = set(expected_texts) | {"sources/" + n for n in SOURCES} | {"SHA256SUMS", "source-records.json"}
    if set(payload) != expected_names:
        raise ValueError("Missing or unexpected companion content")
    sums = "".join(f"{sha(data)}  {name}\n" for name, data in sorted(payload.items()) if name != "SHA256SUMS").encode()
    if payload["SHA256SUMS"] != sums:
        raise ValueError("Companion checksum mismatch")
    for name, data in expected_texts.items():
        if payload[name] != data:
            raise ValueError("Public notice mismatch")
    for name, spec in SOURCES.items():
        inventory = archive_inventory(payload["sources/" + name], name, spec["sha256"], spec["root"])
        if len(inventory) != spec["files"]:
            raise ValueError("Source count mismatch")
    record = json.loads(payload["source-records.json"])
    if (record["sources"] != SOURCES or record["publication_performed"] is not False
            or record["no_tts_check"]["manifest_sha256"] != MANIFEST_SHA
            or record["no_tts_check"]["receipt_sha256"] != RECEIPT_SHA
            or record["no_tts_check"]["eigen_files_compared"] != 1913):
        raise ValueError("Source scope or evidence mismatch")
    return {"sha256": digest(path), "size": path.stat().st_size,
            "members": len(payload), "status": "integrity-verified-not-release-approval"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "verify"))
    parser.add_argument("--bundle", type=Path, default=WORK / "neo-native-sources.zip")
    parser.add_argument("--eigen", type=Path, default=ROOT / ".cache/sherpa-asr/eigen-5.0.1.tar.gz")
    parser.add_argument("--ort-eigen", type=Path, default=ROOT / ".cache/native-corresponding-source/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip")
    parser.add_argument("--native", type=Path, default=ROOT / "target/sherpa-asr/native")
    args = parser.parse_args()
    try:
        if args.command == "build":
            if not args.bundle.resolve().is_relative_to(WORK.resolve()):
                raise ValueError("Build output must stay in target/native-source")
            result = build(args.eigen, args.ort_eigen, args.native, args.bundle)
        else:
            result = verify(args.bundle)
        print(json.dumps(result, indent=2))
    except (ValueError, OSError, KeyError, tarfile.TarError, zipfile.BadZipFile) as exc:
        parser.exit(1, f"native source companion: {exc}\n")


if __name__ == "__main__":
    main()
