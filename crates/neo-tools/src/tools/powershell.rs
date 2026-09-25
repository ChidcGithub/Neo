//! `powershell` —— 在工作区内执行 PowerShell 命令。
//!
//! 边界：**这是能力上界最高、也最难审计的一个**。约定是
//! 「能用 read_file / write_file / edit_file / view_image 表达的，就不要用 shell」——
//! 前者回传结构化的结果，后者回传一坨回显，模型和用户都更难核对。
//!
//! ## 为什么默认宿主是 PowerShell
//!
//! Neo 跑在教室一体机（Windows）上：那里一定有 Windows PowerShell。
//! 需要换宿主时用 `NEO_SHELL` 指一个可执行文件。
//! 想要类 Unix 环境（`ls` / `grep` / `sed` / `git`）请用姊妹工具 `bash`，
//! 它走的是**随包提供的 Git Bash 运行时**。
//!
//! ## 三个工程细节（都是实测踩出来的）
//!
//! 1. **输出编码**：Windows PowerShell 5.1 按控制台 OEM 代码页（中文机器上是 GBK）
//!    写出，我们按 UTF-8 解码就全是乱码。所以命令前先钉住
//!    `[Console]::OutputEncoding = UTF8`。
//! 2. **退出码**：`-Command` 模式下 PowerShell 自己不会把原生程序的退出码带出来，
//!    所以命令尾部补 `if ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE }` ——
//!    否则 `cargo test` 失败也报 0，工具结果就骗人了。
//! 3. **错误即终止**：`$ErrorActionPreference = 'Stop'` 让 cmdlet 的错误变成
//!    终止错误（退出码 1），而不是"打一行红字继续跑完"。
//!
//! 执行、超时、后台、输出收敛与结果形状由 [`super::shell`] 统一实现 ——
//! 本文件只留**给用户看的文字**。

use crate::result::{Args, Outcome};
use crate::spec::Param;
use crate::Scope;

use super::shell::{self, ShellKind};

const KIND: ShellKind = ShellKind::PowerShell;

pub static PARAMS: &[Param] = &[
    Param::text(
        "command",
        "要执行的 PowerShell 命令（Windows PowerShell 5.1 语法，**不是 bash**）。\
         多条语句用 `;` 或换行分隔（5.1 不支持 `&&`）。\
         常用对应：`ls`=Get-ChildItem、`cat`=Get-Content、`rm -rf x`=Remove-Item -Recurse -Force x、\
         `grep`=Select-String、丢弃输出用 `> $null`（不是 `/dev/null`）。\
         要写 bash 语法请改用 `bash` 工具。",
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
        format!("{verb} PowerShell：{head}")
    } else {
        format!("在 {cwd} {verb} PowerShell：{head}")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match shell::exec(KIND, "powershell", scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("powershell", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timeout_default_is_500s_and_range_allows_it() {
        let p = PARAMS
            .iter()
            .find(|p| p.name == "timeout_ms")
            .expect("timeout_ms 参数存在");
        assert_eq!(p.default, Some(crate::spec::Default::Int(500_000)));
        let (lo, hi) = p.range.expect("有区间");
        assert!(lo < 500_000 && hi >= 500_000, "区间必须容得下默认值");
        assert_eq!(hi, 1_800_000);
    }

    #[test]
    fn background_flag_defaults_off() {
        let p = PARAMS
            .iter()
            .find(|p| p.name == "background")
            .expect("background 参数存在");
        assert!(!p.required);
        assert_eq!(p.default, Some(crate::spec::Default::Bool(false)));
    }

    #[test]
    fn preview_says_background_when_asked() {
        let tool = crate::find("powershell").unwrap();

        let v = json!({ "command": "npm run dev", "background": true });
        assert!(preview(&Args::new(tool, &v)).contains("后台执行"));

        let v = json!({ "command": "cargo test" });
        assert!(!preview(&Args::new(tool, &v)).contains("后台"));
    }

    /// 参数说明必须点明"不是 bash，要 bash 请换工具" —— 模型不看文档也要能选对。
    #[test]
    fn command_hint_warns_about_shell_dialect() {
        let p = PARAMS.iter().find(|p| p.name == "command").unwrap();
        assert!(p.desc.contains("不是 bash"), "说明里要标明方言");
        assert!(p.desc.contains("`bash` 工具"), "要指向姊妹工具");
    }
}
