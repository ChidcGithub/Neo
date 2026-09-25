//! `open_file` —— 在用户机器上打开文件。
//!
//! 边界：**只负责"交给系统"**。不读内容、不解析、不等程序退出。
//! 给模型看内容用 [`super::read_file`]；给用户看用本工具。
//! 这是个"可见但无破坏"的动作，风险等级是 [`crate::Risk::Open`]：
//! 默认允许，但策略可以关掉（`Policy::allow_open`）。

use serde_json::json;

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

pub static PARAMS: &[Param] = &[
    Param::text("path", "要打开的文件（或目录）路径，相对工作区。路径必须已存在。"),
    Param::flag(
        "reveal",
        "true 表示在文件管理器里定位并选中该文件（不打开它）；false 用系统默认程序打开。默认 false。",
    ),
];

pub fn preview(args: &Args) -> String {
    let path = args.opt_str("path").unwrap_or_default();
    if args.flag("reveal").unwrap_or(false) {
        format!("在文件管理器中定位 {path}")
    } else {
        format!("用默认程序打开 {path}")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match open(scope, args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("open_file", e),
    }
}

fn open(scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let raw = args.require_str("path")?;
    let reveal = args.flag("reveal")?;

    let path = scope.resolve(&raw)?;
    if !path.exists() {
        return Err(ToolError::not_found(format!("目标不存在：{raw}"))
            .with_hint("用 `bash` 的 `ls` 确认路径；本工具不创建文件"));
    }
    let path = scope.verify_existing(&path)?;
    let shown = scope.display(&path);

    let (program, argv) = launcher(&path, reveal);
    // 只启动、不等待：拉起的是用户自己的程序，本工具不负责它的生命周期。
    std::process::Command::new(&program)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            ToolError::io(format!("无法调用 `{}`：{e}", program.to_string_lossy())).with_hint(
                "该动作依赖系统命令（Windows: cmd/explorer；macOS: open；Linux: xdg-open）",
            )
        })?;

    let action = if reveal { "reveal" } else { "open" };
    Ok(Outcome::ok(
        "open_file",
        if reveal {
            format!("已在文件管理器中定位 {shown}")
        } else {
            format!("已用默认程序打开 {shown}")
        },
        json!({
            "path": shown,
            "action": action,
            "launcher": program.to_string_lossy(),
        }),
    ))
}

/// 按平台选启动方式。返回（程序, 参数）。
fn launcher(path: &std::path::Path, reveal: bool) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    #[cfg(target_os = "windows")]
    {
        if reveal {
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(path);
            ("explorer".into(), vec![arg])
        } else {
            // `start` 是 cmd 内建命令：第一个空引号是"窗口标题"占位，
            // 不写它时带空格的路径会被当成标题。
            (
                "cmd".into(),
                vec!["/C".into(), "start".into(), "".into(), path.into()],
            )
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut argv: Vec<std::ffi::OsString> = Vec::new();
        if reveal {
            argv.push("-R".into());
        }
        argv.push(path.into());
        ("open".into(), argv)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // xdg-open 打开目录等价于"在文件管理器里查看"，不做逐文件选中。
        let target = if reveal {
            path.parent().unwrap_or(path).to_path_buf()
        } else {
            path.to_path_buf()
        };
        ("xdg-open".into(), vec![target.into()])
    }
}
