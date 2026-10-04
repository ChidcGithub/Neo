"""检查发行必需载荷非空，核验有限范围的 x64 PE 导入并收集获授权的 MSVC CRT。

PE 范围仅为 neo.exe、runtime/onnx/*.dll 及其递归导入的白名单 CRT；
不验证完整第三方 DLL 依赖闭包、模型有效性或 MinGit 运行能力。
法律文件仅检查载荷非空，不判定许可合规或解除审计中的发布阻断项。
"""
import argparse
import json
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
LANGUAGES = ("zh-CN", "en-US")
REQUIRED_FILES = (
    "neo.exe", *(f"resources/models/wake/{name}" for name in MODELS), "runtime/onnx/onnxruntime.dll",
    "resources/models/stt/sense-voice/model.int8.onnx", "resources/models/stt/sense-voice/tokens.txt",
    "resources/models/stt/vad/silero_vad.onnx", "LICENSE", "NOTICE", "docs/README.md",
    "docs/licenses/README.md", "docs/licenses/cargo-notices.txt",
    *(f"resources/lang/{language}.lang" for language in LANGUAGES),
)
# fetch_runtime.ensure_bash_named 将 MinGit 的 usr/bin/sh.exe 补名为 usr/bin/bash.exe；
# find_bash 也接受 bin/bash.exe，不能按脚本顶端的布局示意只检查 bin/。
BASH_PATHS = ("runtime/gitbash/bin/bash.exe", "runtime/gitbash/usr/bin/bash.exe")


def require_nonempty_file(path):
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f"Required release resource missing/empty: {path}")


def validate_languages(package):
    directory = package / "resources/lang"
    unexpected = {path.name for path in directory.glob("*.lang")} - {f"{language}.lang" for language in LANGUAGES}
    if unexpected:
        raise ValueError(f"Unsupported release language resources: {sorted(unexpected)}")
    catalogs = {}
    for language in LANGUAGES:
        path = directory / f"{language}.lang"
        require_nonempty_file(path)
        try:
            catalog = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            raise ValueError(f"Invalid UTF-8 JSON language resource: {path}: {error}") from error
        if (not isinstance(catalog, dict) or not catalog
                or any(not isinstance(key, str) or not key.strip() or not isinstance(value, str)
                       for key, value in catalog.items())):
            raise ValueError(f"Language resource must be a non-empty JSON object of str:str with non-empty keys: {path}")
        if language == "en-US" and any(not value.strip() for value in catalog.values()):
            raise ValueError(f"English translations must not be empty: {path}")
        catalogs[language] = catalog
    if catalogs["zh-CN"].keys() != catalogs["en-US"].keys():
        raise ValueError(f"Language resource keys differ: {directory / 'zh-CN.lang'} and {directory / 'en-US.lang'}")


def validate_payload(package):
    for name in REQUIRED_FILES:
        require_nonempty_file(package / name)
    validate_languages(package)
    candidates = [package / name for name in BASH_PATHS]
    present = [path for path in candidates if path.exists() or path.is_symlink()]
    if not present:
        raise ValueError("Required release resource missing/empty: " + " or ".join(map(str, candidates)))
    # 两种布局可共存，但不能让空的优先候选遮蔽另一个可用的 bash。
    for path in present:
        require_nonempty_file(path)


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
    onnx = package / "runtime/onnx"
    validate_payload(package)
    pending = [package / "neo.exe", *sorted(onnx.glob("*.dll"))]
    for binary in pending:
        require_nonempty_file(binary)
    redist = validate_redist_dir(Path(redist_dir)) if redist_dir else None
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
                if not source.is_file() or source.stat().st_size == 0:
                    raise ValueError(f"Required CRT is absent from licensed redistributables or empty: {source}")
                if target not in inspected:
                    shutil.copy2(source, target)
                    print(f"App-local CRT: {source} -> {target}")
            elif not target.is_file() or target.stat().st_size == 0:
                raise ValueError(f"Missing app-local CRT or empty file: {target}; supply licensed VS redistributables")
            pending.append(target)
    print("Required release payload files exist and are non-empty (including bundled Bash and legal documents).")
    print("Legal documents: payload completeness only; license compliance is NOT verified. "
          "Unresolved legal release blockers still require review; private audit reports are not release payloads.")
    print("Verified x64 PE images (neo.exe, runtime/onnx/*.dll and imported allowlisted CRT only):", len(inspected))
    print("Required app-local CRT:", ", ".join(sorted(required)) or "none")
    print("Full third-party DLL dependency closure is NOT audited; MinGit PE images/dependencies "
          "and model validity are NOT audited.")
    print("Static checks only; Windows 10 19041 runtime validation is still required.")
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
