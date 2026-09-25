//! 屏幕捕获与鼠标输入（Windows）。
//!
//! 三个屏幕工具（`screenshot` / `click` / `drag`）共用这一层，
//! 只做两件事：**把屏幕像素读出来**、**把鼠标事件发出去**。
//!
//! ## 坐标系：虚拟桌面的**物理像素**
//!
//! 一切都以**虚拟桌面**（所有显示器拼起来的那块大画布）的**物理像素**为准：
//!
//! ```text
//!         ┌─────────────┬─────────────┐
//!         │ 显示器 1     │ 显示器 2     │   ← 虚拟桌面 = 两者的并集
//!         │  (0,0)      │  (1920,0)   │
//!         └─────────────┴─────────────┘
//!         ↑ 原点在这里；有显示器排在左边/上边时坐标就是**负数**
//! ```
//!
//! 这样"在截图里看到的像素点"和"点击的坐标"是**同一套数字**，模型不需要换算。
//!
//! ## DPI：为什么必须处理
//!
//! 进程如果是 **DPI 不感知**的，Windows 会把整个桌面按缩放比例"虚拟化"：
//! 一块 3840×2160、缩放 200% 的屏在进程眼里是 1920×1080，`BitBlt` 抓回来的是
//! **被拉伸过的糊图**，`SendInput` 也按虚拟坐标落点 —— 于是"照着截图点"必然点偏。
//!
//! 所以进这一层先 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`，
//! 拿到真实物理像素。（`eframe`/`winit` 启动时通常已经设过，重复设置只返回
//! `FALSE`，无害。）顺带用 `GetDpiForMonitor` 把缩放比报给模型，
//! 排查"为什么点偏了"时有用。

use crate::result::{ErrorKind, ToolError};

/// 屏幕上的一个矩形（虚拟桌面物理像素）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// 半开区间：右/下边界不属于本块（虚拟桌面最右一列是 `x + width - 1`）。
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    /// 越界的说明 —— **带上合法范围**，模型据此自己改，不用再问。
    pub fn outside_error(&self, what: &str, x: i32, y: i32) -> ToolError {
        ToolError::bad_args(format!(
            "{what} ({x}, {y}) 不在屏幕内：虚拟桌面 x {}…{}、y {}…{}",
            self.x,
            self.x + self.width - 1,
            self.y,
            self.y + self.height - 1
        ))
        .with_hint("先用 `screenshot` 看一眼，坐标照图里的像素位置写")
    }
}

/// 鼠标键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

impl Button {
    /// 宽容解析：模型写 `left` / `Left` / `primary` 都认。
    pub fn parse(s: &str) -> Result<Self, ToolError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "left" | "l" | "primary" => Ok(Button::Left),
            "right" | "r" | "secondary" => Ok(Button::Right),
            other => Err(ToolError::bad_args(format!(
                "button 只能是 left 或 right，收到 `{other}`"
            ))
            .with_hint("左键用 left（默认），右键用 right")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Button::Left => "left",
            Button::Right => "right",
        }
    }
}

/// 一张截下来的位图（RGBA8，行优先，左上角为原点）。
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Shot {
    /// 编码成 PNG。
    ///
    /// 给模型看的必须是**压缩后**的字节：一张 1080p 截图的原始位图约 8 MB，
    /// PNG 后 2–4 MB，而官方对单张图有 32 MiB 上限、整包 48 MiB ——
    /// 原始位图一多就顶到天花板了。
    pub fn to_png(&self) -> Result<Vec<u8>, ToolError> {
        use image::codecs::png::PngEncoder;
        use image::{ExtendedColorType, ImageEncoder};

        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                &self.rgba,
                self.width,
                self.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| ToolError::io(format!("PNG 编码失败：{e}")))?;
        Ok(out)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::c_void;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC,
        MonitorFromPoint, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT,
        DIB_RGB_COLORS, MONITOR_DEFAULTTONEAREST, SRCCOPY,
    };
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForMonitor, SetProcessDpiAwareness, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
        PROCESS_PER_MONITOR_DPI_AWARE,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, MOUSE_EVENT_FLAGS,
    };
    // `GetSystemMetrics` 在 WindowsAndMessaging，不在 Gdi。
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    /// 进这一层之前调用一次即可（重复调用无害）。
    pub fn ensure_dpi_aware() {
        unsafe {
            // PER_MONITOR_AWARE_V2 需 Win10 1703+。返回 FALSE 有两种典型场景：
            // 系统太老（API 不存在时进程根本起不来，所以这里是"有但不认"），
            // 或宿主进程已经设置过 DPI 感知（此时再设必然 ERROR_ACCESS_DENIED，
            // 而已有的感知仍然有效）。两种情况下退回 Win8.1 就有的
            // Per-Monitor V1 都是无害的兜底：成了保住每显示器感知，败了说明早就有了。
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) == 0 {
                SetProcessDpiAwareness(PROCESS_PER_MONITOR_DPI_AWARE);
            }
        }
    }

    /// 虚拟桌面（所有显示器的并集），物理像素。
    pub fn virtual_screen() -> Rect {
        unsafe {
            Rect {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        }
    }

    /// 某个点所在显示器的缩放比（1.0 = 96 DPI）。取不到返回 `None`。
    pub fn dpi_scale_at(x: i32, y: i32) -> Option<f64> {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() {
                return None;
            }
            let mut dx: u32 = 96;
            let mut dy: u32 = 96;
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
                return None;
            }
            Some(f64::from(dx) / 96.0)
        }
    }

    /// 抓一块屏幕区域。
    pub fn capture(rect: Rect) -> Result<Shot, ToolError> {
        if rect.width <= 0 || rect.height <= 0 {
            return Err(ToolError::bad_args("截屏区域必须是正的宽高")
                .with_hint("width / height 是整数像素"));
        }
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err(ToolError::new(
                    ErrorKind::Unsupported,
                    "拿不到屏幕设备上下文（GetDC 失败）—— 多半是没有可交互的桌面会话",
                )
                .with_hint("确认 Neo 跑在真实登录的桌面会话里（不是服务或无头会话）"));
            }
            let mem = CreateCompatibleDC(screen);
            if mem.is_null() {
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ToolError::io("CreateCompatibleDC 失败"));
            }

            // 负高度 = 自上而下存行，读出来就是屏幕顺序，不用再翻转。
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = rect.width;
            bi.bmiHeader.biHeight = -rect.height;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = 0; // BI_RGB

            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(
                screen,
                &bi,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if dib.is_null() || bits.is_null() {
                DeleteDC(mem);
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(ToolError::io("CreateDIBSection 失败（显存不足？）"));
            }
            let old = SelectObject(mem, dib);

            // `CAPTUREBLT`：把分层窗口也抓进来（否则某些浮层是黑的）。
            // `BitBlt` **不含光标** —— 对"给模型看屏幕"来说正好，光标只会挡住内容。
            let ok = BitBlt(
                mem,
                0,
                0,
                rect.width,
                rect.height,
                screen,
                rect.x,
                rect.y,
                SRCCOPY | CAPTUREBLT,
            );

            let mut rgba = Vec::new();
            if ok != 0 {
                let count = (rect.width as usize) * (rect.height as usize);
                rgba.reserve(count * 4);
                let src = std::slice::from_raw_parts(bits as *const u8, count * 4);
                // GDI 给的是 BGRA，且 alpha 通道不可信（常为 0），一律按不透明处理。
                for px in src.chunks_exact(4) {
                    rgba.extend_from_slice(&[px[2], px[1], px[0], 0xff]);
                }
            }

            SelectObject(mem, old);
            DeleteObject(dib);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);

            if ok == 0 {
                return Err(ToolError::io("BitBlt 失败：没能读回屏幕像素"));
            }
            Ok(Shot {
                width: rect.width as u32,
                height: rect.height as u32,
                rgba,
            })
        }
    }

    fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    const MOVE_ABS: MOUSE_EVENT_FLAGS =
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;

    /// 屏幕坐标 → `SendInput` 的绝对坐标。
    ///
    /// 绝对坐标是 **0…65535 的归一化值**，铺满整个虚拟桌面 ——
    /// 所以必须带 `MOUSEEVENTF_VIRTUALDESK`，否则只映射主显示器，
    /// 副屏上的点会落到主屏上。
    fn to_absolute(x: i32, y: i32, vs: Rect) -> (i32, i32) {
        let span_x = (vs.width - 1).max(1) as f64;
        let span_y = (vs.height - 1).max(1) as f64;
        let nx = ((x - vs.x) as f64 / span_x * 65535.0)
            .round()
            .clamp(0.0, 65535.0) as i32;
        let ny = ((y - vs.y) as f64 / span_y * 65535.0)
            .round()
            .clamp(0.0, 65535.0) as i32;
        (nx, ny)
    }

    fn down_up(button: Button) -> (MOUSE_EVENT_FLAGS, MOUSE_EVENT_FLAGS) {
        match button {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        }
    }

    fn send(events: &[INPUT]) -> Result<(), ToolError> {
        if events.is_empty() {
            return Ok(());
        }
        let sent = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != events.len() {
            // 最典型的成因：目标窗口以**管理员**身份运行而 Neo 不是 ——
            // UIPI 会静默丢掉这些事件。所以这里必须报出来，不能当成功。
            return Err(ToolError::new(
                ErrorKind::NotAllowed,
                format!("鼠标事件没有全部送达（{} / {}）", sent, events.len()),
            )
            .with_hint(
                "通常是因为目标窗口以管理员身份运行、而 Neo 不是：\
                 要么用管理员身份启动 Neo，要么换一个目标",
            ));
        }
        Ok(())
    }

    /// 移动 + 点击。
    ///
    /// `double` 时把"按下—抬起—按下—抬起"**一次发出去**：Windows 按两次点击的
    /// 时间间隔是否小于双击阈值来判定双击，拆成两次 `SendInput` 反而容易被
    /// 线程调度顶出阈值，变成一个单击加一个单击。
    pub fn click(x: i32, y: i32, button: Button, double: bool) -> Result<(), ToolError> {
        let vs = virtual_screen();
        if !vs.contains(x, y) {
            return Err(vs.outside_error("点击位置", x, y));
        }
        let (nx, ny) = to_absolute(x, y, vs);
        let (down, up) = down_up(button);

        let mut events = vec![mouse(MOVE_ABS, nx, ny), mouse(down, 0, 0), mouse(up, 0, 0)];
        if double {
            events.push(mouse(down, 0, 0));
            events.push(mouse(up, 0, 0));
        }
        send(&events)
    }

    /// 拖动：移过去 → 按下 → 分步移动到终点 → 抬起。
    ///
    /// 分步是必需的：直接"按下 → 跳到终点 → 抬起"在多数应用里会被当成单击
    /// （中间没有鼠标移动消息）。步数按 `duration_ms` 切，每步之间小睡。
    pub fn drag(
        from: (i32, i32),
        to: (i32, i32),
        button: Button,
        duration_ms: u64,
    ) -> Result<(), ToolError> {
        let vs = virtual_screen();
        if !vs.contains(from.0, from.1) {
            return Err(vs.outside_error("拖动起点", from.0, from.1));
        }
        if !vs.contains(to.0, to.1) {
            return Err(vs.outside_error("拖动终点", to.0, to.1));
        }

        let (down, up) = down_up(button);
        let (fx, fy) = to_absolute(from.0, from.1, vs);
        send(&[mouse(MOVE_ABS, fx, fy), mouse(down, 0, 0)])?;

        let steps = (duration_ms / 15).clamp(1, 60) as i32;
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            let x = from.0 + ((to.0 - from.0) as f64 * t).round() as i32;
            let y = from.1 + ((to.1 - from.1) as f64 * t).round() as i32;
            let (nx, ny) = to_absolute(x, y, vs);
            if let Err(e) = send(&[mouse(MOVE_ABS, nx, ny)]) {
                // 中途失败也要先把键松开，否则用户会留下一个"一直按着"的鼠标。
                let (tx, ty) = to_absolute(to.0, to.1, vs);
                let _ = send(&[mouse(MOVE_ABS, tx, ty), mouse(up, 0, 0)]);
                return Err(e);
            }
            if i < steps {
                std::thread::sleep(Duration::from_millis(15));
            }
        }

        let (tx, ty) = to_absolute(to.0, to.1, vs);
        send(&[mouse(MOVE_ABS, tx, ty), mouse(up, 0, 0)])
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    fn unsupported() -> ToolError {
        ToolError::new(ErrorKind::Unsupported, "屏幕交互目前只在 Windows 上实现")
            .with_hint("其他平台可以先用 `bash` 的 screencapture / import 等命令截屏")
    }

    pub fn ensure_dpi_aware() {}

    pub fn virtual_screen() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        }
    }

    pub fn dpi_scale_at(_x: i32, _y: i32) -> Option<f64> {
        None
    }

    pub fn capture(_rect: Rect) -> Result<Shot, ToolError> {
        Err(unsupported())
    }

    pub fn click(_x: i32, _y: i32, _button: Button, _double: bool) -> Result<(), ToolError> {
        Err(unsupported())
    }

    pub fn drag(
        _from: (i32, i32),
        _to: (i32, i32),
        _button: Button,
        _duration_ms: u64,
    ) -> Result<(), ToolError> {
        Err(unsupported())
    }
}

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_parsing_is_forgiving_about_case_and_aliases() {
        for s in ["", "left", "LEFT", " l ", "primary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Left, "{s}");
        }
        for s in ["right", "Right", "r", "secondary"] {
            assert_eq!(Button::parse(s).unwrap(), Button::Right, "{s}");
        }
        let err = Button::parse("middle").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.hint.is_some(), "要给可执行的出路");
    }

    /// 边界是**半开**的：副屏排在主屏左边时坐标是负数，也属于桌面。
    #[test]
    fn rect_containment_is_half_open_and_allows_negative() {
        let r = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(r.contains(-1920, 0));
        assert!(r.contains(-1, 1079));
        assert!(!r.contains(0, 0), "右边界不含");
        assert!(!r.contains(-1920, 1080));
        assert!(!r.contains(-1921, 0));
    }

    /// 越界时必须把**合法范围**说出来 —— 模型据此自己改，不用再问一轮。
    #[test]
    fn outside_error_states_the_legal_range() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let e = r.outside_error("点击位置", 5000, 5000);
        assert_eq!(e.kind, ErrorKind::BadArguments);
        assert!(
            e.message.contains("1919") && e.message.contains("1079"),
            "要含合法范围：{}",
            e.message
        );
        assert!(e.hint.is_some());
    }

    /// 把屏幕层的读数打出来（默认忽略，`--ignored --nocapture` 才跑）。
    #[test]
    #[ignore = "诊断用：打印虚拟桌面与缩放比"]
    fn dump_screen_info() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        println!(
            "虚拟桌面: x={} y={} width={} height={}",
            vs.x, vs.y, vs.width, vs.height
        );
        println!(
            "中心点缩放比: {:?}",
            dpi_scale_at(vs.x + vs.width / 2, vs.y + vs.height / 2)
        );
        assert!(vs.width > 0, "没有桌面会话");
    }

    /// 只读的一条：抓 32×32、编码 PNG，并确认 DPI 查询不炸。
    /// 无头会话里允许失败，但必须是**可读的错误**。
    #[test]
    fn screen_layer_never_panics() {
        ensure_dpi_aware();
        let vs = virtual_screen();
        if vs.width <= 0 {
            eprintln!("跳过：没有可用的虚拟桌面（无头会话）");
            return;
        }
        let probe = Rect {
            x: vs.x,
            y: vs.y,
            width: 32,
            height: 32,
        };
        match capture(probe) {
            Ok(shot) => {
                assert_eq!(shot.width, 32);
                assert_eq!(shot.rgba.len(), 32 * 32 * 4);
                let png = shot.to_png().expect("PNG 编码");
                assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "PNG 魔数");
                assert!(png.len() > 50, "不该是空图");
            }
            Err(e) => {
                assert!(!e.message.is_empty());
                assert!(e.hint.is_some(), "失败也要给出路");
            }
        }
        // 缩放比：要么给个数，要么给 None —— 但不能 panic
        if let Some(scale) = dpi_scale_at(vs.x + 1, vs.y + 1) {
            assert!(scale > 0.0 && scale < 10.0, "缩放比看起来不对：{scale}");
        }
    }

    /// 越界点击必须**在发出任何事件之前**就被拒 —— 不能让鼠标先跑过去。
    #[test]
    fn out_of_range_click_is_rejected_without_touching_the_mouse() {
        let vs = virtual_screen();
        if vs.width <= 0 {
            return;
        }
        let err = click(vs.x + vs.width + 5000, vs.y + 10, Button::Left, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("不在屏幕内"), "{}", err.message);
        // 拖动两端都要查
        let err = drag(
            (vs.x + 1, vs.y + 1),
            (vs.x - 5000, vs.y + 1),
            Button::Left,
            100,
        )
        .unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.message.contains("终点"), "{}", err.message);
    }
}
