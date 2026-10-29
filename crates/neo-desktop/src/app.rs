//! 应用启动：Start Menu 快捷方式、可执行文件、窗口出现检测。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::error::{DesktopError, Result};
use crate::window::{self, WindowInfo};

#[derive(Clone, Debug)]
pub struct LaunchResult {
    pub pid: u32,
    pub window: Option<WindowInfo>,
    pub detected: bool,
}

/// 按 Start Menu 显示名/快捷方式名启动；找不到快捷方式时回退到 PATH。
pub fn launch(name: &str) -> Result<LaunchResult> {
    #[cfg(not(windows))]
    {
        let _ = name;
        return Err(DesktopError::unsupported("应用启动只在 Windows 上可用"));
    }
    #[cfg(windows)]
    {
        let name = name.trim();
        if name.is_empty() {
            return Err(DesktopError::bad_args("应用名称不能为空"));
        }

        let direct = Path::new(name);
        if direct.is_file() {
            return launch_executable(direct, &[], None);
        }

        if let Some(link) = find_start_menu_link(name)? {
            let pid = shell_execute(&link)?;
            let window = wait_for_window(name, pid, Duration::from_secs(8));
            return Ok(LaunchResult {
                pid,
                detected: window.is_some(),
                window,
            });
        }

        // 常见内置应用（notepad、mspaint…）没有可枚举的 .lnk 时，交给 PATH。
        if !name.contains(['\\', '/']) {
            match std::process::Command::new(name)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    let pid = child.id();
                    let window = wait_for_window(name, pid, Duration::from_secs(8));
                    return Ok(LaunchResult {
                        pid,
                        detected: window.is_some(),
                        window,
                    });
                }
                Err(error) => {
                    return Err(DesktopError::not_found(format!(
                        "Start Menu 和 PATH 都找不到 {name:?}（{error}）"
                    )));
                }
            }
        }

        Err(DesktopError::not_found(format!(
            "Start Menu 找不到应用 {name:?}"
        )))
    }
}

pub fn launch_executable(
    executable: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<LaunchResult> {
    if !executable.is_file() {
        return Err(DesktopError::not_found(format!(
            "可执行文件不存在：{}",
            executable.display()
        )));
    }
    if let Some(cwd) = cwd {
        if !cwd.is_dir() {
            return Err(DesktopError::not_found(format!(
                "工作目录不存在：{}",
                cwd.display()
            )));
        }
    }
    let mut command = std::process::Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let child = command.spawn().map_err(|error| {
        DesktopError::io(format!("启动 {} 失败：{error}", executable.display()))
    })?;
    let pid = child.id();
    let query = executable
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let window = wait_for_window(query, pid, Duration::from_secs(8));
    Ok(LaunchResult {
        pid,
        detected: window.is_some(),
        window,
    })
}

fn wait_for_window(query: &str, pid: u32, timeout: Duration) -> Option<WindowInfo> {
    let deadline = Instant::now() + timeout;
    let needle = query.to_lowercase();
    loop {
        if let Ok(windows) = window::list() {
            if let Some(found) = windows.iter().find(|item| {
                item.process_id == pid
                    || (!needle.is_empty() && item.title.to_lowercase().contains(&needle))
            }) {
                return Some(found.clone());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

#[cfg(windows)]
fn find_start_menu_link(query: &str) -> Result<Option<PathBuf>> {
    let mut roots = Vec::new();
    for folder in [
        windows::Win32::UI::Shell::FOLDERID_StartMenu,
        windows::Win32::UI::Shell::FOLDERID_CommonStartMenu,
    ] {
        if let Ok(path) = known_folder(&folder) {
            roots.push(path);
        }
    }
    if roots.is_empty() {
        return Ok(None);
    }

    let needle = query.to_lowercase();
    let mut best: Option<(f64, PathBuf)> = None;
    let mut visited = 0usize;
    for root in roots {
        scan_links(&root, 0, &needle, &mut best, &mut visited);
        if visited >= 2_000 {
            break;
        }
    }
    Ok(best.map(|(_, path)| path))
}

#[cfg(not(windows))]
fn find_start_menu_link(_query: &str) -> Result<Option<PathBuf>> {
    Ok(None)
}

#[cfg(windows)]
fn scan_links(
    dir: &Path,
    depth: usize,
    needle: &str,
    best: &mut Option<(f64, PathBuf)>,
    visited: &mut usize,
) {
    if depth > 4 || *visited >= 2_000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        *visited += 1;
        if *visited >= 2_000 {
            return;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            scan_links(&path, depth + 1, needle, best, visited);
            continue;
        }
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let stem = stem.to_lowercase();
        let score = if stem == needle {
            1_000.0
        } else if stem.starts_with(needle) {
            900.0
        } else if stem.contains(needle) {
            800.0
        } else {
            strsim::jaro_winkler(needle, &stem) * 100.0
        };
        if score >= 80.0 && best.as_ref().is_none_or(|(current, _)| score > *current) {
            *best = Some((score, path));
        }
    }
}

#[cfg(windows)]
fn known_folder(id: &windows::core::GUID) -> Result<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};

    let raw = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }
        .map_err(|error| DesktopError::io(format!("读取系统文件夹失败：{error}")))?;
    let path = unsafe { raw.to_string() }
        .map(PathBuf::from)
        .map_err(|error| DesktopError::io(format!("系统文件夹路径无效：{error}")));
    unsafe {
        CoTaskMemFree(Some(raw.0.cast()));
    }
    path
}

#[cfg(windows)]
fn shell_execute(path: &Path) -> Result<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::GetProcessId;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpFile: PCWSTR(wide.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info)
            .map_err(|error| DesktopError::io(format!("启动快捷方式失败：{error}")))?;
        if info.hProcess.is_invalid() {
            return Ok(0);
        }
        let pid = GetProcessId(info.hProcess);
        let _ = CloseHandle(info.hProcess);
        Ok(pid)
    }
}

#[cfg(not(windows))]
fn shell_execute(_path: &Path) -> Result<u32> {
    Err(DesktopError::unsupported("应用启动只在 Windows 上可用"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_rejects_empty_name() {
        assert!(matches!(launch("  "), Err(DesktopError::BadArguments(_))));
    }

    #[test]
    fn launch_executable_rejects_missing_file() {
        let error = launch_executable(Path::new("definitely-not-here.exe"), &[], None).unwrap_err();
        assert!(matches!(error, DesktopError::NotFound(_)));
    }
}
