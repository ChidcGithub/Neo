"""Prepare mandatory no-TTS native inputs before Cargo, without changing PATH.

Fresh CI builds locally; only pinned download archives may be cached. Existing
validated local installs are checked, never rewritten. Failed attempts require
manual review, not an automatic rebuild/validation retry. No Neo executable runs.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import zipfile

if __package__:
    from . import build_sherpa_asr as build
else:
    import build_sherpa_asr as build

CMAKE_VERSION = "4.2.3"
CMAKE_WHEEL = f"cmake-{CMAKE_VERSION}-py3-none-win_amd64.whl"
CMAKE_SHA256 = "0c55af0e1b2db232a94a7c34e89f25f3dbf410a4669b11134d07de0bd7aad03e"
GENERATORS = {17: "Visual Studio 17 2022", 18: "Visual Studio 18 2026"}


def capture(command):
    result = subprocess.run(command, cwd=build.ROOT, check=True, capture_output=True,
                            encoding="utf-8", timeout=15)
    return result.stdout


def discover_vs(vswhere=None):
    if vswhere is None:
        vswhere = shutil.which("vswhere")
        if not vswhere:
            base = os.environ.get("ProgramFiles(x86)")
            if not base:
                raise ValueError("Cannot locate vswhere; use --vswhere")
            vswhere = str(Path(base) / "Microsoft Visual Studio/Installer/vswhere.exe")
    instances = json.loads(capture([
        str(vswhere), "-latest", "-products", "*", "-requires",
        "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-format", "json", "-utf8",
    ]))
    if len(instances) != 1:
        raise ValueError("vswhere must return one C++ Visual Studio installation")
    instance = instances[0]
    version = instance["installationVersion"]
    major = int(version.split(".")[0])
    if major not in GENERATORS:
        raise ValueError(f"Unsupported Visual Studio version: {version}")
    root = Path(instance["installationPath"])
    if not root.is_absolute():
        raise ValueError("vswhere installationPath must be absolute")
    toolset = (root / "VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt").read_text().strip()
    if not toolset or any(c not in "0123456789." for c in toolset):
        raise ValueError("Invalid default MSVC toolset version")
    dumpbin = root / "VC/Tools/MSVC" / toolset / "bin/Hostx64/x64/dumpbin.exe"
    if not dumpbin.is_file():
        raise ValueError(f"Missing x64 dumpbin: {dumpbin}")
    return GENERATORS[major], f"{root},version={version}", str(dumpbin)


def prepare_cmake():
    cache = build.ROOT / ".cache/tools"
    cache.mkdir(parents=True, exist_ok=True)
    wheel = cache / CMAKE_WHEEL
    if not wheel.exists():
        requirements = build.WORK / "ci-cmake-requirements.txt"
        requirements.write_text(f"cmake=={CMAKE_VERSION} --hash=sha256:{CMAKE_SHA256}\n", encoding="utf-8")
        build.run([
            sys.executable, "-m", "pip", "--isolated", "--no-input", "download", "--disable-pip-version-check",
                        "--retries", "0", "--timeout", "30",
            "--no-cache-dir", "--no-deps", "--only-binary=:all:", "--require-hashes",
            "--index-url", "https://pypi.org/simple", "--platform", "win_amd64",
            "--dest", str(cache), "-r", str(requirements),
        ], build.WORK / "ci-cmake-download.log", 480)
    if build.digest(wheel) != CMAKE_SHA256:
        raise ValueError("CMake wheel SHA-256 mismatch; retained for review")
    destination = build.WORK / "ci-cmake"
    if destination.exists():
        raise ValueError("Existing CMake extraction retained; review before another attempt")
    with zipfile.ZipFile(wheel) as archive:
        members = archive.infolist()
        if sum(m.file_size for m in members) > 512 * 1024**2:
            raise ValueError("CMake wheel expansion exceeds 512 MiB")
        for member in members:
            build.safe_name(member.filename)
            mode = member.external_attr >> 16
            if mode & 0o170000 not in (0, 0o100000, 0o040000):
                raise ValueError("CMake wheel contains a link or special file")
        destination.mkdir()
        archive.extractall(destination)
    return destination / "cmake/data/bin/cmake.exe"


def check_cmake(cmake, generator):
    capabilities = json.loads(capture([str(cmake), "-E", "capabilities"]))
    if capabilities["version"]["string"] != CMAKE_VERSION:
        raise ValueError(f"CMake {CMAKE_VERSION} required")
    if generator not in {entry["name"] for entry in capabilities["generators"]}:
        raise ValueError(f"CMake does not support discovered generator: {generator}")


def validate_install(libdir, budget_seconds=180):
    """Integrity check compatible with neo_asr.rs; no compilation or evidence rewrite."""
    deadline = time.monotonic() + budget_seconds

    def checked(path, expected):
        build.remaining(deadline, budget_seconds)
        if path.is_symlink() or not path.is_file() or build.record(path) != expected:
            raise ValueError(f"Native artifact hash/size mismatch: {path}")
        build.remaining(deadline, budget_seconds)

    manifest = json.loads((libdir / "neo-sherpa-asr.json").read_text(encoding="utf-8"))
    identity = {"schema": 1, "status": "native-validated", "version": build.VERSION,
                "source_commit": build.COMMIT, "source_sha256": build.SOURCE_SHA256,
                "target": "x86_64-pc-windows-msvc", "configuration": "Release",
                "options": build.OPTIONS}
    if any(manifest.get(k) != v for k, v in identity.items()):
        raise ValueError("Native manifest identity/options mismatch")
    expected = {name + ".lib" for name in build.INSTALLED_LIBS}
    if set(manifest["libraries"]) != expected:
        raise ValueError("Native manifest library set mismatch")
    if {p.name for p in libdir.iterdir() if p.suffix.lower() == ".lib"} != expected:
        raise ValueError("Native directory library set mismatch")
    if any(p.is_dir() or p.suffix.lower() == ".dll" for p in libdir.iterdir()):
        raise ValueError("Unexpected directory/DLL in native install")
    for name in sorted(expected):
        path = libdir / name
        checked(path, manifest["libraries"][name])
        with path.open("rb") as stream:
            if stream.read(8) != b"!<arch>\n" or path.stat().st_size <= 8:
                raise ValueError(f"Not a native archive: {name}")
    receipt_path = libdir / "neo-asr-receipt.json"
    checked(receipt_path, manifest["receipt"])
    receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
    lock = receipt["source_lock"]
    for key, value in {"version": build.VERSION, "commit": build.COMMIT,
                       "url": build.SOURCE_URL, "archive": build.SOURCE_FILE,
                       "sha256": build.SOURCE_SHA256}.items():
        if lock.get(key) != value:
            raise ValueError("Native receipt source lock mismatch")
    if receipt["options"] != build.OPTIONS:
        raise ValueError("Native receipt options mismatch")
    if receipt["dependency_archives"] != {k: {"url": v[0], "sha256": v[1]} for k, v in build.DEPS.items()}:
        raise ValueError("Native receipt dependency pins mismatch")
    for stage in ("configure", "build", "install"):
        command = receipt["commands"][stage + ".command.json"]
        if command.get("returncode") != 0 or command.get("timed_out"):
            raise ValueError(f"No successful native stage: {stage}")
    reports = receipt["symbol_reports"]
    if set(reports) != {name + ".symbols.txt" for name in build.INSTALLED_LIBS} or not receipt["licenses"]:
        raise ValueError("Missing native symbol/license evidence")
    attempt = libdir.parent.parent
    checked(attempt / "graph.json", receipt["graph"])
    checked(attempt / "omitted-source-links.json", receipt["omitted_source_links"])
    for name, info in reports.items():
        checked(attempt / "symbols" / name, info)
    for name, info in receipt["licenses"].items():
        relative = build.safe_name(name)
        if not relative.parts or relative.parts[0] != "licenses":
            raise ValueError("Invalid native license evidence path")
        checked(libdir.parent / relative, info)
    build.remaining(deadline, budget_seconds)


def prepare(args):
    if args.budget_seconds <= 0 or args.validation_seconds <= 0 or args.max_download_mib <= 0:
        raise ValueError("Build, validation and download budgets must be positive")
    build.WORK.mkdir(parents=True, exist_ok=True)
    build.CACHE.mkdir(parents=True, exist_ok=True)
    attempt = build.WORK / "native"
    libdir = attempt / "install/lib"
    if (libdir / "neo-sherpa-asr.json").exists():
        validate_install(libdir, args.validation_seconds)
        print(f"Existing native install verified (no rebuild): {libdir}")
        return
    if attempt.exists():
        raise ValueError("Incomplete native attempt retained; review it before explicit recovery")
    generator, instance, dumpbin = discover_vs(args.vswhere)
    cmake = Path(args.cmake).resolve() if args.cmake else prepare_cmake()
    check_cmake(cmake, generator)
    build.fetch_source()
    build.fetch_deps(args.max_download_mib * 1024**2)
    build.build(argparse.Namespace(cmake=str(cmake), dumpbin=dumpbin,
                                  generator=generator, vs_instance=instance,
                                  budget_seconds=args.budget_seconds))
    validate_install(libdir, args.validation_seconds)
    print(f"Ready for Cargo: {libdir}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cmake", help="Use an existing CMake 4.2.3 executable instead of the pinned wheel")
    parser.add_argument("--vswhere", help="Explicit vswhere executable")
    parser.add_argument("--budget-seconds", type=int, default=600,
                        help="Builder total configure/build/install/symbol-validation deadline; no retry")
    parser.add_argument("--validation-seconds", type=int, default=180,
                        help="Post-build/existing-install integrity validation deadline")
    parser.add_argument("--max-download-mib", type=int, default=256,
                        help="Aggregate missing dependency archive budget, separate from source/tool downloads")
    args = parser.parse_args(argv)
    try:
        if os.name != "nt":
            raise ValueError("Only Windows x64 MSVC is supported")
        prepare(args)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        print(f"BLOCKED: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
