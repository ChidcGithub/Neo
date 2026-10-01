//! `bash` —— 在工作区内的**类 Unix 环境**里执行命令（Git Bash / MSYS2）。
//!
//! 与 [`super::powershell`] 是一对：同一个执行骨架（[`super::shell`]），
//! 差别只在宿主的语法与找宿主的顺序。
//!
//! ## 宿主从哪来：**随包提供**
//!
//! 一体机上不一定装了 Git，所以运行时随包提供 —— `tools/fetch_runtime.py`
//! 把 Git for Windows 的 MinGit 解到 `runtime/gitbash/`，本工具优先用它。
//! 这样"这个工具真的能用"不依赖用户先装 Git。
//!
//! 解析顺序（`neo_tools::tools::shell::resolve_bash` 的实现）：
//!
//! 1. `NEO_GIT_BASH` —— 显式指定；
//! 2. **随包运行时** `runtime/gitbash/bin/bash.exe`；
//! 3. PATH 里的 `bash.exe`（**排除** `WindowsApps\bash.exe`，那是 WSL 的别名）；
//! 4. PATH 里的 `git.exe` → 反推同装的 `bin/bash.exe`（标准安装只把 `cmd` 放进 PATH，
//!    那里没有 bash.exe）；
//! 5. 常见安装位置；
//! 6. C..G 盘的 `\Program Files\Git`（装在非系统盘又没进 PATH 的情况）。
//!
//! ## 什么时候该用它，什么时候不该
//!
//! Git Bash 给的是一整套 Unix 工具（`ls` / `grep` / `sed` / `awk` / `find` / `git`），
//! 处理文本、跑 git、批量改文件都比 PowerShell 顺手，也是模型最熟的方言。
//! 但**这仍然是"执行命令"，能力上界最高、最难审计**：能用
//! `read_file` / `write_file` / `edit_file` / `view_image` 表达的，就不要用它。
//!
//! ## 环境约定
//!
//! - 不读用户配置（`--noprofile --norc`），输出可预期；
//! - `LANG`/`LC_ALL` 钉在 `C.UTF-8`：否则某些工具在未设 locale 时按单字节处理 UTF-8；
//! - **不开 `pipefail`**：那会改变命令语义。用户看到的应当和自己在 Git Bash 里敲的一样。
//!
//! > 反向注意：MSYS2 会把"看起来像 Unix 路径"的**参数**自动转成 Windows 路径。
//! > 这是 Git Bash 的默认行为，这里**不干预** —— 想要可预期，就让行为与用户手敲一致。

use crate::result::{Args, Outcome};
use crate::spec::Param;
use crate::Scope;

use super::shell::{self, ShellKind};

const KIND: ShellKind = ShellKind::Bash;

pub static PARAMS: &[Param] = &[
    Param::text(
        "command",
        "要执行的 bash 命令（Git Bash / MSYS2 的类 Unix 环境，**不是 PowerShell**）。\
         多步用 `&&` / `;` / 换行。常用：`ls -la`、`grep -rn 关键词 .`、\
         `sed -n '1,20p' 文件`、`find . -name '*.rs'`、`git status`、\
         丢弃输出用 `> /dev/null 2>&1`。\
         要写 PowerShell 语法请改用 `powershell` 工具。",
    ),
    Param::opt_text(
        "cwd",
        "执行目录，相对工作区；默认工作区根目录。不能指向工作区之外。",
    ),
    Param::opt_int(
        "timeout_ms",
        "前台模式的超时毫秒数。默认 500000（500 秒，够编译与测试），上限 1800000。\
         超时会杀掉进程并返回 timeout。后台模式忽略此参数。",
        500_000,
        100,
        1_800_000,
    ),
    Param::flag(
        "background",
        "true = 后台启动：立即返回 PID，不等命令结束、不捕获输出。\
         起服务/长驻进程时用它；编译测试这类要结果的仍用默认的 false。",
    ),
];

pub fn preview(args: &Args) -> String {
    let cmd = args.opt_str("command").unwrap_or_default();
    let cwd = args.opt_str("cwd").unwrap_or_default();
    let head: String = cmd.chars().take(120).collect();
    let verb = if args.flag("background").unwrap_or(false) {
        "后台执行"
    } else {
        "执行"
    };
    if cwd.is_empty() {
        format!("{verb} Git Bash：{head}")
    } else {
        format!("在 {cwd} {verb} Git Bash：{head}")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match shell::exec(KIND, "bash", scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("bash", e),
    }
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
