#!/usr/bin/env python3
"""批量修复 Neo 测试代码中的机械 Clippy 警告。

只改测试文件（生产代码一律不碰）：
  - crates/<x>/src/**/*_tests.rs / *_regression.rs / tests.rs
  - crates/<x>/tests/**/*.rs

处理的模式：
  1. field_reassign_with_default  let mut x = T::default(); x.a = ..; -> T { a: .., ..Default::default() }
  2. drop_non_drop                drop(render);（非 Drop 类型） -> 删除该行
  3. useless_vec                  应用 clippy 的机器建议（vec! -> 数组/切片）
  4. cloned_ref_to_slice_refs     应用机器建议，或 &[x.clone()] -> std::slice::from_ref(x)
  5. bool_assert_comparison       assert_eq!(x, false) -> assert!(!x)
  6. collapsible_if / unnecessary_cast / 其他 -> 只报告，不改（本仓库这些点都在生产代码里）

用法：
  python tools/fix_clippy_tests.py [--dry-run]

脚本自己跑 `cargo clippy --workspace --all-targets --message-format=json`
拿精确位置；每处修改前都会校验现场文本，不符合预期就跳过并列出。
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# 目标 lint（clippy:: 前缀）
LINT_FIELD_REASSIGN = "clippy::field_reassign_with_default"
LINT_DROP_NON_DROP = "clippy::drop_non_drop"
LINT_USELESS_VEC = "clippy::useless_vec"
LINT_CLONED_SLICE = "clippy::cloned_ref_to_slice_refs"
LINT_BOOL_ASSERT = "clippy::bool_assert_comparison"
LINT_LET_RETURN = "clippy::let_and_return"  # field_reassign 修复的次生形态，只处理机器建议
# 不在原始清单内、但同属于测试文件且会挡住 -D warnings 的机械 lint
LINT_NEEDLESS_BORROW = "clippy::needless_borrows_for_generic_args"
LINT_IS_MULTIPLE_OF = "clippy::manual_is_multiple_of"
LINT_PRECEDENCE = "clippy::precedence"
LINT_MANUAL_CONTAINS = "clippy::manual_contains"
LINT_UNUSED_MUT = "unused_mut"  # rustc lint，删除多余的 mut
LINT_UNUSED_VAR = "unused_variables"  # rustc lint，let x -> let _x
# 只报告、不改的 lint（出现位置都在生产代码里）
REPORT_ONLY = {
    "clippy::collapsible_if",
    "clippy::collapsible_match",
    "clippy::unnecessary_cast",
    "clippy::redundant_clone",
}
TARGET_LINTS = {
    LINT_FIELD_REASSIGN,
    LINT_DROP_NON_DROP,
    LINT_USELESS_VEC,
    LINT_CLONED_SLICE,
    LINT_BOOL_ASSERT,
    LINT_LET_RETURN,
    LINT_NEEDLESS_BORROW,
    LINT_IS_MULTIPLE_OF,
    LINT_PRECEDENCE,
    LINT_MANUAL_CONTAINS,
    LINT_UNUSED_MUT,
    LINT_UNUSED_VAR,
} | REPORT_ONLY


def in_scope(path: str) -> bool:
    """是否属于允许修改的测试文件。"""
    p = path.replace("\\", "/")
    m = re.match(r"crates/[^/]+/src/(.+/)?([^/]+)\.rs$", p)
    if m:
        stem = m.group(2)
        if stem.endswith("_tests") or stem.endswith("_regression") or stem == "tests":
            return True
    return bool(re.match(r"crates/[^/]+/tests/[^/]+\.rs$", p))


@dataclass
class Edit:
    """对某个文件的一次修改（行号 0 基）。"""

    line_start: int
    line_end: int  # 含
    new_lines: list[str]
    desc: str


@dataclass
class FileWork:
    path: str  # 绝对路径
    rel: str  # 仓库相对（/ 分隔）
    lines: list[str] = field(default_factory=list)  # 不含换行符
    eol: str = "\n"
    edits: list[Edit] = field(default_factory=list)
    skipped: list[str] = field(default_factory=list)
    fixed: list[str] = field(default_factory=list)


def run_clippy_json() -> list[dict]:
    proc = subprocess.run(
        ["cargo", "clippy", "--workspace", "--all-targets", "--message-format=json"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    diags = []
    for raw in proc.stdout.decode("utf-8", "replace").splitlines():
        raw = raw.strip()
        if not raw.startswith("{"):
            continue
        try:
            obj = json.loads(raw)
        except json.JSONDecodeError:
            continue
        if obj.get("reason") == "compiler-message":
            msg = obj.get("message", {})
            if msg.get("level") == "warning" and msg.get("code"):
                diags.append(msg)
    return diags


def load(path: str, rel: str) -> FileWork:
    with open(path, "r", encoding="utf-8", newline="") as fh:
        text = fh.read()
    eol = "\r\n" if "\r\n" in text else "\n"
    fw = FileWork(path=path, rel=rel, eol=eol)
    fw.lines = text.replace("\r\n", "\n").split("\n")
    return fw


def save(fw: FileWork) -> None:
    with open(fw.path, "w", encoding="utf-8", newline="") as fh:
        fh.write(fw.eol.join(fw.lines))


def strip_noise(line: str) -> str:
    """去掉行内字符串字面量和 // 注释（用于数括号/找标识符，够用即可）。"""
    line = re.sub(r'"(?:\\.|[^"\\])*"', '""', line)
    line = re.sub(r"'(?:\\.|[^'\\])'", "''", line)
    idx = line.find("//")
    return line if idx < 0 else line[:idx]


def balance(text: str) -> int:
    n = 0
    for ch in text:
        if ch in "([{":
            n += 1
        elif ch in ")]}":
            n -= 1
    return n


# ---------------------------------------------------------------------------
# 模式 1：field_reassign_with_default
# ---------------------------------------------------------------------------

def fix_field_reassign(fw: FileWork, span: dict, dry_run: bool) -> None:
    aline_no = span["line_start"] - 1  # 第一个赋值语句所在行
    aline = fw.lines[aline_no]
    col = span["column_start"] - 1
    m = re.match(r"(\w+)\.(\w+)\s*=", aline[col:])
    if not m:
        fw.skipped.append(f"L{aline_no + 1}: 赋值语句不是简单字段赋值（可能是链式调用），跳过")
        return
    var = m.group(1)

    # let 行必须与第一个赋值紧邻
    let_no = aline_no - 1
    let_re = re.compile(r"^(\s*)let mut " + re.escape(var) + r" = ([\w:]+)::default\(\);$")
    lm = let_re.match(fw.lines[let_no]) if let_no >= 0 else None
    if not lm:
        fw.skipped.append(f"L{aline_no + 1}: 上一行不是 `let mut {var} = T::default();`，跳过")
        return
    indent, ty = lm.group(1), lm.group(2)

    # 连续收集 var.field = ...; 语句
    assigns: list[tuple[str, list[str], str]] = []  # (field, 值各行, 行尾注释)
    i = aline_no
    while i < len(fw.lines):
        line = fw.lines[i]
        am = re.match(r"^" + re.escape(indent) + re.escape(var) + r"\.(\w+)\s*=\s*(.*)$", line)
        if not am:
            break
        fname, rest = am.group(1), am.group(2)
        stmt = [rest]
        bal = balance(strip_noise(rest))
        while (bal > 0 or not strip_noise(stmt[-1]).rstrip().endswith(";")) and i + 1 < len(fw.lines):
            i += 1
            stmt.append(fw.lines[i])
            bal += balance(strip_noise(fw.lines[i]))
            if len(stmt) > 60:
                fw.skipped.append(f"L{aline_no + 1}: 赋值语句超过 60 行，跳过")
                return
        last = stmt[-1]
        semi = strip_noise(last).rstrip()
        if bal != 0 or not semi.endswith(";"):
            fw.skipped.append(f"L{aline_no + 1}: `{var}.{fname}` 赋值语句形态异常，跳过")
            return
        # 行尾注释
        comment = ""
        tail = last[last.rfind(";") + 1 :].strip()
        if tail:
            if tail.startswith("//"):
                comment = tail
            else:
                fw.skipped.append(f"L{aline_no + 1}: `{var}.{fname}` 语句 `;` 后还有其他代码，跳过")
                return
        stmt[-1] = last[: last.rfind(";")]  # 去掉结尾分号
        assigns.append((fname, stmt, comment))
        i += 1
    if not assigns:
        fw.skipped.append(f"L{aline_no + 1}: 未收集到字段赋值，跳过")
        return
    end_no = i  # 折叠区域 [let_no, end_no)

    # mut 是否需要保留：只看所在函数体（let 行处在 fn 大括号之内，故初始深度为 1）
    fn_end = len(fw.lines)
    depth = 1
    for j in range(let_no, len(fw.lines)):
        for ch in strip_noise(fw.lines[j]):
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth <= 0:
                    fn_end = j
                    break
        if fn_end != len(fw.lines):
            break
    keep_mut = False
    for j in range(end_no, fn_end):
        clean = strip_noise(fw.lines[j])
        if not re.search(r"\b" + re.escape(var) + r"\b", clean):
            continue
        if re.fullmatch(r"\s*" + re.escape(var) + r"\s*", clean):
            continue  # 裸返回 / 按值移动
        if re.search(r"&\s*" + re.escape(var) + r"\b", clean) and not re.search(
            r"&\s*mut\s*" + re.escape(var) + r"\b", clean
        ):
            continue  # 不可变借用
        keep_mut = True  # 方法调用 / 字段访问 / &mut var 等，保守保留 mut
        break

    mut_kw = "mut " if keep_mut else ""
    out = [f"{indent}let {mut_kw}{var} = {ty} {{"]
    for fname, stmt, comment in assigns:
        first = stmt[0].strip()
        if len(stmt) == 1:
            line_out = f"{indent}    {fname}: {first},"
            out.append(line_out + (f" {comment}" if comment else ""))
        else:
            if comment:
                fw.skipped.append(f"L{aline_no + 1}: 多行赋值带行尾注释，跳过")
                return
            out.append(f"{indent}    {fname}: {first}")
            for cont in stmt[1:-1]:
                out.append("    " + cont)
            out.append("    " + stmt[-1].rstrip() + ",")
    out.append(f"{indent}    ..Default::default()")
    out.append(f"{indent}}};")

    # 不需要 mut 且折叠区后紧跟 `{var}` 裸返回（函数尾表达式）：直接返回结构体，
    # 避免引入 let_and_return。
    tail_no = end_no
    if (
        not keep_mut
        and tail_no < len(fw.lines)
        and fw.lines[tail_no] == f"{indent}{var}"
    ):
        nxt = tail_no + 1
        while nxt < len(fw.lines) and not fw.lines[nxt].strip():
            nxt += 1
        if nxt == fn_end:
            out[0] = f"{indent}{ty} {{"
            out[-1] = f"{indent}}}"
            fw.edits.append(Edit(let_no, tail_no, out, f"field_reassign_with_default `{var}`（尾表达式）"))
            return
    fw.edits.append(Edit(let_no, end_no - 1, out, f"field_reassign_with_default `{var}`"))


# ---------------------------------------------------------------------------
# 模式 2：drop_non_drop —— 删除整行
# ---------------------------------------------------------------------------

def fix_drop_non_drop(fw: FileWork, span: dict, dry_run: bool) -> None:
    ln = span["line_start"] - 1
    line = fw.lines[ln]
    m = re.fullmatch(r"\s*drop\(\s*&?\s*(\w+)\s*\);", line)
    if not m:
        fw.skipped.append(f"L{ln + 1}: drop 语句不是独占一行的简单形式，跳过")
        return
    var = m.group(1)
    # 要求该变量在前面 60 行内（除 let 声明外）至少被用过一次，避免删后变成未使用变量
    used = False
    for j in range(max(0, ln - 60), ln):
        clean = strip_noise(fw.lines[j])
        if re.search(r"\b" + re.escape(var) + r"\b", clean) and not re.match(
            r"\s*let\b", clean
        ):
            used = True
            break
    if not used:
        fw.skipped.append(f"L{ln + 1}: `{var}` 在 drop 前没有其他使用，删除可能引入 unused 警告，跳过")
        return
    fw.edits.append(Edit(ln, ln, [], f"drop_non_drop `drop({var})`"))


# ---------------------------------------------------------------------------
# 模式 5：bool_assert_comparison
# ---------------------------------------------------------------------------

def fix_bool_assert(fw: FileWork, span: dict, dry_run: bool) -> None:
    ls, le = span["line_start"] - 1, span["line_end"] - 1
    cs, ce = span["column_start"] - 1, span["column_end"] - 1
    if ls == le:
        text = fw.lines[ls][cs:ce]
    else:
        parts = [fw.lines[ls][cs:]]
        parts.extend(fw.lines[ls + 1 : le])
        parts.append(fw.lines[le][:ce])
        text = "\n".join(parts)
    text = text.rstrip().rstrip(";")
    if not text.startswith("assert_eq!"):
        fw.skipped.append(f"L{ls + 1}: 宏文本不是 assert_eq! 开头，跳过")
        return
    # 顶层逗号切参
    inner = text[len("assert_eq!") :].strip()
    if not (inner.startswith("(") and inner.rstrip().endswith(")")):
        fw.skipped.append(f"L{ls + 1}: assert_eq! 形态异常，跳过")
        return
    body = inner.strip()[1:-1]
    depth = 0
    split = -1
    for idx, ch in enumerate(body):
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == "," and depth == 0:
            split = idx
            break
    if split < 0:
        fw.skipped.append(f"L{ls + 1}: 找不到顶层逗号，跳过")
        return
    arg1, arg2 = body[:split].strip(), body[split + 1 :].strip()
    if arg2 not in ("true", "false"):
        fw.skipped.append(f"L{ls + 1}: 第二参数不是布尔字面量（{arg2!r}），跳过")
        return
    if "\n" in arg1:
        fw.skipped.append(f"L{ls + 1}: 第一参数跨行，非机械可改，跳过")
        return
    indent = re.match(r"\s*", fw.lines[ls]).group(0)
    expr = arg1 if arg2 == "true" else f"!{arg1}"
    out = [f"{indent}assert!(", f"{indent}    {expr}", f"{indent});"]
    fw.edits.append(Edit(ls, le, out, f"bool_assert_comparison -> assert!({expr[:40]}...)"))


# ---------------------------------------------------------------------------
# 机器建议（useless_vec / cloned_ref_to_slice_refs 等）
# ---------------------------------------------------------------------------

def apply_suggestion(fw: FileWork, msg: dict, dry_run: bool) -> bool:
    for child in msg.get("children", []):
        spans = [
            sp
            for sp in child.get("spans", [])
            if sp.get("suggested_replacement") is not None
        ]
        if not spans:
            continue
        # applicability 可能在 child 上，也可能逐 span 标注（不同 lint 结构不同）
        app = child.get("suggestion_applicability")
        if app is None:
            apps = {sp.get("suggestion_applicability") for sp in spans}
            app = apps.pop() if len(apps) == 1 else None
        if app != "MachineApplicable":
            continue
        if any(
            sp["file_name"].replace("\\", "/") != fw.rel for sp in spans
        ):
            continue
        spans.sort(key=lambda s: (s["line_start"], s["column_start"]))
        pending = []
        ok = True
        for sp in spans:
            ls, le = sp["line_start"] - 1, sp["line_end"] - 1
            cs, ce = sp["column_start"] - 1, sp["column_end"] - 1
            new = sp["suggested_replacement"]
            merged = fw.lines[ls][:cs] + new + fw.lines[le][ce:]
            new_lines = merged.split("\n")
            if not merged.strip():
                new_lines = []  # 整段删除后只剩空白：连行一起删
            old = fw.lines[ls][cs:] if ls != le else fw.lines[ls][cs:ce]
            old = old.strip()[:40]
            shown = new.strip().replace("\n", " ")[:40]
            pending.append(Edit(ls, le, new_lines, f"{msg['code']['code']}: `{old}` -> `{shown}`"))
        if ok and pending:
            fw.edits.extend(pending)
            return True
    return False


def fix_cloned_slice_fallback(fw: FileWork, span: dict, dry_run: bool) -> None:
    """&[x.clone()] -> std::slice::from_ref(x)（机器建议缺失时的兜底）。"""
    ln = span["line_start"] - 1
    cs, ce = span["column_start"] - 1, span["column_end"] - 1
    old = fw.lines[ln][cs:ce]
    m = re.fullmatch(r"&\[\s*(\w+)\.clone\(\)\s*\]", old)
    if not m:
        fw.skipped.append(f"L{ln + 1}: 文本 {old!r} 不是 `&[x.clone()]`，跳过")
        return
    new = f"std::slice::from_ref({m.group(1)})"
    line = fw.lines[ln]
    fw.edits.append(Edit(ln, ln, [line[:cs] + new + line[ce:]], f"cloned_ref_to_slice_refs: `{old}` -> `{new}`"))


# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------

def one_pass(dry_run: bool) -> tuple[int, dict[str, "FileWork"], dict[str, list[str]]]:
    """一轮：跑 clippy JSON、应用修复、写盘。返回 (修复数, 文件, 只报告项)。"""
    diags = run_clippy_json()

    files: dict[str, FileWork] = {}

    def work_for(rel: str) -> FileWork:
        if rel not in files:
            files[rel] = load(os.path.join(ROOT, rel.replace("/", os.sep)), rel)
        return files[rel]

    reported: dict[str, list[str]] = {}  # lint -> [位置]（仅报告）
    seen = set()

    for msg in diags:
        code = msg["code"]["code"]
        if code not in TARGET_LINTS:
            continue
        primaries = [s for s in msg.get("spans", []) if s.get("is_primary")]
        if not primaries:
            continue
        span = primaries[0]
        rel = span["file_name"].replace("\\", "/")
        key = (code, rel, span["line_start"], span["column_start"])
        if key in seen:
            continue
        seen.add(key)

        if not in_scope(rel):
            reported.setdefault(code, []).append(f"{rel}:{span['line_start']}（生产代码，跳过）")
            continue
        if code in REPORT_ONLY:
            reported.setdefault(code, []).append(f"{rel}:{span['line_start']}（策略为只报告）")
            continue

        fw = work_for(rel)
        before = len(fw.edits)
        if code == LINT_FIELD_REASSIGN:
            fix_field_reassign(fw, span, dry_run)
        elif code == LINT_DROP_NON_DROP:
            fix_drop_non_drop(fw, span, dry_run)
        elif code == LINT_BOOL_ASSERT:
            if not apply_suggestion(fw, msg, dry_run):
                fix_bool_assert(fw, span, dry_run)
        elif code == LINT_LET_RETURN:
            if not apply_suggestion(fw, msg, dry_run):
                fw.skipped.append(f"L{span['line_start']}: {code} 无机器建议，跳过")
        elif code in (LINT_USELESS_VEC, LINT_CLONED_SLICE, LINT_NEEDLESS_BORROW, LINT_IS_MULTIPLE_OF):
            if not apply_suggestion(fw, msg, dry_run):
                if code == LINT_CLONED_SLICE:
                    fix_cloned_slice_fallback(fw, span, dry_run)
                else:
                    fw.skipped.append(f"L{span['line_start']}: {code} 无机器建议，跳过")
        elif code in (LINT_PRECEDENCE, LINT_MANUAL_CONTAINS, LINT_UNUSED_MUT, LINT_UNUSED_VAR):
            if not apply_suggestion(fw, msg, dry_run):
                fw.skipped.append(f"L{span['line_start']}: {code} 无机器建议，跳过")
        if len(fw.edits) == before and not any(f"L{span['line_start']}" in s for s in fw.skipped):
            fw.skipped.append(f"L{span['line_start']}: {code} 未处理（建议非机器可应用）")

    # 应用编辑（每个文件按位置倒序，行级替换）
    total_fixed = 0
    for rel, fw in sorted(files.items()):
        if not fw.edits and not fw.skipped:
            continue
        # 重叠检查 + 倒序应用
        edits = sorted(fw.edits, key=lambda e: e.line_start)
        overlap = any(
            edits[i + 1].line_start <= edits[i].line_end
            for i in range(len(edits) - 1)
        )
        if overlap:
            fw.skipped.append("存在重叠编辑，整文件跳过")
            fw.edits.clear()
        if fw.edits:
            for e in reversed(edits):
                fw.lines[e.line_start : e.line_end + 1] = e.new_lines
                fw.fixed.append(f"  L{e.line_start + 1}: {e.desc}")
            if not dry_run:
                save(fw)
            total_fixed += len(fw.fixed)

    return total_fixed, files, reported


def main() -> int:
    dry_run = "--dry-run" in sys.argv
    total = 0
    reported: dict[str, list[str]] = {}
    last_files: dict[str, FileWork] = {}
    for rnd in range(1, 5):
        print(f"== 第 {rnd} 轮 cargo clippy --workspace --all-targets")
        fixed, last_files, reported = one_pass(dry_run)
        total += fixed
        print(f"   本轮修复 {fixed} 处")
        if fixed:
            for rel, fw in sorted(last_files.items()):
                if fw.fixed:
                    print(f"   {rel}")
                    for f_ in sorted(fw.fixed):
                        print("     [fixed]", f_.strip())
        if fixed == 0 or dry_run:
            break

    print()
    skipped_total = 0
    for rel, fw in sorted(last_files.items()):
        if not fw.skipped:
            continue
        print(f"== {rel}（跳过）")
        for s in fw.skipped:
            print("  [skip]", s)
            skipped_total += 1
    if reported:
        print("\n== 只报告、未修改")
        for code, locs in sorted(reported.items()):
            for loc in locs:
                print(f"  {code}: {loc}")
    print(f"\n== 合计：修复 {total} 处，跳过 {skipped_total} 处" + ("（dry-run，未写盘）" if dry_run else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
