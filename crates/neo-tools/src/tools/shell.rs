//! 两个 shell 工具（`powershell` / `bash`）共用的宿主解析与执行骨架。
//!
//! **为什么单独一层**：这两个工具的差别只有三处 —— 参数说明里的语法提示、
//! 找宿主的顺序、怎么把命令交给宿主。执行、超时、后台、输出收敛、结果形状
//! 完全一样。把这些分开写会漂移（改一处忘一处），所以差异点收在这里，
//! 每个工具文件只留**给用户看的文字**。
//!
//! ## 后台模式的语义
//!
//! | 模式 | `background` | 行为 |
//! |---|---|---|
//! | 前台（默认） | `false` | 等命令结束，捕获 stdout/stderr，返回退出码与耗时；所有退出路径清理整树 |
//! | 后台 | `true` | 启动即返回（带 shell PID），**不等待、不捕获输出**，不随调用超时/取消清理 |
//!
//! 要保留 GUI / 长驻子进程必须显式选后台；前台中的 `Start-Process` 不改变归属。
//! 后台不创建本工具的前台 Job，但不保证逃离 Neo 自身继承的外部 Job。
//!
//! ## 两条贯穿的约定
//!
//! 1. **`ok = true` 只表示"命令跑起来了"**，成败看 `data.exit_code`。
//!    这样"工具执行失败"（`error` 字段）与"命令返回非零"是两件可区分的事。
//! 2. **输出超限就截断并标记**（各 64 KiB），否则一条 `-Recurse` 的回显
//!    就能把上下文吃光。
//!
//! > 本层是**同步**的（一次调用一次返回）；"不阻塞界面"由上层把整次调用
//! > 丢到后台线程实现 —— 见 `neo-app` 的 `spawn_ready_tools`。

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::Scope;

use super::limits;

/// 用哪个宿主跑命令。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShellKind {
    /// Windows PowerShell / PowerShell 7。
    PowerShell,
    /// 类 Unix 环境（Git Bash / MSYS2 bash）。
    Bash,
}

impl ShellKind {
    /// 结果与错误里用来称呼这个宿主的名。
    pub fn name(self) -> &'static str {
        match self {
            ShellKind::PowerShell => "PowerShell",
            ShellKind::Bash => "bash",
        }
    }

    /// 换宿主的出路，拼进错误提示里。
    fn override_env(self) -> &'static str {
        match self {
            ShellKind::PowerShell => "NEO_SHELL",
            ShellKind::Bash => "NEO_GIT_BASH",
        }
    }
}

/// 选中的宿主。
pub struct Host {
    pub executable: PathBuf,
    /// 结果里 `host` 字段的取值，如 `gitbash` / `powershell` / `pwsh`。
    pub kind: &'static str,
}

// ---------------------------------------------------------------------------
// 宿主解析
// ---------------------------------------------------------------------------

/// 找宿主。顺序对两个 shell 都是「**显式指定 > 随包运行时 > 系统安装**」。
pub fn resolve(kind: ShellKind) -> Result<Host, ToolError> {
    match kind {
        ShellKind::PowerShell => resolve_powershell(),
        ShellKind::Bash => resolve_bash(),
    }
}

fn resolve_powershell() -> Result<Host, ToolError> {
    if let Ok(custom) = std::env::var("NEO_SHELL") {
        let custom = custom.trim().to_owned();
        if !custom.is_empty() {
            let kind = if custom.to_ascii_lowercase().contains("pwsh") {
                "pwsh"
            } else {
                "powershell"
            };
            return Ok(Host {
                executable: PathBuf::from(custom),
                kind,
            });
        }
    }
    if let Some(pwsh) = which_one(&["pwsh.exe", "pwsh"]) {
        return Ok(Host {
            executable: pwsh,
            kind: "pwsh",
        });
    }
    #[cfg(target_os = "windows")]
    {
        // 优先用已知路径，PATH 被裁剪过的环境下也能起来。
        if let Some(sysroot) = std::env::var_os("SystemRoot") {
            let known = PathBuf::from(sysroot)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe");
            if known.is_file() {
                return Ok(Host {
                    executable: known,
                    kind: "powershell",
                });
            }
        }
        Ok(Host {
            executable: PathBuf::from("powershell.exe"),
            kind: "powershell",
        })
    }
    #[cfg(not(target_os = "windows"))]
    Err(
        ToolError::new(ErrorKind::Unsupported, "本机没有找到 PowerShell（pwsh）")
            .with_hint("装 PowerShell 7（pwsh），或用 NEO_SHELL 指定可执行文件路径"),
    )
}

/// Git Bash 的解析顺序（Windows）：
///
/// 1. `NEO_GIT_BASH` —— 显式指定，最高优先；
/// 2. **随包运行时** `runtime/gitbash/`（由 `tools/fetch_runtime.py` 下载的 MinGit，
///    脚本会把 `usr/bin/sh.exe` 硬链接成 `usr/bin/bash.exe`），
///    这样**没装 Git 的机器也能用** —— 这是一体机场景的兜底；
/// 3. PATH 里的 `bash.exe`（**排除** `WindowsApps\bash.exe` —— 那是 WSL 的
///    应用执行别名，不是 Git Bash，跑起来会去启动一个 Linux 发行版）；
/// 4. PATH 里的 `git.exe` → 反推它同级的 `bin/bash.exe`（Git for Windows 的
///    标准安装只把 `cmd` 放进 PATH，那里**没有** bash.exe，所以要靠 git 反推）；
/// 5. 常见安装位置（`%ProgramFiles%`、`%LOCALAPPDATA%\Programs` 等）；
/// 6. 兜底：在 C..G 盘的 `\Program Files\Git` 下找一份（装在 D 盘又没进 PATH 的情况）。
fn resolve_bash() -> Result<Host, ToolError> {
    if let Ok(custom) = std::env::var("NEO_GIT_BASH") {
        let custom = custom.trim().to_owned();
        if !custom.is_empty() {
            let p = PathBuf::from(custom);
            return Ok(Host {
                executable: p,
                kind: "gitbash",
            });
        }
    }

    for candidate in bash_candidates() {
        if candidate.is_file() {
            let kind = if candidate
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("gitbash")
            {
                "gitbash-bundled"
            } else {
                "gitbash"
            };
            return Ok(Host {
                executable: candidate,
                kind,
            });
        }
    }

    Err(ToolError::new(
        ErrorKind::Unsupported,
        "本机没有可用的 Git Bash（类 Unix 环境）",
    )
    .with_hint(
        "三条出路：① 运行 `python tools/fetch_runtime.py` 让运行时随包提供；\
         ② 装 Git for Windows；③ 用 NEO_GIT_BASH 指定 bash.exe 的完整路径",
    ))
}

/// 逐个候选位置。**顺序即优先级**，`is_file` 由调用方判。
fn bash_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    // 2) 随包运行时
    //
    // 注意 `usr/bin/sh.exe`：MinGit 里**没有 `bash.exe`** —— 它把 bash 装成了
    // `sh.exe`（同一个二进制）。`fetch_runtime.py` 会把它硬链接成 `bash.exe`，
    // 但万一那一步失败，用 `sh.exe` 也能跑（代价是 bash 进 POSIX 模式）。
    for root in runtime_roots() {
        out.push(root.join("bin").join("bash.exe"));
        out.push(root.join("usr").join("bin").join("bash.exe"));
        out.push(root.join("usr").join("bin").join("sh.exe"));
    }

    // 3) PATH 里的 bash.exe（排除 WSL 的应用执行别名）
    for dir in path_dirs() {
        out.push(dir.join("bash.exe"));
    }

    // 4) 从 PATH 里的 git.exe 反推
    for dir in path_dirs() {
        let git = dir.join("git.exe");
        if git.is_file() {
            out.extend(derive_from_git(&git));
        }
    }

    // 5) 常见安装位置
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(base) = std::env::var_os(var) {
            let git = PathBuf::from(base).join("Git");
            out.push(git.join("bin").join("bash.exe"));
            out.push(git.join("usr").join("bin").join("bash.exe"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let git = PathBuf::from(local).join("Programs").join("Git");
        out.push(git.join("bin").join("bash.exe"));
    }

    // 6) 盘符兜底：装在非系统盘又没进 PATH 的情况（一体机上常见）
    #[cfg(target_os = "windows")]
    for drive in ["C", "D", "E", "F", "G"] {
        out.push(PathBuf::from(format!(
            r"{drive}:\Program Files\Git\bin\bash.exe"
        )));
    }

    out.retain(|p| !is_wsl_stub(p));
    out
}

/// 从一份 `git.exe` 反推同装 Git 的 bash.exe。
///
/// `git.exe` 一般落在 `<root>\cmd\`（标准安装）或 `<root>\mingw64\bin\`（PortableGit）。
fn derive_from_git(git: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(bin) = git.parent() else {
        return out;
    };
    // 同一层（PortableGit 的 mingw64\bin）
    out.push(bin.join("bash.exe"));
    if let Some(root) = bin.parent() {
        out.push(root.join("bin").join("bash.exe"));
        out.push(root.join("usr").join("bin").join("bash.exe"));
    }
    out
}

/// `WindowsApps\bash.exe` 是 WSL 的应用执行别名（0 字节的转发点），
/// 不是 Git Bash：跑它等于去启动一个 Linux 发行版。必须排除。
fn is_wsl_stub(p: &Path) -> bool {
    p.to_string_lossy()
        .to_ascii_lowercase()
        .contains("windowsapps")
}

/// `runtime/` 可能在哪。
///
/// 可执行文件所在目录**及其上溯几层**（`cargo run` 时 exe 在 `target/debug/`，
/// 往上是仓库根），再加上当前目录 —— 开发时和装好之后都能找到同一份运行时。
/// `NEO_RUNTIME` 可直接指到 runtime 目录本身。
fn runtime_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(dir) = std::env::var("NEO_RUNTIME") {
        let dir = dir.trim();
        if !dir.is_empty() {
            out.push(PathBuf::from(dir));
        }
    }
    let bases = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf)),
        std::env::current_dir().ok(),
    ];
    for base in bases.into_iter().flatten() {
        let mut dir = base;
        for _ in 0..5 {
            out.push(dir.join("runtime").join("gitbash"));
            let Some(parent) = dir.parent().map(Path::to_path_buf) else {
                break;
            };
            dir = parent;
        }
    }
    out
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

/// 在 PATH 里找一个可执行文件。
fn which_one(names: &[&str]) -> Option<PathBuf> {
    for dir in path_dirs() {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

/// 前台/后台共用的一条执行路径。
pub fn exec(
    kind: ShellKind,
    tool: &'static str,
    scope: &Scope,
    args: &Args,
) -> Result<Outcome, ToolError> {
    let command = args.require_str("command")?;
    let timeout_ms = args.opt_int("timeout_ms")? as u64;
    let background = args.flag("background")?;

    let cwd = match args.opt_str("cwd")? {
        s if s.trim().is_empty() => scope.root().to_path_buf(),
        s => {
            let p = scope.resolve(&s)?;
            if !p.is_dir() {
                return Err(ToolError::bad_args(format!("cwd `{s}` 不是目录"))
                    .with_hint(format!("用 `{}` 确认目录存在", dir_hint(kind))));
            }
            scope.verify_existing(&p)?
        }
    };

    let host = resolve(kind)?;
    let shown_cwd = scope.display(&cwd);
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }

    if background {
        let child =
            spawn(kind, &host, &cwd, &command, false).map_err(|e| spawn_error(kind, &host, &e))?;
        let pid = child.id();
        // 只报告"已启动"：句柄丢弃后进程继续跑（关句柄 ≠ 杀进程）。
        drop(child);
        let head: String = command.chars().take(60).collect();
        return Ok(Outcome::ok(
            tool,
            format!("已在后台启动：`{head}`（PID {pid}）"),
            json!({
                "command": command,
                "host": host.kind,
                "executable": host.executable.to_string_lossy(),
                "cwd": shown_cwd,
                "background": true,
                "pid": pid,
                "timeout_applies": false,
                "note": "后台模式不等待、不捕获输出，启动后不随本次调用的超时或取消清理。PID 是 shell PID，不一定是 GUI / 服务 PID；成功只表示 shell 已启动。要看输出请自行重定向到工作区内的文件（PowerShell 用 `*>`，bash 用 `>` / `2>&1`）再用 read_file 读；结束进程用任务管理器或系统的 kill。",
            }),
        ));
    }

    let started = Instant::now();
    let mut cmd = shell_command(kind, &host, &cwd, &command, true);
    let captured = run_foreground(&mut cmd, scope, Duration::from_millis(timeout_ms))?;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let exit_code = captured.status.code().unwrap_or(-1);
    let (stdout, out_cut) = clip(&captured.stdout);
    let (stderr, err_cut) = clip(&captured.stderr);

    let head: String = command.chars().take(60).collect();
    let summary = if exit_code == 0 {
        format!("`{head}` 执行成功（{elapsed_ms} ms）")
    } else {
        format!("`{head}` 退出码 {exit_code}（{elapsed_ms} ms）")
    };

    Ok(Outcome::ok(
        tool,
        summary,
        json!({
            "command": command,
            "host": host.kind,
            "executable": host.executable.to_string_lossy(),
            "cwd": shown_cwd,
            "background": false,
            "exit_code": exit_code,
            "duration_ms": elapsed_ms,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": out_cut,
            "stderr_truncated": err_cut,
        }),
    ))
}

/// 出错时该建议用户敲什么命令列目录（两个宿主语法不同）。
fn dir_hint(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::PowerShell => "Get-ChildItem",
        ShellKind::Bash => "ls",
    }
}

/// 起一个宿主进程。`capture` 决定是否接管输出管道。
///
/// 两个宿主的 argv 差异在这里，**别的地方不用关心**。
fn spawn(
    kind: ShellKind,
    host: &Host,
    cwd: &Path,
    command: &str,
    capture: bool,
) -> std::io::Result<Child> {
    shell_command(kind, host, cwd, command, capture).spawn()
}

fn shell_command(
    kind: ShellKind,
    host: &Host,
    cwd: &Path,
    command: &str,
    capture: bool,
) -> Command {
    let io = if capture {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut cmd = Command::new(&host.executable);
    match kind {
        ShellKind::PowerShell => {
            // `-NoProfile`：不加载用户 profile，输出与行为才是可预期的。
            // `-NonInteractive`：任何需要交互的 cmdlet 直接报错，而不是挂住。
            cmd.arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-Command")
                .arg(wrap_powershell(command));
        }
        ShellKind::Bash => {
            // `--noprofile --norc`：同上，不读用户配置，输出可预期。
            // 不做 `set -o pipefail`：那会改变命令的语义，用户看到的应当
            // 和自己在 Git Bash 里敲的一样。
            cmd.arg("--noprofile").arg("--norc").arg("-c").arg(command);
            // 钉住 locale，免得某些工具在未设 locale 时按单字节处理 UTF-8。
            cmd.env("LANG", "C.UTF-8")
                .env("LC_ALL", "C.UTF-8")
                .env("TERM", "dumb");
            // MinGit 的 bash 位于 usr/bin，但裸 `-c` 不加载 profile：继承的 Windows
            // PATH 通常没有这个目录，于是 ls/grep/sed 明明随包却报 command not found。
            let mut paths = vec![host.executable.parent().unwrap_or(cwd).to_path_buf()];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            if let Ok(path) = std::env::join_paths(paths) {
                cmd.env("PATH", path);
            }
        }
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(io)
        .stderr(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    cmd
}

/// PowerShell 的命令前置：钉住输出编码、静音进度条、错误即终止。
const PS_PREAMBLE: &str = concat!(
    "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ",
    "$ProgressPreference = 'SilentlyContinue'; ",
    "$ErrorActionPreference = 'Stop'"
);

/// PowerShell 的命令后置：把原生程序的退出码带出来（`-Command` 自己不传）。
const PS_EPILOGUE: &str = "if ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE }";

/// 拼出真正要交给 PowerShell `-Command` 的脚本。
pub fn wrap_powershell(command: &str) -> String {
    format!("{PS_PREAMBLE}; {command}; {PS_EPILOGUE}")
}

/// 起不来的错误说明（含"可以换宿主"的出路）。
fn spawn_error(kind: ShellKind, host: &Host, e: &std::io::Error) -> ToolError {
    let env = kind.override_env();
    ToolError::io(format!(
        "无法启动 `{}`：{e}",
        host.executable.to_string_lossy()
    ))
    .with_hint(format!(
        "可用 {env} 指定{}可执行文件的完整路径",
        kind.name()
    ))
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const CLEANUP_TIMEOUT: Duration = Duration::from_millis(500);

struct Captured {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// 前台所有退出路径都收回所属进程树。后台调用不进入此生命周期。
struct Foreground {
    child: Child,
    tree: process_tree::Tree,
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = self.tree.terminate();
        let _ = self.child.kill();
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

fn run_foreground(
    cmd: &mut Command,
    scope: &Scope,
    timeout: Duration,
) -> Result<Captured, ToolError> {
    if scope.is_cancelled() {
        return Err(crate::cancelled_error());
    }
    let started = Instant::now();
    let mut process = process_tree::spawn(cmd)
        .map_err(|e| ToolError::io(format!("无法安全启动前台命令：{e}")))?;
    let mut out = process.child.stdout.take();
    let mut err = process.child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    let mut cleanup_deadline = None;
    loop {
        if scope.is_cancelled() {
            return Err(crate::cancelled_error());
        }
        if started.elapsed() >= timeout {
            return Err(ToolError::new(
                ErrorKind::Timeout,
                "前台命令超时，正在收回所属进程树；已发生的操作不会回滚",
            )
            .with_hint("拆小命令或提高 timeout_ms；长驻进程请显式使用 background: true"));
        }
        if status.is_none() {
            status = process
                .child
                .try_wait()
                .map_err(|e| ToolError::io(format!("等待命令结束失败：{e}")))?;
            if status.is_some() {
                // 前台拥有整树：正常退出也清理后代，是否继承输出管道无关。
                // GUI / 长驻程序应显式走 background，不能仅移除此处终止或 Job 的兜底。
                process
                    .tree
                    .terminate()
                    .map_err(|e| ToolError::io(format!("终止前台进程树失败：{e}")))?;
                cleanup_deadline = Some(Instant::now() + CLEANUP_TIMEOUT);
            }
        }
        drain_ready(&mut out, &mut stdout)
            .and_then(|()| drain_ready(&mut err, &mut stderr))
            .map_err(|e| ToolError::io(format!("收集命令输出失败：{e}")))?;
        if let Some(status) = status {
            if out.is_none() && err.is_none() {
                return Ok(Captured {
                    status,
                    stdout,
                    stderr,
                });
            }
            if cleanup_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(ToolError::new(
                    ErrorKind::Timeout,
                    "命令已退出，但输出管道未在清理期限内关闭；已停止收集输出",
                ));
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// 每轮限定读取量，避免持续大量输出饿死取消/超时检查。不创建可能永久阻塞的读线程。
fn drain_ready<R: Read + process_tree::Pipe>(
    pipe: &mut Option<R>,
    kept: &mut Vec<u8>,
) -> std::io::Result<()> {
    for _ in 0..32 {
        let Some(reader) = pipe.as_mut() else { break };
        let Some(available) = process_tree::readable(reader)? else {
            break;
        };
        if available == 0 {
            *pipe = None;
            break;
        }
        let mut buf = [0u8; 8192];
        let count = available.min(buf.len());
        match reader.read(&mut buf[..count]) {
            Ok(0) => {
                *pipe = None;
                break;
            }
            Ok(n) => {
                let room = limits::OUTPUT_BYTES.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
mod process_tree {
    use super::*;
    use std::io;
    use std::os::windows::{io::AsRawHandle, process::CommandExt};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_BROKEN_PIPE, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Pipes::PeekNamedPipe,
            Threading::{
                OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
            },
        },
    };

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub struct Tree(Handle);
    impl Tree {
        pub fn terminate(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0 .0, 1) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    fn new_tree() -> io::Result<Tree> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let tree = Tree(Handle(raw));
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(tree)
    }

    pub fn spawn(cmd: &mut Command) -> io::Result<Foreground> {
        spawn_in_tree(cmd, new_tree()?)
    }

    fn spawn_in_tree(cmd: &mut Command, tree: Tree) -> io::Result<Foreground> {
        // 不使用 BREAKAWAY：允许嵌套在宿主 Job 内；不兼容的宿主 Job 必须失败关闭。
        // 主线程挂起后先 assign，绝不让用户脚本在未受保护的窗口内执行。
        cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
        let child = cmd.spawn()?;
        let process = Foreground { child, tree };
        if unsafe { AssignProcessToJobObject(process.tree.0 .0, process.child.as_raw_handle()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        resume(&process.child)?;
        Ok(process)
    }

    #[cfg(test)]
    pub(super) fn spawn_with_failing_assignment(cmd: &mut Command) -> io::Result<Foreground> {
        use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        let tree = new_tree()?;
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        // 把活动进程数限制为 0，使 assign 确定失败；用户脚本仍处于挂起状态。
        info.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
        info.BasicLimitInformation.ActiveProcessLimit = 0;
        if unsafe {
            SetInformationJobObject(
                tree.0 .0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        spawn_in_tree(cmd, tree)
    }

    fn resume(child: &Child) -> io::Result<()> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = Handle(snapshot);
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of_val(&entry) as u32;
        let mut found = unsafe { Thread32First(snapshot.0, &mut entry) };
        while found != 0 {
            if entry.th32OwnerProcessID == child.id() {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = Handle(thread);
                if unsafe { ResumeThread(thread.0) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.0, &mut entry) };
        }
        Err(io::Error::other("未找到挂起的 shell 主线程"))
    }

    pub trait Pipe: AsRawHandle {}
    impl<T: AsRawHandle> Pipe for T {}
    pub fn readable(pipe: &impl Pipe) -> io::Result<Option<usize>> {
        let mut available = 0;
        if unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        } == 0
        {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                Ok(Some(0))
            } else {
                Err(e)
            };
        }
        Ok((available > 0).then_some(available as usize))
    }
}

#[cfg(unix)]
mod process_tree {
    use super::*;
    use std::io;
    use std::os::{fd::AsRawFd, unix::process::CommandExt};

    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
        fn poll(fds: *mut PollFd, count: Nfds, timeout: i32) -> i32;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    type Nfds = std::ffi::c_ulong;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    type Nfds = u32;
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    pub struct Tree(i32, std::cell::Cell<bool>);
    impl Tree {
        pub fn terminate(&self) -> io::Result<()> {
            if self.1.replace(true) {
                return Ok(());
            }
            // 独立进程组，绝不向宿主或其它调用的进程组发信号。
            if unsafe { kill(-self.0, 9) } == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(3) {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
    pub fn spawn(cmd: &mut Command) -> io::Result<Foreground> {
        let child = cmd.process_group(0).spawn()?;
        let tree = Tree(child.id() as i32, std::cell::Cell::new(false));
        Ok(Foreground { child, tree })
    }
    pub trait Pipe: AsRawFd {}
    impl<T: AsRawFd> Pipe for T {}
    pub fn readable(pipe: &impl Pipe) -> io::Result<Option<usize>> {
        let mut fd = PollFd {
            fd: pipe.as_raw_fd(),
            events: 1,
            revents: 0,
        };
        let ready = unsafe { poll(&mut fd, 1, 0) };
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((ready > 0).then_some(8192))
    }
}

#[cfg(not(any(windows, unix)))]
mod process_tree {
    use super::*;
    use std::io;
    pub struct Tree;
    impl Tree {
        pub fn terminate(&self) -> io::Result<()> {
            Ok(())
        }
    }
    pub fn spawn(_: &mut Command) -> io::Result<Foreground> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "本平台不支持受保护的前台进程树",
        ))
    }
    pub trait Pipe {}
    impl<T> Pipe for T {}
    pub fn readable(_: &impl Pipe) -> io::Result<Option<usize>> {
        Ok(Some(0))
    }
}

/// 字节 → 文本，超限截断并标记。
pub fn clip(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() >= limits::OUTPUT_BYTES;
    let text = String::from_utf8_lossy(bytes).into_owned();
    if truncated {
        (
            format!("{text}\n…（输出超过 {} 字节已截断）", limits::OUTPUT_BYTES),
            true,
        )
    } else {
        (text, false)
    }
}

/// 后台模式的说明句（两个工具的 `background` 参数说明共用，只有重定向写法不同）。
pub fn background_note(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::PowerShell => {
            "要看后台输出请自行重定向，例如 `... *> run.log`（`*>` 收全部流），再用 read_file 读。"
        }
        ShellKind::Bash => {
            "要看后台输出请自行重定向，例如 `... > run.log 2>&1`，再用 read_file 读。"
        }
    }
}

/// 把 `Vec<OsString>` 拼成一行，只为在测试/日志里看得清。
#[allow(dead_code)]
pub fn argv_debug(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
