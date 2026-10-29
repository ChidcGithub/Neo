//! 顶层窗口枚举、模糊查找、前台切换与缩放。

use crate::display::ScreenRect;
use crate::error::{DesktopError, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub handle: isize,
    pub title: String,
    pub process_id: u32,
    pub rect: ScreenRect,
    pub foreground: bool,
    pub minimized: bool,
}

#[cfg(windows)]
pub fn list() -> Result<Vec<WindowInfo>> {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible,
    };

    unsafe extern "system" fn visit(hwnd: HWND, data: isize) -> i32 {
        let out = unsafe { &mut *(data as *mut Vec<WindowInfo>) };
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                return 1;
            }
            let mut cloaked = 0u32;
            let cloak_result = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                (&mut cloaked as *mut u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
            if cloak_result >= 0 && cloaked != 0 {
                return 1;
            }
            let mut title = [0u16; 512];
            let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            if len <= 0 {
                return 1;
            }
            let mut process_id = 0u32;
            if GetWindowThreadProcessId(hwnd, &mut process_id) == 0 {
                return 1;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect) == 0 {
                return 1;
            }
            let (Some(width), Some(height)) = (
                rect.right.checked_sub(rect.left),
                rect.bottom.checked_sub(rect.top),
            ) else {
                return 1;
            };
            out.push(WindowInfo {
                handle: hwnd as isize,
                title: String::from_utf16_lossy(&title[..len as usize]),
                process_id,
                rect: ScreenRect {
                    x: rect.left,
                    y: rect.top,
                    width,
                    height,
                },
                foreground: GetForegroundWindow() == hwnd,
                minimized: IsIconic(hwnd) != 0,
            });
        }
        1
    }

    let mut out: Vec<WindowInfo> = Vec::new();
    let ok = unsafe { EnumWindows(Some(visit), (&mut out as *mut Vec<WindowInfo>) as isize) };
    if ok == 0 && out.is_empty() {
        return Err(DesktopError::io("枚举顶层窗口失败"));
    }
    out.sort_by(|a, b| {
        b.foreground
            .cmp(&a.foreground)
            .then_with(|| a.rect.y.cmp(&b.rect.y))
            .then_with(|| a.rect.x.cmp(&b.rect.x))
    });
    Ok(out)
}

#[cfg(not(windows))]
pub fn list() -> Result<Vec<WindowInfo>> {
    Err(DesktopError::unsupported("窗口枚举只在 Windows 上可用"))
}

/// 按窗口标题查找；先精确/包含，再用 Jaro-Winkler 模糊匹配。
/// 找不到时把前几个窗口标题带回去，方便模型自己改查询。
pub fn find(query: &str) -> Result<WindowInfo> {
    let query = query.trim();
    if query.is_empty() {
        return Err(DesktopError::bad_args("窗口名称不能为空"));
    }
    let windows = list()?;
    if let Ok(handle) = query.parse::<isize>() {
        if let Some(window) = windows.iter().find(|window| window.handle == handle) {
            return Ok(window.clone());
        }
    }
    let needle = query.to_lowercase();
    let mut best: Option<(f64, &WindowInfo)> = None;
    for window in &windows {
        let title = window.title.to_lowercase();
        let score = if title == needle {
            1_000.0
        } else if title.starts_with(&needle) {
            900.0
        } else if title.contains(&needle) {
            800.0
        } else {
            strsim::jaro_winkler(&needle, &title) * 100.0
        };
        if score >= 70.0 && best.is_none_or(|(current, _)| score > current) {
            best = Some((score, window));
        }
    }
    if let Some((_, window)) = best {
        return Ok(window.clone());
    }
    let mut titles: Vec<&str> = windows
        .iter()
        .map(|window| window.title.as_str())
        .take(8)
        .collect();
    titles.sort_unstable();
    let hint = if titles.is_empty() {
        "当前没有可见的顶层窗口".to_owned()
    } else {
        format!("可见窗口：{}", titles.join(" / "))
    };
    Err(DesktopError::not_found(format!(
        "找不到标题匹配 {query:?} 的窗口。{hint}"
    )))
}

pub fn by_handle(handle: isize) -> Result<WindowInfo> {
    list()?
        .into_iter()
        .find(|window| window.handle == handle)
        .ok_or_else(|| DesktopError::not_found(format!("窗口句柄 {handle} 不存在或已关闭")))
}

pub fn active() -> Result<Option<WindowInfo>> {
    Ok(list()?.into_iter().find(|window| window.foreground))
}

#[cfg(windows)]
pub fn switch(window: &WindowInfo) -> Result<()> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, IsIconic, IsWindow, SetForegroundWindow,
        SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SW_RESTORE,
    };

    let hwnd = window.handle as HWND;
    unsafe {
        if IsWindow(hwnd) == 0 {
            return Err(DesktopError::not_found(format!(
                "窗口 {:?} 已关闭",
                window.title
            )));
        }
        if IsIconic(hwnd) != 0 {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );
        let _ = SetForegroundWindow(hwnd);
        if GetForegroundWindow() == hwnd {
            return Ok(());
        }
        bring_to_front(hwnd);
        let _ = BringWindowToTop(hwnd);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if GetForegroundWindow() == hwnd {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }
    Err(DesktopError::not_allowed(format!(
        "无法把窗口 {:?} 切到前台（可能被系统前台锁或权限阻止）",
        window.title
    )))
}

#[cfg(windows)]
fn bring_to_front(hwnd: windows_sys::Win32::Foundation::HWND) {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AllowSetForegroundWindow, BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId,
        SetForegroundWindow,
    };

    unsafe {
        let foreground = GetForegroundWindow();
        if foreground == hwnd {
            return;
        }
        let mut _foreground_pid = 0u32;
        let mut _target_pid = 0u32;
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, &mut _foreground_pid)
        };
        let target_thread = GetWindowThreadProcessId(hwnd, &mut _target_pid);
        let current = GetCurrentThreadId();
        let _ = AllowSetForegroundWindow(u32::MAX);

        let mut attached = Vec::new();
        for thread in [foreground_thread, target_thread] {
            if thread != 0 && thread != current && AttachThreadInput(current, thread, 1) != 0 {
                attached.push(thread);
            }
        }
        let _ = SetForegroundWindow(hwnd);
        let _ = BringWindowToTop(hwnd);
        for thread in attached.into_iter().rev() {
            let _ = AttachThreadInput(current, thread, 0);
        }
    }
}

#[cfg(not(windows))]
pub fn switch(_window: &WindowInfo) -> Result<()> {
    Err(DesktopError::unsupported("窗口切换只在 Windows 上可用"))
}

#[cfg(windows)]
pub fn resize(window: &WindowInfo, rect: ScreenRect) -> Result<()> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, MoveWindow};

    if rect.width <= 0 || rect.height <= 0 {
        return Err(DesktopError::bad_args("窗口宽高必须为正"));
    }
    let hwnd = window.handle as HWND;
    unsafe {
        if IsWindow(hwnd) == 0 {
            return Err(DesktopError::not_found(format!(
                "窗口 {:?} 已关闭",
                window.title
            )));
        }
        if MoveWindow(hwnd, rect.x, rect.y, rect.width, rect.height, 1) == 0 {
            return Err(DesktopError::io(format!(
                "缩放窗口 {:?} 失败",
                window.title
            )));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn resize(_window: &WindowInfo, _rect: ScreenRect) -> Result<()> {
    Err(DesktopError::unsupported("窗口缩放只在 Windows 上可用"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_rejects_empty_query() {
        assert!(matches!(find("  "), Err(DesktopError::BadArguments(_))));
    }
}
