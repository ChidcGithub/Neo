//! Independently implemented Win32 selector; no external runtime source copied.
use super::{encode, selection, CaptureResult, Shared};
use neo_tools::tools::screen::{self, Rect};
use std::{cell::Cell, ptr::null_mut, time::Duration};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::DwmFlush, Gdi::*},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn error(operation: &str) -> String {
    format!("截图 {operation} 失败：Win32 {}", unsafe {
        GetLastError()
    })
}

#[derive(PartialEq)]
struct Layout {
    desktop: Rect,
    monitors: Vec<Rect>,
    scales: Vec<Option<f64>>,
}
impl Layout {
    fn read() -> Result<Self, String> {
        let monitors = screen::monitors().map_err(|e| e.to_string())?;
        let desktop = screen::virtual_screen();
        if desktop.width <= 0
            || desktop.height <= 0
            || desktop.x.checked_add(desktop.width).is_none()
            || desktop.y.checked_add(desktop.height).is_none()
        {
            return Err("虚拟桌面尺寸无效".into());
        }
        let scales = monitors
            .iter()
            .map(|m| screen::dpi_scale_at(m.x, m.y))
            .collect();
        Ok(Self {
            desktop,
            monitors,
            scales,
        })
    }
    fn check(&self, shared: &Shared) -> Result<(), String> {
        shared.check()?;
        if unsafe {
            GetAsyncKeyState(VK_ESCAPE as i32) < 0 || GetAsyncKeyState(VK_RBUTTON as i32) < 0
        } {
            shared
                .cancel
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(super::CANCELLED.into());
        }
        if *self != Self::read()? {
            return Err("显示布局已改变，截图已取消".into());
        }
        shared.check()
    }
}

struct Input<'a> {
    shared: &'a Shared,
    armed: Cell<bool>,
    cancelled: Cell<bool>,
    start: Cell<Option<(i32, i32)>>,
    end: Cell<Option<(i32, i32)>>,
    selected: Cell<bool>,
    message_point: Cell<Option<(i32, i32)>>,
    desktop: Rect,
}
impl Input<'_> {
    fn active(&self) -> bool {
        self.shared.check().is_ok() && !self.cancelled.get()
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(l as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        return 1;
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let Some(input) = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Input<'_>).as_ref() else {
        return DefWindowProcW(hwnd, msg, w, l);
    };
    match msg {
        WM_CLOSE | WM_CANCELMODE | WM_RBUTTONDOWN | WM_RBUTTONUP => {
            input.cancelled.set(true);
            0
        }
        WM_KILLFOCUS | WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            if input.armed.get() {
                input.cancelled.set(true);
            }
            0
        }
        WM_ACTIVATEAPP if w == 0 => {
            if input.armed.get() {
                input.cancelled.set(true);
            }
            0
        }
        WM_CAPTURECHANGED => {
            if input.armed.get() && !input.selected.get() {
                input.cancelled.set(true);
            }
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN if w == VK_ESCAPE as usize => {
            input.cancelled.set(true);
            0
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP => {
            if !input.armed.get() || !input.active() || input.selected.get() {
                return 0;
            }
            // MSG.pt preserves the queued event's full physical coordinates:
            // lParam wraps at 16 bits and GetCursorPos races fast queued drags.
            let Some(point) = input.message_point.get() else {
                input.cancelled.set(true);
                return 0;
            };
            if msg == WM_LBUTTONDOWN {
                input.start.set(Some(point));
                SetCapture(hwnd);
                if GetCapture() != hwnd {
                    input.cancelled.set(true);
                }
            }
            if input.start.get().is_some() {
                input.end.set(Some(point));
                InvalidateRect(hwnd, std::ptr::null(), 0);
                if msg == WM_LBUTTONUP {
                    input.selected.set(true);
                    ReleaseCapture();
                }
            }
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            if !dc.is_null() {
                let mut client = RECT::default();
                GetClientRect(hwnd, &mut client);
                // Stock brushes need neither allocation nor DeleteObject.
                FillRect(dc, &client, GetStockObject(BLACK_BRUSH) as HBRUSH);
                if let (Some(a), Some(b)) = (input.start.get(), input.end.get()) {
                    let mut rect = RECT {
                        left: a.0.min(b.0).saturating_sub(input.desktop.x),
                        top: a.1.min(b.1).saturating_sub(input.desktop.y),
                        right: a.0.max(b.0).saturating_sub(input.desktop.x),
                        bottom: a.1.max(b.1).saturating_sub(input.desktop.y),
                    };
                    for _ in 0..3 {
                        if rect.left >= rect.right || rect.top >= rect.bottom {
                            break;
                        }
                        FrameRect(dc, &rect, GetStockObject(WHITE_BRUSH) as HBRUSH);
                        rect.left += 1;
                        rect.top += 1;
                        rect.right -= 1;
                        rect.bottom -= 1;
                    }
                }
            }
            EndPaint(hwnd, &paint);
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

struct Class {
    name: Vec<u16>,
    instance: HINSTANCE,
}
impl Drop for Class {
    fn drop(&mut self) {
        unsafe {
            UnregisterClassW(self.name.as_ptr(), self.instance);
        }
    }
}
struct Window(HWND);
impl Window {
    fn destroy(&mut self) -> Result<(), String> {
        if self.0.is_null() {
            return Ok(());
        }
        unsafe {
            // Clear the borrowed callback pointer even if a native operation
            // fails. Only this owner thread can destroy the HWND.
            SetWindowLongPtrW(self.0, GWLP_USERDATA, 0);
            if GetCapture() == self.0 {
                ReleaseCapture();
            }
            ShowWindow(self.0, SW_HIDE);
            if DestroyWindow(self.0) == 0 && IsWindow(self.0) != 0 {
                return Err(error("DestroyWindow"));
            }
        }
        self.0 = null_mut();
        Ok(())
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.destroy();
    }
}

fn select(shared: &Shared, layout: &Layout) -> Result<Rect, String> {
    shared.check()?;
    let input = Box::new(Input {
        shared,
        armed: Cell::new(false),
        cancelled: Cell::new(false),
        start: Cell::new(None),
        end: Cell::new(None),
        selected: Cell::new(false),
        message_point: Cell::new(None),
        desktop: layout.desktop,
    });
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        if instance.is_null() {
            return Err(error("GetModuleHandleW"));
        }
        let name: Vec<u16> = "Neo.DrawingCapture.Overlay.v1\0".encode_utf16().collect();
        let cursor = LoadCursorW(null_mut(), IDC_CROSS);
        if cursor.is_null() {
            return Err(error("LoadCursorW"));
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: cursor,
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(error("RegisterClassW"));
        }
        let class = Class { name, instance };
        shared.check()?;
        let d = layout.desktop;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            class.name.as_ptr(),
            class.name.as_ptr(),
            WS_POPUP,
            d.x,
            d.y,
            d.width,
            d.height,
            null_mut(),
            null_mut(),
            instance,
            input.as_ref() as *const Input<'_> as *const _,
        );
        if hwnd.is_null() {
            return Err(error("CreateWindowExW"));
        }
        let mut window = Window(hwnd);
        shared.check()?;
        // Nonzero alpha retains hit testing across the whole desktop; no
        // desktop snapshot/backdrop buffer is allocated by this overlay.
        if SetLayeredWindowAttributes(hwnd, 0, 80, LWA_ALPHA) == 0 {
            return Err(error("SetLayeredWindowAttributes"));
        }
        layout.check(shared)?;
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        input.armed.set(true);
        if GetForegroundWindow() != hwnd || GetFocus() != hwnd {
            return Err("无法取得框选窗口焦点，截图已取消".into());
        }
        let selected = (|| {
            loop {
                shared.check()?;
                // Bound each message batch so a message flood cannot starve
                // connection cancellation or the deadline.
                for _ in 0..64 {
                    shared.check()?;
                    let mut message = MSG::default();
                    if PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) == 0 {
                        break;
                    }
                    if message.message == WM_QUIT {
                        input.cancelled.set(true);
                        break;
                    }
                    input.message_point.set(
                        (message.hwnd == hwnd
                            && matches!(
                                message.message,
                                WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP
                            ))
                        .then_some((message.pt.x, message.pt.y)),
                    );
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                    input.message_point.set(None);
                    if !input.active() {
                        break;
                    }
                }
                if !input.active()
                    || GetForegroundWindow() != hwnd
                    || GetAsyncKeyState(VK_ESCAPE as i32) < 0
                    || GetAsyncKeyState(VK_RBUTTON as i32) < 0
                {
                    return Err(super::CANCELLED.into());
                }
                layout.check(shared)?;
                if input.selected.get() {
                    let rect = selection(
                        input.start.get().ok_or("缺少框选起点")?,
                        input.end.get().ok_or("缺少框选终点")?,
                        d,
                    )?;
                    if screen::monitor_coverage(rect, &layout.monitors) == 0 {
                        return Err("框选区域没有实际显示器覆盖".into());
                    }
                    return Ok(rect);
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        })();
        input.armed.set(false);
        // Even errors/cancellation leave only after destruction. DwmFlush and
        // capture are deliberately outside this HWND/class/input scope.
        window.destroy()?;
        selected
    }
}

// This entire native module is excluded from test builds, independently of
// the disabled public start entry point.
pub(super) fn run(shared: &Shared) -> CaptureResult {
    shared.check()?;
    let _dpi = screen::physical_pixels().map_err(|e| e.to_string())?;
    shared.check()?;
    let layout = Layout::read()?;
    let rect = select(shared, &layout)?;
    layout.check(shared)?;
    let hr = unsafe { DwmFlush() };
    if hr < 0 {
        return Err(format!("DwmFlush 失败：{hr:#x}"));
    }
    layout.check(shared)?;
    super::rgba_bytes(rect.width, rect.height)?;
    shared.check()?;
    let shot = screen::capture_silent(rect).map_err(|e| e.to_string());
    // Do not let errors bypass the post-capture connection/deadline check.
    shared.check()?;
    layout.check(shared)?;
    let png = encode(shot?, rect, shared)?;
    layout.check(shared)?;
    shared.check()?;
    Ok(png)
}
