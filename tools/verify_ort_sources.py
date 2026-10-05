"""Offline ORT/Eigen public notice and source-delivery integrity checks.

Schema 1: components have id, artifact_sha256, eigen_archive, eigen_sha256,
patches [{path, sha256, member}]; drawing additionally has drawing_commit.
Patches lists the supplied baseline patches, not an exact producer attestation;
in particular drawing's empty list must not be read as proof of no modifications.
artifact_sha256 identifies actual wake DLL bytes, the build.DEPS static ORT
archive pin, or the fixed drawing distribution archive pin, respectively.
source_delivery has kind, url, size, sha256, eigen_member. The required
final_binary_identity_verified=false prevents this integrity check from being
mistaken for a final EXE/static-library or producer-source attestation.

Default checks tracked public files, local wake DLLs and current builder/lock
pins. --package additionally checks delivered notices, DLLs and drawing assembly
provenance. --native-bundle checks original Eigen and patch members without
extracting or changing the historical ZIP. No network or sibling checkout use.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import stat
import subprocess
import zipfile
from urllib.parse import urlsplit

if __package__:
    from . import build_sherpa_asr as build
    from . import native_source_bundle as native
    from .runtime_sources import check_path, file_record, hash_stream, safe_relative
else:
    import build_sherpa_asr as build
    import native_source_bundle as native
    from runtime_sources import check_path, file_record, hash_stream, safe_relative

ROOT = Path(__file__).resolve().parents[1]
NOTICE = "docs/licenses/runtime/ORT-EIGEN-SOURCE.txt"
RECORD = "docs/licenses/runtime/ort-eigen-correspondence.json"
LOCK = "tools/source-companions.lock.json"
DRAWING_LOCK = "tools/drawing-runtime.lock.json"
DRAWING_SOURCE = "docs/licenses/NeoRuntime-drawing/SOURCE.json"
DRAWING_COMMIT = "ea1ecc87ec97717117f03625fd958c75cc2c0a49"
# Fixed distribution identified in the existing public native-source README.
# Package mode also compares the assembler's pinned dist.txt provenance.
DRAWING_ORT_SHA256 = "540d19b3379fda6fb8f7280d8c15efde20ed225a67a357a6dae38c4300fe190d"
EIGEN = "eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip"
PATCHES = ("ort-1.28.2-eigen-s390x-build.patch", "ort-1.28.2-eigen-s390x-build-werror.patch")
PUBLIC_NATIVE = "docs/licenses/runtime/native-sources"
COPYING = ("APACHE", "BSD", "MINPACK", "MPL2", "README")
EIGEN_TEXTS = tuple(f"eigen-{version}-COPYING.{suffix}" for version in ("1d8b82b", "5.0.1") for suffix in COPYING) + (
    "eigen-5.0.1-LICENSE", "eigen-5.0.1-CopyrightMINPACK.txt")
STATIC_ORT_TEXTS = ("onnxruntime-1.28.2-LICENSE", "onnxruntime-1.28.2-ThirdPartyNotices.txt")
MAIN_ORT_TEXTS = ("docs/licenses/runtime/onnxruntime-LICENSE", "docs/licenses/runtime/onnxruntime-ThirdPartyNotices.txt")
DRAWING_LEGAL_ROOT = "docs/licenses/NeoRuntime-drawing/"
DRAWING_NOTICES = tuple("distribution/legal/app/native/" + name for name in (
    "onnxruntime/LICENSE", "onnxruntime/ThirdPartyNotices.txt",
    "ort-sys/LICENSE-APACHE", "ort-sys/LICENSE-MIT",
    "directml/LICENSE-CODE.txt", "directml/LICENSE.txt", "directml/ThirdPartyNotices.txt"))
WAKE = {"wake-onnxruntime": "onnxruntime.dll",
        "wake-onnxruntime-providers-shared": "onnxruntime_providers_shared.dll"}
IDS = {*WAKE, "sherpa-static-onnxruntime", "drawing-static-onnxruntime"}
MAX_JSON = 2 * 1024**2


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "Invalid SHA-256 pin")


def relative_file(root, name):
    path = root / safe_relative(name)
    check_path(path)
    require(path.resolve().is_relative_to(root.resolve()), "Path escapes root: " + name)
    require(path.is_file(), "Missing regular file: " + name)
    info = path.stat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1, "Linked/non-regular file: " + name)
    return path


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "Duplicate JSON key: " + key)
        result[key] = value
    return result


def read_json(path):
    check_path(path)
    require(path.stat().st_size <= MAX_JSON, "JSON budget exceeded")
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object)


def tracked(root, names):
    """Require exact index paths, not glob/pathspec matches or sibling files."""
    result = subprocess.run(["git", "--no-pager", "ls-files", "-z"], cwd=root,
                            capture_output=True, check=True, timeout=15)
    paths = set(result.stdout.decode("utf-8").split("\0"))
    require(set(names) <= paths, "Public ORT source files must be tracked at exact root paths")


def check_lock_entry(lock, *, required=False):
    if "ort_eigen_notice" not in lock:
        require(not required, "Missing ort_eigen_notice lock entry")
        return None
    entry = lock["ort_eigen_notice"]
    require(isinstance(entry, dict) and set(entry) == {"path", "sha256", "record_path", "record_sha256"},
            "Incomplete/unknown ort_eigen_notice lock fields")
    require(entry["path"] == NOTICE and entry["record_path"] == RECORD,
            "ORT notice/record must use exact public root paths")
    sha(entry["sha256"])
    sha(entry["record_sha256"])
    return entry


def check_record(record, lock, root):
    require(isinstance(record, dict) and type(record.get("schema")) is int and record["schema"] == 1,
            "Unsupported ORT correspondence schema")
    require(record.get("final_binary_identity_verified") is False,
            "ORT notice check cannot attest final binary identity")
    components = record.get("components")
    require(isinstance(components, list) and len(components) == len(IDS)
            and all(isinstance(c, dict) for c in components)
            and {c.get("id") for c in components} == IDS, "Missing/duplicate/unknown ORT components")
    delivery = record.get("source_delivery")
    require(isinstance(delivery, dict) and set(delivery) == {"kind", "url", "size", "sha256", "eigen_member"},
            "Invalid source_delivery fields")
    require(delivery["kind"] == "native", "ORT source delivery must be native")
    sha(delivery["sha256"])
    require(type(delivery["size"]) is int and 0 < delivery["size"] <= native.LIMIT, "Invalid source_delivery size")
    url = delivery["url"]
    require(isinstance(url, str) and not any(c.isspace() for c in url), "Invalid source_delivery URL")
    parsed = urlsplit(url)
    require(parsed.scheme == "https" and parsed.hostname and not parsed.username and not parsed.password
            and not parsed.fragment, "Expected fixed HTTPS source_delivery URL")
    sources = lock.get("sources", [])
    require(isinstance(sources, list), "Invalid source lock")
    sources = [s for s in sources if s.get("kind") == "native"]
    require(len(sources) == 1, "Expected exactly one native source lock")
    source = sources[0]
    require({"url": source.get("download_url"), "size": source.get("size"), "sha256": source.get("sha256")}
            == {k: delivery[k] for k in ("url", "size", "sha256")}, "source_delivery differs from source lock")
    require(delivery["eigen_member"] == "sources/" + EIGEN, "Wrong Eigen source member")
    index = read_json(relative_file(root, PUBLIC_NATIVE + "/index.json"))
    drawing = read_json(relative_file(root, DRAWING_LOCK))
    require(drawing.get("commit") == DRAWING_COMMIT, "Drawing pin changed; review ORT distribution binding")
    for component in components:
        sha(component.get("artifact_sha256"))
        require(component.get("eigen_archive") == EIGEN
                and component.get("eigen_sha256") == native.SOURCES[EIGEN]["sha256"], "Eigen archive/hash mismatch")
        expected = [{"path": PUBLIC_NATIVE + "/" + name,
                     "sha256": index["texts"][name]["sha256"], "member": "notices/" + name}
                    for name in PATCHES] if component["id"] == "sherpa-static-onnxruntime" else []
        require(component.get("patches") == expected, "Wrong/missing ORT Eigen patches: " + component["id"])
        if component["id"] == "sherpa-static-onnxruntime":
            require(component["artifact_sha256"] == build.DEPS["onnxruntime"][1], "Static ORT build.DEPS archive hash mismatch")
        if component["id"] == "drawing-static-onnxruntime":
            require(component.get("drawing_commit") == drawing["commit"]
                    and component["artifact_sha256"] == DRAWING_ORT_SHA256, "Drawing ORT commit/archive hash mismatch")
    return components


def verify_public_texts(root, package):
    index = read_json(relative_file(root, PUBLIC_NATIVE + "/index.json"))
    for name in (*EIGEN_TEXTS, *STATIC_ORT_TEXTS):
        path = PUBLIC_NATIVE + "/" + name
        expected = index["texts"].get(name)
        require(isinstance(expected, dict) and set(expected) == {"size", "sha256"}, "Missing indexed license text: " + name)
        sha(expected["sha256"])
        require(type(expected["size"]) is int and expected["size"] > 0, "Empty indexed license text: " + name)
        require(file_record(relative_file(root, path)) == expected, "Indexed license text mismatch: " + name)
        if package is not None:
            require(file_record(relative_file(package, path)) == expected, "Package license text mismatch: " + name)
    for name in MAIN_ORT_TEXTS:
        expected = file_record(relative_file(root, name))
        require(expected["size"] > 0, "Empty main ORT notice: " + name)
        if package is not None:
            require(file_record(relative_file(package, name)) == expected, "Package main ORT notice mismatch: " + name)


def verify_drawing_notices(package, drawing, provenance):
    # The assembler verifies these hashes against the pinned Git tree. Reuse its
    # technical source binding, not a package-local manifest that can bless itself.
    binding = drawing.get("source_binding", {}).get("legal_files")
    require(isinstance(binding, dict) and provenance.get("legal_files") == binding,
            "Package drawing legal manifest differs from source binding")
    allowlist = drawing.get("legal_files", [])
    require(set(DRAWING_NOTICES) <= set(allowlist), "Drawing lock omits required native notices")
    names = set(DRAWING_NOTICES) | {name for name in allowlist if name.startswith("distribution/legal/app/native/")}
    for name in sorted(names):
        expected = binding.get(name)
        sha(expected)
        actual = file_record(relative_file(package, DRAWING_LEGAL_ROOT + safe_relative(name)))
        require(actual["size"] > 0 and actual["sha256"] == expected, "Package drawing native notice mismatch: " + name)


def verify_bundle(bundle, record):
    delivery = record["source_delivery"]
    check_path(bundle)
    require(bundle.stat().st_size <= native.LIMIT, "Native ZIP budget exceeded")
    require(file_record(bundle) == {k: delivery[k] for k in ("size", "sha256")}, "Native ZIP size/hash mismatch")
    members = {delivery["eigen_member"]: native.SOURCES[EIGEN]["sha256"]}
    for component in record["components"]:
        for patch in component["patches"]:
            members[patch["member"]] = patch["sha256"]
    with zipfile.ZipFile(bundle) as archive:
        infos = archive.infolist()
        require(len(infos) <= native.MAX_FILES and sum(i.file_size for i in infos) <= native.LIMIT,
                "Native ZIP expanded/member budget exceeded")
        seen = set()
        for info in infos:
            name = safe_relative(info.orig_filename)
            require(name == info.filename, "Normalized native ZIP member alias")
            require(name.casefold() not in seen, "Duplicate native ZIP member")
            seen.add(name.casefold())
            require(stat.S_IFMT(info.external_attr >> 16) in (0, stat.S_IFREG)
                    and not info.is_dir() and not info.flag_bits & 1, "Non-regular/encrypted native ZIP member")
            require(Path(name).name.casefold() not in {Path(NOTICE).name.casefold(), Path(RECORD).name.casefold()},
                    "New ORT notice/record must remain outside historical native ZIP")
        for name, digest in members.items():
            require(name in archive.namelist(), "Missing source member: " + name)
            with archive.open(name) as stream:
                actual, _ = hash_stream(stream, native.LIMIT)
            require(actual == digest, "Source member hash mismatch: " + name)


def verify(lock, *, root=ROOT, package=None, bundle=None, required=True):
    entry = check_lock_entry(lock, required=required)
    if entry is None:
        return None
    names = [NOTICE, RECORD, PUBLIC_NATIVE + "/index.json", DRAWING_LOCK,
             *(PUBLIC_NATIVE + "/" + name for name in (*PATCHES, *EIGEN_TEXTS, *STATIC_ORT_TEXTS)), *MAIN_ORT_TEXTS]
    tracked(root, names)
    for name, key in ((NOTICE, "sha256"), (RECORD, "record_sha256")):
        source = relative_file(root, name)
        expected = file_record(source)
        require(expected["size"] > 0 and expected["sha256"] == entry[key], "Public ORT notice/record hash mismatch: " + name)
        if package is not None:
            require(file_record(relative_file(package, name)) == expected, "Package ORT notice/record differs from tracked root: " + name)
    record = read_json(relative_file(root, RECORD))
    components = check_record(record, lock, root)
    verify_public_texts(root, package)
    for component in components:
        if component["id"] in WAKE:
            name = WAKE[component["id"]]
            require(file_record(relative_file(root, "crates/neo-wake/assets/" + name))["sha256"] == component["artifact_sha256"],
                    "Local wake DLL hash mismatch: " + name)
            if package is not None:
                require(file_record(relative_file(package, "runtime/onnx/" + name))["sha256"] == component["artifact_sha256"],
                        "Package wake DLL hash mismatch: " + name)
        for patch in component["patches"]:
            expected = file_record(relative_file(root, patch["path"]))
            require(expected["sha256"] == patch["sha256"], "Public patch hash mismatch")
            if package is not None:
                require(file_record(relative_file(package, patch["path"])) == expected, "Package patch hash mismatch")
    if package is not None:
        drawing = read_json(relative_file(root, DRAWING_LOCK))
        provenance = read_json(relative_file(package, DRAWING_SOURCE))
        require(provenance.get("source_commit") == drawing["commit"]
                and provenance.get("repository") == drawing["repository"]
                and provenance.get("lock_sha256") == file_record(relative_file(root, DRAWING_LOCK))["sha256"],
                "Package drawing provenance differs from current lock")
        require(provenance.get("directml_provenance", {}).get("ort_archive_sha256", "").lower() == DRAWING_ORT_SHA256,
                "Package drawing ORT archive provenance mismatch")
        verify_drawing_notices(package, drawing, provenance)
    if bundle is not None:
        verify_bundle(bundle, record)
    return {"status": "integrity-verified-not-release-approval", "components": sorted(IDS),
            "package_checked": package is not None, "native_zip_checked": bundle is not None,
            "final_binary_identity_verified": False,
            "limitations": "No final EXE/static-library identity or producer-source attestation; drawing default checks pins only"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lock", type=Path, default=ROOT / LOCK)
    parser.add_argument("--package", type=Path)
    parser.add_argument("--native-bundle", type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(verify(read_json(args.lock), package=args.package, bundle=args.native_bundle), indent=2))
    except (ValueError, OSError, KeyError, TypeError, AttributeError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        parser.exit(1, f"ORT source verification BLOCKED: {error}\n")


if __name__ == "__main__":
    main()
