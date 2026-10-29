//! 虚拟桌面几何与 DPI。输入注入统一使用**虚拟桌面物理像素**。

use crate::error::{DesktopError, Result};

/// 虚拟桌面上的一个矩形；`width`/`height` 必须为正。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// 当前虚拟桌面（所有显示器并集）的物理像素范围。
#[cfg(windows)]
pub fn virtual_screen() -> ScreenRect {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    unsafe {
        ScreenRect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

#[cfg(not(windows))]
pub fn virtual_screen() -> ScreenRect {
    ScreenRect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    }
}

/// 让进程使用物理像素。重复设置无害；失败时保持原状态。
#[cfg(windows)]
pub fn ensure_dpi_aware() {
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
pub fn ensure_dpi_aware() {}

/// 把像素坐标映射到 `SendInput` 的 0..=65535 绝对坐标。
///
/// 只接受能精确落在目标像素内部的整数点；拒绝而不是夹取，
/// 避免越界点击被 Windows 静默挪到边缘。
pub fn normalize_axis(pixel: i32, origin: i32, span: i32) -> Result<i32> {
    let local = i64::from(pixel) - i64::from(origin);
    if !(1..=65_536).contains(&span) || local < 0 || local >= i64::from(span) {
        return Err(DesktopError::bad_args(
            "输入坐标越界或桌面跨度超过 65536，无法精确表示，已拒绝而非夹取",
        ));
    }
    let span = i64::from(span);
    let first = (local * 65_536 + span - 1) / span;
    let last = ((local + 1) * 65_536 + span - 1) / span - 1;
    Ok(((first + last) / 2) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_axis_keeps_point_inside_pixel() {
        let value = normalize_axis(10, 0, 1920).unwrap();
        assert!((0..=65_535).contains(&value));
        assert_eq!(normalize_axis(0, 0, 1).unwrap(), 32_767);
    }

    #[test]
    fn normalize_axis_rejects_unrepresentable() {
        assert!(normalize_axis(1920, 0, 1920).is_err());
        assert!(normalize_axis(0, 0, 0).is_err());
        assert!(normalize_axis(0, 0, 65_537).is_err());
        assert!(normalize_axis(-1, 0, 1920).is_err());
    }
}
