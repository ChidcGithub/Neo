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

    // 只启动、不等待：拉起的是用户自己的程序，本工具不负责它的生命周期。
    let launcher = launch(&path, reveal)?;

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
            "launcher": launcher,
        }),
    ))
}

/// 拉起文件或文件管理器。路径始终作为独立参数/Shell API 参数传递，
/// 不经过 cmd.exe 之类的命令行再解析。
#[cfg(target_os = "windows")]
fn launch(path: &std::path::Path, reveal: bool) -> Result<String, ToolError> {
    use std::os::windows::ffi::OsStrExt;

    if reveal {
        let root = std::env::var_os("SystemRoot")
            .ok_or_else(|| ToolError::io("无法定位系统目录（SystemRoot 未设置）".to_owned()))?;
        let explorer = std::path::PathBuf::from(root).join("explorer.exe");
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        spawn_quiet(&explorer, &[arg])?;
        return Ok("explorer.exe".to_owned());
    }

    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    if result as usize <= 32 {
        return Err(
            ToolError::io(format!("系统无法用默认程序打开（错误码 {}）", result as usize))
                .with_hint("确认该类型文件有默认打开方式；也可直接把路径告诉用户"),
        );
    }
    Ok("ShellExecuteW".to_owned())
}

#[cfg(target_os = "windows")]
fn spawn_quiet(program: &std::path::Path, args: &[std::ffi::OsString]) -> Result<(), ToolError> {
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| ToolError::io(format!("无法调用 `{}`：{e}", program.display())))
}

#[cfg(not(target_os = "windows"))]
fn launch(path: &std::path::Path, reveal: bool) -> Result<String, ToolError> {
    let (program, argv) = launcher(path, reveal);
    std::process::Command::new(&program)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            ToolError::io(format!("无法调用 `{}`：{e}", program.to_string_lossy())).with_hint(
                "该动作依赖系统命令（macOS: open；Linux: xdg-open）",
            )
        })?;
    Ok(program.to_string_lossy().into_owned())
}

/// 按平台选启动方式。返回（程序, 参数）。
#[cfg(not(target_os = "windows"))]
fn launcher(path: &std::path::Path, reveal: bool) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
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
