"""Collect/recheck MSVC Redist evidence, NOT redistribution approval.

Offline, read-only inputs; never execute a CRT, inspect accounts, or copy DLLs.
The only output is an explicitly requested, new JSON manifest. See
 docs-pri/licenses/msvc-remediation.md for scope and CLI examples.
"""
import argparse
import ctypes
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys

if __package__:
    from .check_release import CRT_FAMILY, CRT_NAMES, validate_redist_dir
else:
    from check_release import CRT_FAMILY, CRT_NAMES, validate_redist_dir

SCHEMA = 1
DISCLAIMER = "Evidence consistency only; redistribution authorization NOT verified"
FORBIDDEN = {"system32", "syswow64", "winsxs", "onecore", "debug_nonredist"}
PRODUCTS = {"Microsoft.VisualStudio.Product." + name for name in
            ("BuildTools", "Enterprise", "Professional", "Community")}


def plain_path(path, directory=False):
    path = Path(os.path.abspath(path))
    if FORBIDDEN.intersection(part.lower() for part in path.parts):
        raise ValueError(f"Forbidden system/non-release source: {path}")
    # Reject junctions as well as symlinks, including ancestors, before resolving.
    for part in (path, *path.parents):
        st = part.lstat()
        if part.is_symlink() or getattr(st, "st_file_attributes", 0) & 0x400:
            raise ValueError(f"Symlink/reparse source is not evidence: {part}")
    if directory:
        if not path.is_dir():
            raise ValueError(f"Expected directory: {path}")
    elif not path.is_file() or path.stat().st_size == 0 or path.stat().st_nlink != 1:
        raise ValueError(f"Expected nonempty, non-hardlinked regular file: {path}")
    return path.resolve(strict=True)


def fingerprint(path):
    path = plain_path(path)
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "size": path.stat().st_size, "sha256": digest}


def read_json(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def pe_versions(path):
    """Read fixed PE file/product versions via Windows resources, not LoadLibrary(path)."""
    with Path(path).open("rb") as stream:
        if stream.read(2) != b"MZ":
            raise ValueError(f"Not a PE: {path}")
        stream.seek(0x3c)
        offset = stream.read(4)
        if len(offset) != 4:
            raise ValueError(f"Truncated PE: {path}")
        stream.seek(struct.unpack("<I", offset)[0])
        if stream.read(6) != b"PE\0\0\x64\x86":
            raise ValueError(f"Not a native x64 PE: {path}")
    if os.name != "nt":
        raise ValueError("Real file-version collection requires Windows")
    from ctypes import wintypes as w
    api = ctypes.WinDLL("version.dll", winmode=0x800, use_last_error=True)
    api.GetFileVersionInfoSizeW.argtypes = [w.LPCWSTR, ctypes.POINTER(w.DWORD)]
    api.GetFileVersionInfoSizeW.restype = w.DWORD
    api.GetFileVersionInfoW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, w.LPVOID]
    api.GetFileVersionInfoW.restype = w.BOOL
    api.VerQueryValueW.argtypes = [w.LPCVOID, w.LPCWSTR,
                                  ctypes.POINTER(w.LPVOID), ctypes.POINTER(w.UINT)]
    api.VerQueryValueW.restype = w.BOOL
    size = api.GetFileVersionInfoSizeW(str(path), None)
    if not size or size > 16 * 1024 * 1024:
        raise ValueError(f"Missing/invalid version resource: {path}")
    buffer = ctypes.create_string_buffer(size)
    pointer, length = w.LPVOID(), w.UINT()
    if (not api.GetFileVersionInfoW(str(path), 0, size, buffer)
            or not api.VerQueryValueW(buffer, "\\", ctypes.byref(pointer), ctypes.byref(length))
            or length.value < 52):
        raise ValueError(f"Cannot read fixed version resource: {path}")
    values = struct.unpack("<13I", ctypes.string_at(pointer, 52))
    if values[0] != 0xFEEF04BD:
        raise ValueError(f"Invalid fixed version signature: {path}")
    def version(ms, ls):
        return ".".join(str(n) for n in (ms >> 16, ms & 65535, ls >> 16, ls & 65535))
    # VS_FF_DEBUG, PRERELEASE, PRIVATEBUILD, SPECIALBUILD are not accepted.
    if values[7] & values[6] & (0x1 | 0x2 | 0x8 | 0x20):
        raise ValueError(f"Debug/preview/private/special CRT build: {path}")
    return {"machine": "x64", "file_version": version(values[2], values[3]),
            "product_version": version(values[4], values[5]),
            "version_flags": values[7] & values[6]}


def installation(instance_dir):
    instance = plain_path(instance_dir, directory=True)
    state_path, catalog_path = instance / "state.json", instance / "catalog.json"
    state_fp, catalog_fp = fingerprint(state_path), fingerprint(catalog_path)
    state, catalog = read_json(state_path), read_json(catalog_path)
    product = state.get("product", {})
    product_id, version = product.get("id"), state.get("installationVersion")
    if product_id not in PRODUCTS or not isinstance(version, str) or not re.fullmatch(r"\d+(?:\.\d+){3}", version):
        raise ValueError("Missing/unsupported exact VS product identity/version")
    matches = [p for p in catalog.get("packages", []) if p.get("id") == product_id]
    if len(matches) != 1 or matches[0].get("version") != version or product.get("version") != version:
        raise ValueError("State/catalog product version mismatch")
    info = catalog.get("info", {})
    if info.get("buildVersion") != version or info.get("productMilestoneIsPreRelease") != "False":
        raise ValueError("Catalog is mismatched, preview, or has unknown release status")
    resources = matches[0].get("localizedResources", [])
    english = [r for r in resources if r.get("language", "").lower() == "en-us"]
    refs = sorted({r["license"] for r in resources if r.get("license")})
    if len(english) != 1 or not english[0].get("title") or not refs:
        raise ValueError("Missing product title or catalog license reference")
    if any(not isinstance(ref, str) or not ref.startswith("https://") for ref in refs):
        raise ValueError("Expected HTTPS catalog license references")
    root = plain_path(state["installationPath"], directory=True)
    return {"installation_path": str(root), "instance_dir": str(instance),
            "product_id": product_id, "product_title": english[0]["title"],
            "installation_version": version, "display_version": info.get("productDisplayVersion"),
            "channel_id": state.get("channelId"), "product_installed": product.get("installed"),
            "setup_canceled": state.get("properties", {}).get("canceled"),
            "catalog_license_refs": refs, "state": state_fp, "catalog": catalog_fp}


def collect(redist_dir, instance_dir, license_policy_ref):
    identity = installation(instance_dir)
    root = Path(identity["installation_path"])
    redist = validate_redist_dir(plain_path(redist_dir, directory=True))
    try:
        relative = redist.relative_to(root)
    except ValueError as error:
        raise ValueError("Redist is outside the recorded VS installation") from error
    parts = relative.parts
    if (len(parts) != 6 or [s.lower() for s in parts[:3]] != ["vc", "redist", "msvc"]
            or not re.fullmatch(r"\d+(?:\.\d+){2,3}", parts[3])):
        raise ValueError("Expected exact installation/VC/Redist/MSVC/version/x64/CRT source")
    policy = fingerprint(license_policy_ref)
    files = []
    excluded = []
    seen = set()
    for path in sorted(redist.iterdir(), key=lambda p: p.name.lower()):
        name = path.name.lower()
        if name not in CRT_NAMES:
            excluded.append(path.name)
            continue
        if name in seen:
            raise ValueError(f"Duplicate case-insensitive CRT name: {name}")
        seen.add(name)
        fp = fingerprint(path)
        versions = pe_versions(path)
        if fingerprint(path) != fp:
            raise ValueError(f"CRT changed during collection: {path}")
        files.append({"name": name, "source": fp, **versions})
    if not files:
        raise ValueError("No allowlisted CRT DLLs in Redist directory")
    warnings = []
    if identity["product_installed"] is not True:
        warnings.append("Installer does not affirm product.installed=true; installation completeness unresolved")
    if identity["setup_canceled"] not in (None, "0", 0, False):
        warnings.append("Installer records a canceled operation; review installation completeness")
    return {"schema": SCHEMA, "result": DISCLAIMER,
            "authorization": "not_assessed", "installation": identity,
            "redist_dir": str(redist), "redist_version_directory": parts[3],
            "license_policy_ref": policy, "files": files,
            "excluded_directory_entries": excluded, "warnings": warnings}


def check_package(package, evidence):
    package = plain_path(package, directory=True)
    sources = {f["name"]: f for f in evidence["files"]}
    found = []
    for path in sorted(package.iterdir(), key=lambda p: p.name.lower()):
        name = path.name.lower()
        if not CRT_FAMILY.fullmatch(name):
            continue
        if name not in sources:
            raise ValueError(f"Packaged CRT has no allowlisted source evidence: {path}")
        if name in found:
            raise ValueError(f"Duplicate packaged CRT: {name}")
        fp = fingerprint(path)
        source = sources[name]["source"]
        if any(fp[k] != source[k] for k in ("size", "sha256")):
            raise ValueError(f"Packaged CRT differs from exact Redist source: {path}")
        found.append(name)
    if not found:
        raise ValueError("No app-local CRT to compare; this is not a no-CRT dependency audit")
    return found


def verify(manifest, package=None):
    saved = read_json(manifest)
    if saved.get("schema") != SCHEMA:
        raise ValueError("Unsupported evidence manifest schema")
    current = collect(saved["redist_dir"], saved["installation"]["instance_dir"],
                      saved["license_policy_ref"]["path"])
    expected = dict(saved)
    expected.pop("collected_at_utc", None)
    if expected != current:
        raise ValueError("Evidence changed or manifest modified; recollect and review (never auto-approve)")
    return check_package(package, current) if package else []


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--manifest", type=Path, help="Write NEW evidence-only manifest; never overwrite")
    mode.add_argument("--verify", type=Path, help="Recheck manifest against the same installed source")
    parser.add_argument("--redist-dir", type=Path)
    parser.add_argument("--instance-dir", type=Path, help="Explicit VS Installer _Instances/<id> metadata directory")
    parser.add_argument("--license-policy-ref", type=Path, help="Nonempty local scope/review document to hash; NOT an approval")
    parser.add_argument("--package", type=Path, help="With --verify, compare root app-local CRT subset to source hashes")
    args = parser.parse_args(argv)
    if args.manifest and (not all((args.redist_dir, args.instance_dir, args.license_policy_ref)) or args.package):
        parser.error("Collection requires --redist-dir, --instance-dir, --license-policy-ref; --package is verify-only")
    if args.verify and any((args.redist_dir, args.instance_dir, args.license_policy_ref)):
        parser.error("Verification uses exact paths recorded in the manifest")
    try:
        if args.manifest:
            result = collect(args.redist_dir, args.instance_dir, args.license_policy_ref)
            result["collected_at_utc"] = datetime.now(timezone.utc).isoformat()
            with args.manifest.open("x", encoding="utf-8", newline="\n") as stream:
                json.dump(result, stream, ensure_ascii=True, indent=2)
                stream.write("\n")
            for warning in result["warnings"]:
                print("WARNING:", warning)
            print(f"Recorded {len(result['files'])} source DLLs: {args.manifest}")
        else:
            found = verify(args.verify, args.package)
            if args.package:
                print("Matching app-local CRT:", ", ".join(found))
        print(DISCLAIMER)
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Redist evidence FAILED: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
