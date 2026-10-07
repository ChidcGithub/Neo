"""最终发布前的文档/代码一致性只读检查（Python 3.11+，无第三方依赖）。

范围：README.md / NOTICE / LICENSE 与当前代码在版本号、功能数量、系统要求、
第三方许可文件布局上的一致性；docs/licenses 必备文件存在且非空；
仓库源码中的调试残留（dbg! 与未解释的 TODO/FIXME）。

不做的事：不构建、不执行任何被检查代码、不访问网络、不修改工作区、
不判定许可合规、不验证 docs-pri/ 私有审计结论，也不证明"没有遗留问题"——
只核对本文件明列的公开文档声明与仓库内可读证据是否一致。
CHANGELOG.md 按项目决策不存在（README「Versioning & Releasing」采用
GitHub Release 自动生成 changelog）；本脚本把这一决策作为固定前提核对，
而不是发现缺失时才报告。
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
README = "README.md"
NOTICE = "NOTICE"
LICENSE = "LICENSE"
WORKSPACE_TOML = "Cargo.toml"
TOOL_REGISTRY = "crates/neo-tools/src/tools/mod.rs"
RISK_ENUM = "crates/neo-tools/src/policy.rs"

# 发布载荷法律文件子集（与 tools/check_release.py REQUIRED_FILES 中的法律项保持一致；
# 那里核对打包产物，这里核对仓库源码树）。docs-pri/ 不在 Git 中，不作要求。
REQUIRED_LICENSE_FILES = (
    "docs/licenses/cargo-notices.txt",
    "docs/licenses/models/hi_neo-model.json",
    "docs/licenses/models/hi_neo-MIT.txt",
)
REQUIRED_LICENSE_DIRS = (
    "docs/licenses/assets",
    "docs/licenses/models",
    "docs/licenses/runtime",
    "docs/licenses/supplemental",
)
LANG_FILES = ("resources/lang/zh-CN.lang", "resources/lang/en-US.lang")

# 调试残留扫描的豁免：vendored/适配的第三方代码保留上游 TODO 属正常；
# 测试快照里出现的 'TODO' 字符串是测试数据，不是待办。
DEBUG_SCAN_EXEMPT = (
    "crates/neo-ui/src/flex/",   # 裁剪自 egui_flex 的 vendored 代码
    "vendor/",                   # 整体 vendored
)
DEBUG_SCAN_ALLOW_SUBSTRINGS = (
    "grep -rn 'TODO'",           # app_snapshot.rs 的测试数据
)


def fail(errors, message):
    errors.append(message)


def read(root, name):
    path = root / name
    if not path.is_file():
        raise ValueError(f"required file missing: {path}")
    return path.read_text(encoding="utf-8")


def workspace_field(text, field):
    match = re.search(rf'^\[workspace\.package\][^\[]*?^({field})\s*=\s*"([^"]+)"',
                      text, re.M | re.S)
    return match.group(2) if match else None


def parse_registry(text):
    """从 REGISTRY 静态数组提取 (name, risk) 对；只解析字面量，不执行代码。"""
    pairs = re.findall(r'name:\s*"([a-z0-9_]+)"[^}]*?risk:\s*Risk::(\w+)', text, re.S)
    names = [name for name, _ in pairs]
    if len(names) != len(set(names)):
        raise ValueError("duplicate tool names in registry: " + ", ".join(sorted(names)))
    return pairs


def parse_readme_tool_table(readme):
    """解析 Tool Chain 小节的风险表：| `risk` | a · b · c | policy |。"""
    section = re.search(r"^## Tool Chain\n(.*?)^## ", readme, re.M | re.S)
    if not section:
        raise ValueError("README is missing the '## Tool Chain' section")
    rows = {}
    for match in re.finditer(r"^\| `(\w+)` \| ([^|]+) \|", section.group(1), re.M):
        risk, cell = match.groups()
        rows[risk] = [name.strip() for name in cell.split("·")]
    return rows


def check_versions(root, errors):
    toml = read(root, WORKSPACE_TOML)
    readme = read(root, README)
    version = workspace_field(toml, "version")
    rust = workspace_field(toml, "rust-version")
    if version is None or rust is None:
        fail(errors, "Cargo.toml: [workspace.package] version/rust-version not found")
        return
    # README 明确声明“工作区正在准备 <version>”，两处必须一致。
    preparing = re.search(r"preparing \*\*`([0-9][^`]*)`\*\*", readme)
    if not preparing:
        fail(errors, "README: 'preparing **`X.Y.Z`**' statement not found "
                     "(Download & Install section)")
    elif preparing.group(1) != version:
        fail(errors, f"version mismatch: Cargo.toml={version}, "
                     f"README preparing={preparing.group(1)}")
    # Rust 版本：badge、Requirements 表、Build from Source 三处。
    for label, pattern in (
        ("badge", rf"badge/rust-{re.escape(rust)}%2B"),
        ("requirements", rf"\*\*Rust {re.escape(rust)}\+\*\*"),
        ("build-from-source", rf"Rust {re.escape(rust)}\+"),
    ):
        if not re.search(pattern, readme):
            fail(errors, f"README {label}: Rust {rust}+ not found "
                         f"(Cargo.toml rust-version={rust})")
    # LICENSE 是未替换占位符的 Apache-2.0 原文属正常；NOTICE 必须带真实版权行。
    notice = read(root, NOTICE)
    for label, text in (("NOTICE", notice), ("README", readme)):
        if not re.search(r"Copyright \(c\) \d{4} \S", text):
            fail(errors, f"{label}: 'Copyright (c) <year> <owner>' not found")
    notice_year = re.search(r"Copyright \(c\) (\d{4})", notice)
    readme_year = re.search(r"Copyright \(c\) (\d{4})", readme)
    if notice_year and readme_year and notice_year.group(1) != readme_year.group(1):
        fail(errors, f"copyright year mismatch: NOTICE={notice_year.group(1)}, "
                     f"README={readme_year.group(1)}")
    license_text = read(root, LICENSE)
    if "Apache License" not in license_text:
        fail(errors, "LICENSE does not look like Apache-2.0")
    if "Apache-2.0" not in toml:
        fail(errors, "Cargo.toml workspace license is not Apache-2.0")


def check_changelog_policy(root, errors):
    """项目决策：仓库不维护 CHANGELOG.md，发布说明由 GitHub Release 自动生成。

    该决策必须继续由 README 明确描述；若将来改为维护仓库内 CHANGELOG，
    删除本检查并改为核对 CHANGELOG 与 workspace 版本。
    """
    changelog = list(root.glob("CHANGELOG*"))
    readme = read(root, README)
    if changelog:
        if "auto changelog" not in readme:
            fail(errors, "CHANGELOG exists but README no longer describes the "
                         "changelog policy: " + ", ".join(p.name for p in changelog))
    elif "auto changelog" not in readme:
        fail(errors, "no repo CHANGELOG and README no longer documents the "
                     "GitHub auto-changelog policy; one of them must exist")


def check_tools(root, errors):
    readme = read(root, README)
    registry_text = read(root, TOOL_REGISTRY)
    pairs = parse_registry(registry_text)
    code_names = sorted(name for name, _ in pairs)
    # Feature Highlights 与 Architecture 中的数量声明。
    for match in re.finditer(r"(\d+) (?:gated )?tools", readme):
        if int(match.group(1)) != len(pairs):
            fail(errors, f"README claims {match.group(1)} tools, "
                         f"registry has {len(pairs)}")
    if f"{len(pairs)} gated tools" not in readme:
        fail(errors, f"README architecture diagram: '{len(pairs)} gated tools' not found")
    rows = parse_readme_tool_table(readme)
    readme_names = sorted(name for names in rows.values() for name in names)
    if readme_names != code_names:
        fail(errors, "tool names differ between README Tool Chain table and registry: "
                     f"README-only={sorted(set(readme_names) - set(code_names))}, "
                     f"code-only={sorted(set(code_names) - set(readme_names))}")
        return
    # 风险分组：ask_user 在代码中是 Risk::Read，README 单独列一行说明其交互语义。
    code_risks = {}
    for name, risk in pairs:
        code_risks.setdefault(risk.lower(), set()).add(name)
    ask_names = set(rows.get("ask", []))
    for row_risk, names in rows.items():
        expected = set(names)
        if row_risk == "ask":
            if expected - code_risks.get("read", set()):
                fail(errors, f"README 'ask' row lists tools not in code Read risk: "
                             f"{sorted(expected - code_risks.get('read', set()))}")
            continue
        actual = code_risks.get(row_risk, set()) - ask_names
        if actual != expected:
            fail(errors, f"risk '{row_risk}' mismatch: README={sorted(expected)}, "
                         f"code={sorted(actual)}")


def check_requirements(root, errors):
    readme = read(root, README)
    for needle in ("Windows 10 2004", "19041", "Python 3.11+",
                   "WDA_EXCLUDEFROMCAPTURE"):
        if needle not in readme:
            fail(errors, f"README requirements: {needle!r} not found")
    badge = re.search(r"badge/platform-Windows%2010%20(\d+)%2B", readme)
    if not badge or badge.group(1) != "2004":
        fail(errors, "README platform badge does not match 'Windows 10 2004+'")


def check_license_tree(root, errors):
    for name in REQUIRED_LICENSE_FILES:
        path = root / name
        if not path.is_file() or path.stat().st_size == 0:
            fail(errors, f"required license file missing/empty: {name}")
    for name in REQUIRED_LICENSE_DIRS:
        path = root / name
        if not path.is_dir() or not any(path.iterdir()):
            fail(errors, f"required license directory missing/empty: {name}")
    notice = read(root, NOTICE)
    if "docs/licenses/" not in notice:
        fail(errors, "NOTICE no longer points at docs/licenses/")
    index = root / "docs/licenses/models/hi_neo-model.json"
    if index.is_file():
        try:
            data = json.loads(index.read_text(encoding="utf-8"))
            if data.get("license") != "MIT":
                fail(errors, "hi_neo-model.json: license field is not 'MIT'")
        except ValueError as error:
            fail(errors, f"hi_neo-model.json is not valid JSON: {error}")
    # supplemental 索引必须与目录内容一致（哈希文件名 + index.json）。
    supplemental = root / "docs/licenses/supplemental"
    if supplemental.is_dir():
        texts = {p.name for p in supplemental.glob("*.txt")}
        index_path = supplemental / "index.json"
        if not index_path.is_file():
            fail(errors, "docs/licenses/supplemental/index.json missing")
        else:
            try:
                entries = json.loads(index_path.read_text(encoding="utf-8"))
                indexed = set()
                items = entries if isinstance(entries, list) else entries.get("entries", [])
                for entry in items:
                    if isinstance(entry, dict) and "file" in entry:
                        indexed.add(entry["file"])
                if indexed and indexed != texts:
                    fail(errors, "supplemental index/files drift: "
                                 f"index-only={sorted(indexed - texts)}, "
                                 f"files-only={sorted(texts - indexed)}")
            except ValueError as error:
                fail(errors, f"supplemental/index.json is not valid JSON: {error}")


def check_languages(root, errors):
    catalogs = {}
    for name in LANG_FILES:
        path = root / name
        if not path.is_file() or path.stat().st_size == 0:
            fail(errors, f"language resource missing/empty: {name}")
            continue
        try:
            catalog = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            fail(errors, f"invalid UTF-8 JSON language resource {name}: {error}")
            continue
        if not isinstance(catalog, dict) or not catalog:
            fail(errors, f"language resource must be a non-empty JSON object: {name}")
            continue
        catalogs[name] = catalog
    if len(catalogs) == 2:
        zh, en = catalogs[LANG_FILES[0]], catalogs[LANG_FILES[1]]
        if zh.keys() != en.keys():
            fail(errors, "zh-CN.lang and en-US.lang keys differ: "
                         f"zh-only={sorted(zh.keys() - en.keys())[:5]}, "
                         f"en-only={sorted(en.keys() - zh.keys())[:5]}")
        empty = [key for key, value in en.items() if not str(value).strip()]
        if empty:
            fail(errors, f"en-US.lang has empty translations: {empty[:5]}")


def scan_debug_residue(root, errors):
    """只读扫描 crates/**.rs：dbg! 一律报错；TODO/FIXME 在豁免目录外报错。"""
    todo = re.compile(r"TODO|FIXME|XXX|HACK")
    for path in sorted((root / "crates").rglob("*.rs")):
        relative = path.relative_to(root).as_posix()
        if any(relative.startswith(prefix) for prefix in DEBUG_SCAN_EXEMPT):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for lineno, line in enumerate(text.splitlines(), 1):
            if "dbg!(" in line:
                fail(errors, f"dbg! macro left in {relative}:{lineno}")
            if todo.search(line) and not any(s in line for s in DEBUG_SCAN_ALLOW_SUBSTRINGS):
                fail(errors, f"TODO/FIXME left in {relative}:{lineno}: {line.strip()[:80]}")


def run(root):
    errors = []
    root = Path(root)
    for check in (check_versions, check_changelog_policy, check_tools,
                  check_requirements, check_license_tree, check_languages,
                  scan_debug_residue):
        try:
            check(root, errors)
        except ValueError as error:
            fail(errors, f"{check.__name__}: {error}")
    return errors


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT,
                        help="repository root (default: parent of tools/)")
    args = parser.parse_args(argv)
    errors = run(args.root)
    if errors:
        print(f"doc consistency: {len(errors)} problem(s):")
        for error in errors:
            print("  - " + error)
        print("Static consistency only; this is NOT license compliance, "
              "a release approval, or proof that no other drift exists.")
        return 1
    print("doc consistency: README/NOTICE/license tree/tool table/version strings "
          "match the repository evidence checked here.")
    print("Scope: static text checks only — no license verdict, no build, "
              "no claim that unlisted claims are correct.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
