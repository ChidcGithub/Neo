"""核验发行包的 x64 PE 导入，仅收集依赖所需且获授权的 MSVC CRT DLL。"""
import argparse
from pathlib import Path
import re
import shutil
import subprocess


CRT_NAMES = (
    "vcruntime140.dll", "vcruntime140_1.dll", "vcruntime140_threads.dll",
    "msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll",
    "msvcp140_atomic_wait.dll", "msvcp140_codecvt_ids.dll", "concrt140.dll",
)
CRT_NAME = re.compile("(?:" + "|".join(re.escape(name) for name in CRT_NAMES) + ")", re.I)
CRT_FAMILY = re.compile(r"(?:vcruntime|msvcp|msvcr|concrt|vccorlib).*\.dll", re.I)
MODELS = ("melspectrogram.onnx", "embedding_model.onnx", "hi_neo.onnx")


def parse_pe_report(report):
    machines = re.findall(r"^\s*([0-9A-Fa-f]+) machine \(", report, re.M)
    if machines != ["8664"]:
        raise ValueError(f"Expected one native x64 PE image, got machines={machines}")
    imports = {name.lower() for name in re.findall(r"^\s*([\w.-]+\.dll)\s*$", report, re.M | re.I)}
    if not imports:
        raise ValueError("No imports found in dumpbin report; refusing an incomplete audit")
    for name in imports:
        if CRT_FAMILY.fullmatch(name) and not CRT_NAME.fullmatch(name):
            raise ValueError(f"Unsupported CRT dependency (debug/legacy/unknown): {name}")
    return imports


def inspect_pe(binary, dumpbin):
    result = subprocess.run([str(dumpbin), "/NOLOGO", "/HEADERS", "/IMPORTS", str(binary)],
                            capture_output=True, text=True, errors="replace", timeout=60, check=True)
    imports = parse_pe_report(result.stdout)
    # 在 CI 日志中保留符号级导入证据，包含延迟加载的依赖。
    print(f"=== PE audit: {binary} ===\n{result.stdout}")
    return imports


def validate_redist_dir(directory):
    directory = directory.resolve()
    parts = [p.lower() for p in directory.parts]
    if (not re.fullmatch(r"microsoft\.vc\d+\.crt", directory.name, re.I)
            or directory.parent.name.lower() != "x64"
            or ["vc", "redist", "msvc"] not in [parts[i:i + 3] for i in range(len(parts) - 2)]
            or "onecore" in parts or "debug_nonredist" in parts):
        raise ValueError("CRT source must be Visual Studio VC/Redist/MSVC/<version>/x64/Microsoft.VC*.CRT")
    if not directory.is_dir():
        raise ValueError(f"MSVC redistributable directory missing: {directory}")
    return directory


def check_package(package, dumpbin, redist_dir=None):
    package = Path(package)
    assets = package / "assets"
    for path in [package / "neo.exe", *(assets / name for name in MODELS), assets / "onnxruntime.dll"]:
        if not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"Required release resource missing/empty: {path}")
    redist = validate_redist_dir(Path(redist_dir)) if redist_dir else None
    pending = [package / "neo.exe", *sorted(assets.glob("*.dll"))]
    inspected = set()
    required = set()
    while pending:
        binary = pending.pop(0)
        if binary in inspected:
            continue
        inspected.add(binary)
        dependencies = inspect_pe(binary, dumpbin)
        for name in sorted(dependencies):
            if not CRT_NAME.fullmatch(name):
                continue
            required.add(name)
            target = package / name
            if redist:
                source = redist / name
                if not source.is_file():
                    raise ValueError(f"Required CRT is absent from licensed redistributables: {source}")
                if target not in inspected:
                    shutil.copy2(source, target)
                    print(f"App-local CRT: {source} -> {target}")
            elif not target.is_file():
                raise ValueError(f"Missing app-local CRT: {target}; supply licensed VS redistributables")
            pending.append(target)
    print("Verified x64 PE images:", len(inspected))
    print("Required app-local CRT:", ", ".join(sorted(required)) or "none")
    print("Static PE/import audit only; Windows 10 19041 runtime validation is still required.")
    return required


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, default=Path("dist/neo"))
    parser.add_argument("--dumpbin", default="dumpbin.exe")
    parser.add_argument("--redist-dir", type=Path, help="Licensed Visual Studio x64 CRT directory; never System32")
    args = parser.parse_args()
    check_package(args.package, args.dumpbin, args.redist_dir)


if __name__ == "__main__":
    main()
