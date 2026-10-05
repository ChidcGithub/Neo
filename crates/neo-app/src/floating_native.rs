use super::*;
use std::ptr::null_mut;
#[cfg(not(test))]
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ptr::null,
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*};
#[cfg(not(test))]
use windows_sys::Win32::{
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn error(operation: &str) -> String {
    format!("floating {operation}: Win32 error {}", unsafe {
        GetLastError()
    })
}

// The procedure only queues input. No mutable renderer borrow crosses a Win32
// call, which may synchronously re-enter this procedure (DPI/show/position).
#[cfg(not(test))]
struct Input {
    shared: Arc<Shared>,
    events: RefCell<VecDeque<(bool, POINT, Instant)>>,
    cancel: Cell<bool>,
    layout: Cell<bool>,
    close: Cell<bool>,
}

#[cfg(not(test))]
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
    let input = (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Input).as_ref();
    match msg {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps = std::mem::zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => {
            if let Some(input) = input {
                if !input.shared.interactive() {
                    return 0;
                }
                // lParam is the message position, unlike GetCursorPos which can
                // already refer to a later move when a quick click is dequeued.
                let mut p = POINT {
                    x: l as i16 as i32,
                    y: (l >> 16) as i16 as i32,
                };
                if ClientToScreen(hwnd, &mut p) != 0 {
                    let mut events = input.events.borrow_mut();
                    if events.len() < 128 {
                        events.push_back((msg == WM_LBUTTONDOWN, p, Instant::now()));
                    } else {
                        input.cancel.set(true);
                    }
                }
            }
            0
        }
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            if let Some(input) = input {
                input.cancel.set(true);
            }
            0
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            if let Some(input) = input {
                input.layout.set(true);
            }
            0
        }
        WM_CLOSE => {
            if let Some(input) = input {
                input.close.set(true);
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

#[cfg(not(test))]
struct DpiContext(DPI_AWARENESS_CONTEXT);
#[cfg(not(test))]
impl Drop for DpiContext {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
}
#[cfg(not(test))]
struct Window<'a>(HWND, &'a Shared);
#[cfg(not(test))]
impl Drop for Window<'_> {
    fn drop(&mut self) {
        unsafe {
            ShowWindow(self.0, SW_HIDE);
            self.1
                .hidden
                .store(IsWindowVisible(self.0) == 0, Ordering::Release);
            DestroyWindow(self.0);
        }
    }
}

#[cfg(not(test))]
unsafe fn hide(hwnd: HWND, surface: &mut Surface, shared: &Shared, acknowledge: bool) {
    ShowWindow(hwnd, SW_HIDE);
    surface.shown = false;
    if acknowledge {
        shared
            .hidden
            .store(IsWindowVisible(hwnd) == 0, Ordering::Release);
    }
}
#[cfg(not(test))]
struct Class {
    name: Vec<u16>,
    instance: HINSTANCE,
}
#[cfg(not(test))]
impl Drop for Class {
    fn drop(&mut self) {
        unsafe {
            UnregisterClassW(self.name.as_ptr(), self.instance);
        }
    }
}

/// Owns a selected top-down 32-bit DIB. Never exposes a desktop-sized buffer.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    size: i32,
}
impl Dib {
    unsafe fn new(size: i32) -> Result<Self, String> {
        if !(1..=720).contains(&size) {
            return Err("floating invalid surface size".into());
        }
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return Err(error("CreateCompatibleDC"));
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            DeleteDC(dc);
            return Err(error("CreateDIBSection"));
        }
        let old = SelectObject(dc, bitmap);
        if old.is_null() || old as isize == GDI_ERROR as isize {
            DeleteObject(bitmap);
            DeleteDC(dc);
            return Err(error("SelectObject"));
        }
        let mut dib = Self {
            dc,
            bitmap,
            old,
            bits: bits.cast(),
            size,
        };
        dib.pixels().fill(0);
        Ok(dib)
    }
    unsafe fn pixels(&mut self) -> &mut [u32] {
        std::slice::from_raw_parts_mut(self.bits, (self.size * self.size) as usize)
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            GdiFlush();
            self.pixels().fill(0);
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[cfg(not(test))]
unsafe fn monitor(point: POINT) -> Result<(RECT, u32), String> {
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        return Err(error("GetMonitorInfoW"));
    }
    if info.rcWork.right <= info.rcWork.left || info.rcWork.bottom <= info.rcWork.top {
        return Err("floating monitor has no work area".into());
    }
    let (mut x, mut y) = (96, 96);
    if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) < 0 {
        x = 96;
    }
    Ok((info.rcWork, x))
}

#[cfg(not(test))]
struct Surface {
    origin: Point,
    scale: f32,
    size: i32,
    work: RECT,
    glass: Option<Vec<u32>>,
    gesture: Gesture,
    menu_animation: MenuAnimation,
    press_scale: f32,
    shown: bool,
    dirty: bool,
    recapture: bool,
}
#[cfg(not(test))]
impl Surface {
    fn hit(&self, p: POINT) -> Option<Target> {
        // Moving menu targets are not actionable until the opening finishes.
        hit(
            self.local(p),
            self.gesture.menu && !self.menu_animation.active(true),
        )
    }

    fn local(&self, p: POINT) -> Point {
        Point {
            x: (p.x as f32 - self.origin.x) / self.scale,
            y: (p.y as f32 - self.origin.y) / self.scale,
        }
    }
    fn screen(&self, p: POINT) -> Point {
        Point {
            x: p.x as f32 / self.press_scale,
            y: p.y as f32 / self.press_scale,
        }
    }
    fn clamp(&mut self) {
        self.origin = clamp_origin(
            self.origin,
            self.work.left,
            self.work.top,
            self.work.right,
            self.work.bottom,
            self.size,
        );
        self.origin.x = self.origin.x.round();
        self.origin.y = self.origin.y.round();
    }
    unsafe fn relayout(&mut self, point: POINT) -> Result<(), String> {
        let (work, dpi) = monitor(point)?;
        let old_scale = self.scale;
        self.scale = fit_scale(dpi, work.right - work.left, work.bottom - work.top);
        self.size = (EXTENT * self.scale).round().max(1.0) as i32;
        self.origin.x += 90.0 * (old_scale - self.scale);
        self.origin.y += 90.0 * (old_scale - self.scale);
        self.work = work;
        self.clamp();
        // Monitor polling during a drag is not itself a pixel change.
        self.dirty |= self.glass.is_some() || old_scale != self.scale;
        clear_glass(&mut self.glass);
        self.recapture = true;
        Ok(())
    }
}

#[cfg(not(test))]
impl Drop for Surface {
    fn drop(&mut self) {
        clear_glass(&mut self.glass);
    }
}

/// Hidden-window-only capture. Wait for the compositor to remove the entire
/// menu first; affinity is defense in depth, not a reason to sample ourselves.
/// A failed capture simply leaves a neutral gray plate. No screenshot IO.
#[cfg(not(test))]
unsafe fn capture(surface: &Surface, shared: &Shared, revision: u64) -> Option<Vec<u32>> {
    if !shared.may_capture(revision) {
        return None;
    }
    let size = (surface.size + 3) / 4;
    let mut dib = Dib::new(size).ok()?;
    if !shared.may_capture(revision) {
        return None;
    }
    let screen = GetDC(null_mut());
    if screen.is_null() {
        return None;
    }
    SetStretchBltMode(dib.dc, HALFTONE as i32);
    SetBrushOrgEx(dib.dc, 0, 0, null_mut());
    if !shared.may_capture(revision) {
        ReleaseDC(null_mut(), screen);
        return None;
    }
    let copied = StretchBlt(
        dib.dc,
        0,
        0,
        size,
        size,
        screen,
        surface.origin.x as i32,
        surface.origin.y as i32,
        surface.size,
        surface.size,
        SRCCOPY | CAPTUREBLT,
    );
    let flushed = GdiFlush();
    ReleaseDC(null_mut(), screen);
    if copied == 0 || flushed == 0 || !shared.may_capture(revision) {
        return None;
    }
    let pixels = dib.pixels();
    let mut image = image::RgbImage::from_fn(size as u32, size as u32, |x, y| {
        let p = pixels[(y * size as u32 + x) as usize];
        image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
    });
    pixels.fill(0);
    let mut blurred = image::imageops::blur(&image, 4.0);
    image.as_mut().fill(0);
    let mut expanded = image::imageops::resize(
        &blurred,
        surface.size as u32,
        surface.size as u32,
        image::imageops::FilterType::Triangle,
    );
    blurred.as_mut().fill(0);
    let result = expanded
        .pixels()
        .map(|p| {
            let tint = |v: u8| (v as u32 * 3 + 193 * 7) / 10;
            0xff000000 | tint(p[0]) << 16 | tint(p[1]) << 8 | tint(p[2])
        })
        .collect();
    expanded.as_mut().fill(0);
    let mut result = Some(result);
    if !shared.may_capture(revision) {
        clear_glass(&mut result);
    }
    result
}

// Dynamically loaded: DWM is optional and no Cargo feature/dependency is needed.
// If flushing is unavailable, skip capture after hiding rather than risk feedback.
#[cfg(not(test))]
unsafe fn flush_compositor() -> bool {
    let module = GetModuleHandleW(wide("dwmapi.dll").as_ptr());
    if module.is_null() {
        return false;
    }
    let Some(proc) = windows_sys::Win32::System::LibraryLoader::GetProcAddress(
        module,
        c"DwmFlush".as_ptr().cast(),
    ) else {
        return false;
    };
    let flush: unsafe extern "system" fn() -> i32 = std::mem::transmute(proc);
    flush() >= 0
}

unsafe fn label_font(height: i32) -> Result<HFONT, String> {
    let font = CreateFontW(
        -height,
        0,
        0,
        0,
        500,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        ANTIALIASED_QUALITY as u32,
        0,
        wide("Microsoft YaHei UI").as_ptr(),
    );
    if font.is_null() {
        return Err(error("CreateFontW"));
    }
    Ok(font)
}

#[derive(Default)]
struct Fonts(Vec<(i32, HFONT)>);

impl Fonts {
    unsafe fn get(&mut self, height: i32) -> Result<HFONT, String> {
        if let Some((_, font)) = self.0.iter().find(|(h, _)| *h == height) {
            return Ok(*font);
        }
        let font = label_font(height)?;
        self.0.push((height, font));
        Ok(font)
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for (_, font) in self.0.drain(..) {
            unsafe {
                DeleteObject(font);
            }
        }
    }
}

// Measure with the actual Win10 GDI font/fallback, not UTF-8 length or an
// assumed Latin glyph width. Keep both lines inside the circular button.
unsafe fn draw_label(
    dc: HDC,
    text: &str,
    bounds: RECT,
    height: i32,
    fonts: &mut Fonts,
) -> Result<SIZE, String> {
    let text = wide(text);
    let mut height = height.max(1);
    let mut font = fonts.get(height)?;
    let old = SelectObject(dc, font);
    if old.is_null() || old as isize == GDI_ERROR as isize {
        return Err(error("select label font"));
    }
    SetTextColor(dc, 0x00ffffff);
    SetBkMode(dc, TRANSPARENT as i32);
    let measured = loop {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        DrawTextW(
            dc,
            text.as_ptr(),
            (text.len() - 1) as i32,
            &mut rect,
            DT_CALCRECT | DT_CENTER | DT_NOPREFIX,
        );
        let size = SIZE {
            cx: rect.right,
            cy: rect.bottom,
        };
        if (size.cx <= bounds.right - bounds.left && size.cy <= bounds.bottom - bounds.top)
            || height == 1
        {
            break size;
        }
        SelectObject(dc, old);
        height -= 1;
        font = fonts.get(height)?;
        SelectObject(dc, font);
    };
    let mut rect = bounds;
    rect.top += ((bounds.bottom - bounds.top - measured.cy) / 2).max(0);
    DrawTextW(
        dc,
        text.as_ptr(),
        (text.len() - 1) as i32,
        &mut rect,
        DT_CENTER | DT_NOPREFIX,
    );
    SelectObject(dc, old);
    Ok(measured)
}

fn label_bounds(circle: Circle, scale: f32) -> RECT {
    RECT {
        left: ((circle.center.x - circle.radius * 0.85) * scale).round() as i32,
        right: ((circle.center.x + circle.radius * 0.85) * scale).round() as i32,
        top: ((circle.center.y - circle.radius * 0.475) * scale).round() as i32,
        bottom: ((circle.center.y + circle.radius * 0.475) * scale).round() as i32,
    }
}

unsafe fn labels(
    dib: &mut Dib,
    scale: f32,
    amount: f32,
    fonts: &mut Fonts,
) -> Result<Vec<u8>, String> {
    for circle in animated_circles(amount).skip(2) {
        draw_label(
            dib.dc,
            circle.target.label(),
            label_bounds(circle, scale),
            (8.5 * scale).round().max(1.0) as i32,
            fonts,
        )?;
    }
    let flushed = GdiFlush();
    if flushed == 0 {
        return Err(error("label GdiFlush"));
    }
    Ok(dib.pixels().iter().map(|p| *p as u8).collect())
}

// Only generated artwork is cached. The reusable DIB is wiped after upload so
// revoking capture cannot leave a second persistent copy of desktop pixels.
#[derive(Default)]
struct RenderCache {
    dib: Option<Dib>,
    scale: Option<f32>,
    icon: Option<image::RgbaImage>,
    blurred: Option<(f32, image::RgbaImage)>,
    fonts: Fonts,
    label_key: Option<LabelKey>,
    mask: Vec<u8>,
    #[cfg(test)]
    work: CacheWork,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CacheWork {
    dibs: usize,
    rasterizations: usize,
    blurs: usize,
    masks: usize,
}

#[derive(PartialEq, Eq)]
struct LabelKey {
    language: crate::i18n::Language,
    bounds: [(i32, i32, i32, i32); 2],
}

impl RenderCache {
    unsafe fn prepare(&mut self, size: i32, scale: f32, amount: f32) -> Result<(), String> {
        if self.dib.as_ref().is_none_or(|dib| dib.size != size) {
            self.dib = Some(Dib::new(size)?);
            #[cfg(test)]
            {
                self.work.dibs += 1;
            }
            self.label_key = None;
        }
        if self.scale != Some(scale) {
            let icon_size = (34.0 * scale).round().max(1.0) as usize;
            let (mut rgba, width, height) = crate::brand::whale_rgba(icon_size);
            #[cfg(test)]
            {
                self.work.rasterizations += 1;
            }
            for pixel in rgba.chunks_exact_mut(4) {
                pixel[..3].copy_from_slice(&[65, 68, 72]);
            }
            self.icon = Some(
                image::RgbaImage::from_raw(width, height, rgba)
                    .ok_or("floating invalid whale bitmap")?,
            );
            self.blurred = None;
            self.fonts = Fonts::default();
            self.label_key = None;
            self.scale = Some(scale);
        }
        if amount > 0.0 {
            if self
                .blurred
                .as_ref()
                .is_none_or(|(previous, _)| *previous != amount)
            {
                self.blurred = Some((
                    amount,
                    image::imageops::blur(self.icon.as_ref().unwrap(), 0.8 * scale * amount),
                ));
                #[cfg(test)]
                {
                    self.work.blurs += 1;
                }
            }
            // Use exact integer GDI bounds, not quantized animation progress.
            // An open menu always has two labels; cache hits need no heap key.
            let mut label_circles = animated_circles(amount).skip(2);
            let key = LabelKey {
                language: crate::i18n::language(),
                bounds: std::array::from_fn(|_| {
                    let r = label_bounds(label_circles.next().unwrap(), scale);
                    (r.left, r.top, r.right, r.bottom)
                }),
            };
            if self.label_key.as_ref() != Some(&key) {
                let dib = self.dib.as_mut().unwrap();
                dib.pixels().fill(0);
                self.mask = labels(dib, scale, amount, &mut self.fonts)?;
                #[cfg(test)]
                {
                    self.work.masks += 1;
                }
                self.label_key = Some(key);
            }
        }
        Ok(())
    }
}

fn blend(base: u32, rgb: [u8; 3], alpha: u32) -> u32 {
    let channel = |shift: u32, v: u8| {
        ((((base >> shift) & 255) * (255 - alpha) + v as u32 * alpha + 127) / 255) << shift
    };
    0xff000000 | channel(16, rgb[0]) | channel(8, rgb[1]) | channel(0, rgb[2])
}

unsafe fn paint(
    cache: &mut RenderCache,
    size: i32,
    scale: f32,
    amount: f32,
    glass: Option<&[u32]>,
) -> Result<(), String> {
    cache.prepare(size, scale, amount)?;
    let icon = if amount > 0.0 {
        &cache.blurred.as_ref().unwrap().1
    } else {
        cache.icon.as_ref().unwrap()
    };
    let icon_size = icon.width() as i32;
    let icon_left = (90.0 * scale).round() as i32 - icon_size / 2;
    let frame_circles: Vec<_> = animated_circles(amount).collect(); // At most four.
    let pixels = cache.dib.as_mut().unwrap().pixels();
    pixels.fill(0);
    for y in 0..size {
        for x in 0..size {
            let p = Point {
                x: (x as f32 + 0.5) / scale,
                y: (y as f32 + 0.5) / scale,
            };
            let Some((circle, coverage)) = frame_circles.iter().find_map(|circle| {
                let coverage = circle_coverage(p, *circle, scale);
                (coverage > 0.0).then_some((circle, coverage))
            }) else {
                continue;
            };
            let index = (y * size + x) as usize;
            let distance = p.distance(circle.center);
            let mut color = glass.map_or(0xffc1c1c1, |v| v[index]);
            // Soft inner rim; outside pixels stay exactly alpha zero.
            let rim = (distance - (circle.radius - 1.0)).clamp(0.0, 1.0);
            color = blend(color, [236, 238, 240], (100.0 * rim) as u32);
            if circle.target == Target::Main {
                let (ix, iy) = (x - icon_left, y - icon_left);
                if ix >= 0 && iy >= 0 && ix < icon_size as i32 && iy < icon_size as i32 {
                    let p = icon.get_pixel(ix as u32, iy as u32);
                    color = blend(color, [65, 68, 72], p[3] as u32);
                }
            } else if circle.target == Target::Close {
                let dx = (p.x - circle.center.x).abs();
                let dy = (p.y - circle.center.y).abs();
                if dx.max(dy) < 6.5 {
                    let coverage = ((1.4 - (dx - dy).abs()) * scale).clamp(0.0, 1.0);
                    color = blend(color, [197, 54, 59], (coverage * 255.0) as u32);
                }
            } else {
                color = blend(color, [96, 99, 103], cache.mask[index] as u32);
            }
            let opacity = if circle.target == Target::Main {
                1.0
            } else {
                amount
            };
            let alpha = coverage * opacity;
            let a = (alpha * 255.0).round() as u32;
            let premul = |shift: u32| (((color >> shift) & 255) * a + 127) / 255;
            pixels[index] = a << 24 | premul(16) << 16 | premul(8) << 8 | premul(0);
        }
    }
    Ok(())
}

#[cfg(not(test))]
unsafe fn render(hwnd: HWND, surface: &Surface, cache: &mut RenderCache) -> Result<(), String> {
    let amount = surface.menu_animation.amount();
    // Build the region before painting; any early error leaves no desktop copy.
    // A union region additionally excludes gaps from cross-process hit testing;
    // HTTRANSPARENT alone would only forward clicks within the current thread.
    let region = CreateRectRgn(0, 0, 0, 0);
    if region.is_null() {
        return Err(error("CreateRectRgn"));
    }
    for c in animated_circles(amount) {
        // HRGN is binary: keep it outside the antialiased fringe. Layered-window
        // alpha still makes the extra fully transparent pixels click-through.
        let part = CreateEllipticRgn(
            ((c.center.x - c.radius) * surface.scale).floor() as i32 - 2,
            ((c.center.y - c.radius) * surface.scale).floor() as i32 - 2,
            ((c.center.x + c.radius) * surface.scale).ceil() as i32 + 2,
            ((c.center.y + c.radius) * surface.scale).ceil() as i32 + 2,
        );
        if part.is_null() || CombineRgn(region, region, part, RGN_OR) == ERROR {
            if !part.is_null() {
                DeleteObject(part);
            }
            DeleteObject(region);
            return Err(error("circle region"));
        }
        DeleteObject(part);
    }
    if SetWindowRgn(hwnd, region, 0) == 0 {
        DeleteObject(region);
        return Err(error("SetWindowRgn"));
    }
    let size = SIZE {
        cx: surface.size,
        cy: surface.size,
    };
    let source = POINT { x: 0, y: 0 };
    let destination = POINT {
        x: surface.origin.x as i32,
        y: surface.origin.y as i32,
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    paint(
        cache,
        surface.size,
        surface.scale,
        amount,
        surface.glass.as_deref(),
    )?;
    let dib = cache.dib.as_mut().unwrap();
    let uploaded = UpdateLayeredWindow(
        hwnd,
        null_mut(),
        &destination,
        &size,
        dib.dc,
        &source,
        0,
        &blend,
        ULW_ALPHA,
    );
    GdiFlush();
    dib.pixels().fill(0);
    if uploaded == 0 {
        return Err(error("UpdateLayeredWindow"));
    }
    Ok(())
}

#[cfg(not(test))]
fn apply(
    effect: Effect,
    surface: &mut Surface,
    shared: &Shared,
    ctx: &egui::Context,
    send: &mpsc::SyncSender<Action>,
) {
    if !shared.interactive() {
        return;
    }
    match effect {
        Effect::None => {}
        Effect::Wake => {
            let _ = send.try_send(Action::Wake);
            ctx.request_repaint();
        }
        Effect::OpenDrawing | Effect::OpenBlackboard => {
            surface.dirty = true;
            let action = if effect == Effect::OpenDrawing {
                Action::OpenDrawing
            } else {
                Action::OpenBlackboard
            };
            let _ = send.try_send(action);
            ctx.request_repaint();
        }
        Effect::MenuChanged => surface.dirty = true,
        Effect::Drag(delta) => {
            surface.origin.x += delta.x * surface.press_scale;
            surface.origin.y += delta.y * surface.press_scale;
            surface.dirty |= surface.glass.is_some();
            clear_glass(&mut surface.glass);
            surface.recapture = true;
        }
    }
}

#[cfg(not(test))]
pub(super) fn run(
    shared: Arc<Shared>,
    ctx: egui::Context,
    send: mpsc::SyncSender<Action>,
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    struct ExitNotice<'a>(&'a Shared, &'a egui::Context);
    impl Drop for ExitNotice<'_> {
        fn drop(&mut self) {
            if !self.0.stop.load(Ordering::Acquire) {
                self.0.dead.store(true, Ordering::Release);
                self.1.request_repaint();
            }
        }
    }
    let _exit = ExitNotice(&shared, &ctx);
    let mut reported = false;
    let result = unsafe { run_inner(&shared, &ctx, &send, &ready, &mut reported) };
    if !reported {
        let _ = ready.send(result);
    } else if let Err(error) = result {
        eprintln!("{error}");
    }
    // Runtime failures close the surface rather than leaving an invisible input
    // blocker. Startup errors are returned; no main-app logging dependency.
}

#[cfg(not(test))]
unsafe fn run_inner(
    shared: &Arc<Shared>,
    ctx: &egui::Context,
    send: &mpsc::SyncSender<Action>,
    ready: &mpsc::SyncSender<Result<(), String>>,
    reported: &mut bool,
) -> Result<(), String> {
    let _dpi = DpiContext(SetThreadDpiAwarenessContext(
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    ));
    if _dpi.0.is_null() {
        return Err(error("SetThreadDpiAwarenessContext"));
    }
    let name = wide(&format!("Neo.Floating.{}", GetCurrentThreadId()));
    let instance = GetModuleHandleW(null());
    let wc = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: name.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        ..std::mem::zeroed()
    };
    if RegisterClassW(&wc) == 0 {
        return Err(error("RegisterClassW"));
    }
    let _class = Class { name, instance };
    let input = Box::new(Input {
        shared: shared.clone(),
        events: RefCell::new(VecDeque::new()),
        cancel: Cell::new(false),
        layout: Cell::new(false),
        close: Cell::new(false),
    });
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED,
        _class.name.as_ptr(),
        wide(crate::i18n::tr("Neo 悬浮按钮")).as_ptr(),
        WS_POPUP,
        0,
        0,
        1,
        1,
        null_mut(),
        null_mut(),
        instance,
        (&*input as *const Input).cast_mut().cast(),
    );
    if hwnd.is_null() {
        return Err(error("CreateWindowExW"));
    }
    let _window = Window(hwnd, shared);
    // Supported starting with Win10 2004. On older/non-composited systems we
    // still hide before capture; never fall back to an opaque rectangular HWND.
    SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    let (work, dpi) = monitor(cursor)?;
    let scale = fit_scale(dpi, work.right - work.left, work.bottom - work.top);
    let size = (EXTENT * scale).round().max(1.0) as i32;
    let mut surface = Surface {
        origin: Point {
            x: (work.right - size) as f32,
            y: (work.top + (work.bottom - work.top - size) / 2) as f32,
        },
        scale,
        size,
        work,
        glass: None,
        gesture: Gesture::default(),
        menu_animation: MenuAnimation::default(),
        press_scale: scale,
        shown: false,
        dirty: true,
        recapture: true,
    };
    let mut cache = RenderCache::default();
    let mut presented_origin = None;
    let clock = Instant::now();
    let mut animation_tick = clock;
    let mut initialized = false;
    let mut capture_revision = shared.capture_revision.load(Ordering::Acquire);
    let mut language = crate::i18n::language();
    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        if refresh_capture(shared, &mut capture_revision, &mut surface.glass) {
            surface.dirty = true;
            surface.recapture = shared.capture_allowed.load(Ordering::Acquire);
        }
        if refresh_language(&mut language) {
            SetWindowTextW(hwnd, wide(crate::i18n::tr("Neo 悬浮按钮")).as_ptr());
            // The layered HWND retains its pixels while idle. Regenerate text
            // without recapturing the desktop or waiting for menu interaction.
            surface.dirty = true;
        }
        let mut message: MSG = std::mem::zeroed();
        for _ in 0..64 {
            if PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) == 0 {
                break;
            }
            if message.message == WM_QUIT {
                return Ok(());
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if input.close.replace(false) {
            shared.visible.store(false, Ordering::Release);
        }
        if !shared.visible.load(Ordering::Acquire) {
            hide(hwnd, &mut surface, shared, true);
            surface.gesture.cancel();
            surface.menu_animation = MenuAnimation::default();
            animation_tick = Instant::now();
            clear_glass(&mut surface.glass);
            surface.recapture = true;
            surface.dirty = true;
            input.events.borrow_mut().clear();
            std::thread::sleep(Duration::from_millis(40));
            continue;
        }
        if input.cancel.replace(false) {
            surface.gesture.cancel();
            input.events.borrow_mut().clear();
            surface.dirty = true;
        }
        let events: Vec<_> = input.events.borrow_mut().drain(..).collect();
        for (down, p, time) in events {
            if !shared.interactive() {
                break;
            }
            let now = time.duration_since(clock).as_millis() as u64;
            if down {
                if let Some(target) = surface.hit(p) {
                    surface.press_scale = surface.scale;
                    surface.gesture.down(target, surface.screen(p), now);
                }
            } else {
                let effect = surface
                    .gesture
                    .update(surface.screen(p), false, surface.hit(p), now);
                apply(effect, &mut surface, shared, ctx, send);
            }
        }
        if !shared.visible.load(Ordering::Acquire) {
            continue;
        }
        if surface.gesture.active() {
            if GetCursorPos(&mut cursor) == 0 {
                surface.gesture.cancel();
                surface.dirty = true;
            } else {
                // No SetCapture: a background/no-activate HWND cannot reliably
                // capture another foreground thread's mouse. Poll only during
                // our own press, and stop on release even outside the window.
                let held = GetAsyncKeyState(VK_LBUTTON as i32) < 0;
                let effect = surface.gesture.update(
                    surface.screen(cursor),
                    held,
                    surface.hit(cursor),
                    clock.elapsed().as_millis() as u64,
                );
                apply(effect, &mut surface, shared, ctx, send);
            }
        }
        let now = Instant::now();
        surface.dirty |= surface.menu_animation.advance(
            surface.gesture.menu,
            now.duration_since(animation_tick).as_secs_f32(),
        );
        animation_tick = now;
        let dragging = matches!(surface.gesture.phase, Phase::Drag { .. });
        if input.layout.replace(false) || dragging {
            let center = POINT {
                x: (surface.origin.x + 90.0 * surface.scale) as i32,
                y: (surface.origin.y + 90.0 * surface.scale) as i32,
            };
            // While dragging, choose the cursor monitor (allows crossing gaps
            // and clamped monitor edges); otherwise keep the current monitor.
            surface.relayout(if dragging { cursor } else { center })?;
        }
        if surface.recapture && !surface.gesture.active() {
            clear_glass(&mut surface.glass);
            if shared.may_capture(capture_revision) {
                if surface.shown {
                    hide(hwnd, &mut surface, shared, false);
                }
                if shared.may_capture(capture_revision)
                    && IsWindowVisible(hwnd) == 0
                    && (!initialized || flush_compositor())
                {
                    surface.glass = capture(&surface, shared, capture_revision);
                }
            }
            surface.recapture = false;
            surface.dirty = true;
        }
        if refresh_capture(shared, &mut capture_revision, &mut surface.glass) {
            surface.dirty = true;
            surface.recapture = shared.capture_allowed.load(Ordering::Acquire);
        }
        if !shared.interactive() {
            continue;
        }
        match presentation(surface.dirty, surface.origin, presented_origin) {
            Presentation::Render => {
                render(hwnd, &surface, &mut cache)?;
                surface.dirty = false;
                presented_origin = Some(surface.origin);
            }
            Presentation::Move => {
                if SetWindowPos(
                    hwnd,
                    null_mut(),
                    surface.origin.x as i32,
                    surface.origin.y as i32,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                ) == 0
                {
                    return Err(error("move floating window"));
                }
                presented_origin = Some(surface.origin);
            }
            Presentation::Idle => {}
        }
        if !shared.interactive() {
            continue;
        }
        if !surface.shown {
            shared.hidden.store(false, Ordering::Release);
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            if !shared.interactive() {
                continue;
            }
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            surface.shown = true;
            if !shared.interactive() {
                hide(hwnd, &mut surface, shared, true);
                continue;
            }
        }
        if !initialized {
            initialized = true;
            *reported = true;
            let _ = ready.send(Ok(()));
        }
        std::thread::sleep(Duration::from_millis(
            if surface.gesture.active() || surface.menu_animation.active(surface.gesture.menu) {
                16
            } else {
                40
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "floating_native_tests.rs"]
mod tests;
