"""检查发行载荷及有限范围 x64 PE；CLI 默认 MSVC external-prerequisite，不复制 CRT。

PE 范围为 neo.exe、runtime/onnx/*.dll；app-local 另查递归导入的白名单 CRT；
只读解析实际 PE 导出并拒绝已知 eSpeak/Piper 标记；无标记不证明不存在 GPL 代码。
不验证完整第三方 DLL 依赖闭包、模型有效性或 MinGit 运行能力。
法律文件仅检查载荷非空，不判定许可合规或解除审计中的发布阻断项。
"""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess

if __package__:
    from .audit_native_link import PE, risk_family
    from .prepare_runtime_distribution import verify_distribution, file_record, check_path
else:
    from audit_native_link import PE, risk_family
    from prepare_runtime_distribution import verify_distribution, file_record, check_path


CRT_NAMES = (
    "vcruntime140.dll", "vcruntime140_1.dll", "vcruntime140_threads.dll",
    "msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll",
    "msvcp140_atomic_wait.dll", "msvcp140_codecvt_ids.dll", "concrt140.dll",
)
CRT_MIN_VERSION = "14.51.36247.0"
CRT_DOWNLOAD_URL = "https://aka.ms/vc14/vc_redist.x64.exe"
CRT_NAME = re.compile("(?:" + "|".join(re.escape(name) for name in CRT_NAMES) + ")", re.I)
CRT_FAMILY = re.compile(r"(?:vcruntime|msvcp|msvcr|concrt|vccorlib).*\.dll", re.I)
MODELS = ("melspectrogram.onnx", "embedding_model.onnx", "hi_neo.onnx")
LANGUAGES = ("zh-CN", "en-US")
REQUIRED_FILES = (
    "neo.exe", *(f"resources/models/wake/{name}" for name in MODELS), "runtime/onnx/onnxruntime.dll",
    "resources/models/stt/sense-voice/model.int8.onnx", "resources/models/stt/sense-voice/tokens.txt",
    "resources/models/stt/vad/silero_vad.onnx", "LICENSE", "NOTICE",
    "docs/licenses/cargo-notices.txt",
    "docs/licenses/models/hi_neo-model.json", "docs/licenses/models/hi_neo-MIT.txt",
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


def validate_hi_neo(model, legal):
    index_path = legal / "hi_neo-model.json"
    license_path = legal / "hi_neo-MIT.txt"
    for path in (model, index_path, license_path):
        check_path(path)
        require_nonempty_file(path)
    index = json.loads(index_path.read_text(encoding="utf-8"))
    if (index.get("path") != "resources/models/wake/hi_neo.onnx"
            or index.get("license") != "MIT" or index.get("license_file") != "hi_neo-MIT.txt"
            or type(index.get("size")) is not int
            or file_record(model) != {k: index.get(k) for k in ("sha256", "size")}):
        raise ValueError("hi_neo model bytes/license index mismatch")
    if not license_path.read_text(encoding="utf-8-sig").strip():
        raise ValueError("hi_neo MIT license text is empty")
    return index


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
    validate_hi_neo(package / "resources/models/wake/hi_neo.onnx", package / "docs/licenses/models")
    verify_distribution(package / "docs/runtime-distribution", package / "runtime/gitbash")


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


def inspect_native_exports(binary):
    # Read actual export tables, not strings/dumpbin mocks; never load the image.
    exports = PE(Path(binary).read_bytes()).exports()
    findings = [entry["symbol"] for entry in exports if risk_family(entry["symbol"])]
    if findings:
        raise ValueError(f"Distribution BLOCKED: eSpeak/Piper native export markers in {binary}: "
                         + ", ".join(findings))
    print(f"Native export scan: {binary}: no known eSpeak/Piper markers; "
          "absence is inconclusive, NOT legal clearance or proof of GPL-free code.")


def inspect_pe(binary, dumpbin):
    inspect_native_exports(binary)
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


def check_package(package, dumpbin, redist_dir=None, crt_policy="app-local"):
    # Keep existing Python callers compatible; the release CLI defaults to external.
    if crt_policy not in ("external", "app-local"):
        raise ValueError(f"Unknown CRT policy: {crt_policy}")
    if crt_policy == "external" and redist_dir is not None:
        raise ValueError("--redist-dir requires --crt-policy app-local; external never copies CRT")
    package = Path(package)
    if crt_policy == "external":
        bundled = sorted(str(path.relative_to(package)) for path in package.rglob("*")
                         if CRT_NAME.fullmatch(path.name))
        if bundled:
            raise ValueError("external-prerequisite forbids bundled allowlisted CRT: " + ", ".join(bundled))
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
            if crt_policy == "external":
                continue
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
    if crt_policy == "external":
        print("CRT policy: external-prerequisite (no CRT copied or bundled).")
        print("Verified x64 PE images (neo.exe and runtime/onnx/*.dll only):", len(inspected))
        print("Required CRT names (direct/delay imports in audited images only):",
              ", ".join(sorted(required)) or "none")
        print(f"Prerequisite: official Microsoft Visual C++ v14 x64 Redistributable >= {CRT_MIN_VERSION}; "
              f"install the latest supported release independently: {CRT_DOWNLOAD_URL}")
        print("This is a conservative release prerequisite, NOT a minimum inferred from PE imports. "
              "A newer build toolset/runtime requires re-evaluation; use a runtime at least as new as the build tools.")
        print("External runtime presence, DLL versions/exports and transitive CRT imports are NOT verified; "
              "in-process import/runtime compatibility is NOT established.")
    else:
        print("CRT policy: app-local; redistribution approval/evidence is separately required, NOT granted here.")
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
    parser.add_argument("--crt-policy", choices=("external", "app-local"), default="external",
                        help="Default: external prerequisite, no bundled CRT; app-local requires separate approval")
    parser.add_argument("--redist-dir", type=Path,
                        help="Requires --crt-policy app-local; licensed Visual Studio x64 CRT directory, never System32")
    args = parser.parse_args()
    if args.redist_dir is not None and args.crt_policy != "app-local":
        parser.error("--redist-dir requires explicit --crt-policy app-local")
    check_package(args.package, args.dumpbin, args.redist_dir, crt_policy=args.crt_policy)


if __name__ == "__main__":
    main()
