//! 文本剪贴板。只处理 `CF_UNICODETEXT`，用于长文本输入和 Clipboard 工具。

use crate::error::{DesktopError, Result};
use std::sync::Mutex;

static CLIPBOARD_LOCK: Mutex<()> = Mutex::new(());

/// 读取当前剪贴板文本；没有文本格式时返回 `None`。
#[cfg(windows)]
pub fn get_text() -> Result<Option<String>> {
    use std::ptr;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    fn open() -> Result<()> {
        use windows_sys::Win32::System::DataExchange::OpenClipboard;
        for _ in 0..10 {
            if unsafe { OpenClipboard(ptr::null_mut()) } != 0 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(DesktopError::io("剪贴板被其它程序占用，无法打开"))
    }

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    let _lock = CLIPBOARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    open()?;
    let _guard = Guard;
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) } == 0 {
        return Ok(None);
    }
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
    if handle.is_null() {
        return Ok(None);
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        return Err(DesktopError::io("无法锁定剪贴板文本内存"));
    }
    let bytes = unsafe { GlobalSize(handle) };
    let units = bytes / 2;
    let mut text = String::new();
    if units > 0 {
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const u16, units) };
        let len = slice.iter().position(|unit| *unit == 0).unwrap_or(units);
        text = String::from_utf16_lossy(&slice[..len]);
    }
    unsafe {
        GlobalUnlock(handle);
    }
    Ok(Some(text))
}

/// 写入纯文本剪贴板；系统接管内存后不再由本进程释放。
#[cfg(windows)]
pub fn set_text(text: &str) -> Result<()> {
    use std::ptr;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    fn open() -> Result<()> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(ptr::null_mut()) } != 0 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(DesktopError::io("剪贴板被其它程序占用，无法打开"))
    }

    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    let _lock = CLIPBOARD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    open()?;
    let _guard = Guard;
    unsafe {
        if EmptyClipboard() == 0 {
            return Err(DesktopError::io("清空剪贴板失败"));
        }
    }

    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes = units.len() * std::mem::size_of::<u16>();
    let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
    if handle.is_null() {
        return Err(DesktopError::io("为剪贴板分配内存失败"));
    }
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(DesktopError::io("无法锁定剪贴板内存"));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr.cast::<u16>(), units.len());
        GlobalUnlock(handle);
    }

    let result = unsafe { SetClipboardData(CF_UNICODETEXT as u32, handle) };
    if result.is_null() {
        unsafe {
            GlobalFree(handle);
        }
        return Err(DesktopError::io("写入剪贴板失败"));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn get_text() -> Result<Option<String>> {
    Err(DesktopError::unsupported("剪贴板只在 Windows 上可用"))
}

#[cfg(not(windows))]
pub fn set_text(_text: &str) -> Result<()> {
    Err(DesktopError::unsupported("剪贴板只在 Windows 上可用"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn roundtrip_text() {
        // 无交互桌面/剪贴板被占用时跳过，不让环境差异把单元测试变成偶发失败。
        let Ok(previous) = get_text() else {
            return;
        };
        if set_text("Neo clipboard 测试 🙂").is_err() {
            return;
        }
        assert_eq!(
            get_text().unwrap().as_deref(),
            Some("Neo clipboard 测试 🙂")
        );
        if let Some(previous) = previous {
            let _ = set_text(&previous);
        }
    }
}
