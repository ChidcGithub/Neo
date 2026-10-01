//! Native hit testing, not hardware input routing. All HWNDs live on a new,
//! never-activated desktop. No input injection, desktop switching, capture,
//! collector, GPU, or app runtime is used by the default regression tests.
//! There is no Explorer taskbar here: property/focus checks do not exercise
//! Shell fullscreen detection, taskbar auto-hide, or final composed pixels.

use super::*;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, SetThreadDesktop, HDESK,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, GetPropW, IsWindowVisible, UnregisterClassW,
    WindowFromPoint, GWL_EXSTYLE, HTCLIENT, WNDPROC, WS_CHILD, WS_VISIBLE,
};

struct Desktop(HDESK);

impl Desktop {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name: Vec<u16> = format!(
            "neo-overlay-test-{}-{}\0",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
        .encode_utf16()
        .collect();
        // DESKTOP_READOBJECTS | DESKTOP_CREATEWINDOW | DESKTOP_WRITEOBJECTS.
        // Deliberately no DESKTOP_SWITCHDESKTOP permission.
        let desktop = unsafe {
            CreateDesktopW(
                name.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0x0083,
                std::ptr::null(),
            )
        };
        assert!(!desktop.is_null(), "CreateDesktopW failed: {}", unsafe {
            GetLastError()
        });
        Self(desktop)
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        // Both window threads must exit before this handle is closed.
        let ok = unsafe { CloseDesktop(self.0) };
        if !std::thread::panicking() {
            assert_ne!(ok, 0, "CloseDesktop failed: {}", unsafe { GetLastError() });
        }
    }
}

fn attach(desktop: usize) {
    // Called first on fresh threads, before any HWND or hook can exist.
    assert_ne!(unsafe { SetThreadDesktop(desktop as HDESK) }, 0,
        "SetThreadDesktop failed: {}", unsafe { GetLastError() });
}

struct Class(Vec<u16>);

impl Class {
    fn new(proc: WNDPROC) -> Self {
        let name: Vec<u16> = format!("neo-overlay-test-class-{}\0", unsafe {
            GetCurrentThreadId()
        })
        .encode_utf16()
        .collect();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            lpszClassName: name.as_ptr(),
            // Opaque system brush: the hit test must not pass just because
            // the synthetic drawing window has no visible content.
            hbrBackground: 6usize as _,
            ..Default::default()
        };
        assert_ne!(unsafe { RegisterClassExW(&wc) }, 0,
            "RegisterClassExW failed: {}", unsafe { GetLastError() });
        Self(name)
    }
}

impl Drop for Class {
    fn drop(&mut self) {
        let ok = unsafe {
            UnregisterClassW(self.0.as_ptr(), GetModuleHandleW(std::ptr::null()))
        };
        if !std::thread::panicking() {
            assert_ne!(ok, 0, "UnregisterClassW failed: {}", unsafe { GetLastError() });
        }
    }
}

struct Window(HWND);

impl Window {
    fn new(class: &[u16], ex: u32, style: u32, rect: [i32; 4], parent: HWND) -> Self {
        let [x, y, w, h] = rect;
        let hwnd = unsafe {
            CreateWindowExW(ex, class.as_ptr(), std::ptr::null(), style, x, y, w, h,
                parent, std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null())
        };
        assert!(!hwnd.is_null(), "CreateWindowExW failed: {}", unsafe { GetLastError() });
        Self(hwnd)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let ok = unsafe { DestroyWindow(self.0) };
        if !std::thread::panicking() {
            assert_ne!(ok, 0, "DestroyWindow failed: {}", unsafe { GetLastError() });
        }
    }
}

fn show_topmost(hwnd: HWND) {
    unsafe { ShowWindow(hwnd, SW_SHOWNA); }
    assert_ne!(unsafe {
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)
    }, 0, "SetWindowPos failed: {}", unsafe { GetLastError() });
    assert_ne!(unsafe { IsWindowVisible(hwnd) }, 0);
}

fn pump() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn target_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return HTCLIENT as LRESULT;
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

enum Request {
    Hit(i32, i32),
    Dialog(bool),
}

struct TargetThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    tx: Sender<Request>,
    rx: Receiver<usize>,
    base: HWND,
    button: HWND,
    tid: u32,
}

impl TargetThread {
    fn new(desktop: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (tx, requests) = channel();
        let (reply, rx) = channel();
        let (ready, startup) = channel();
        let thread = std::thread::spawn(move || {
            attach(desktop);
            let class = Class::new(Some(target_proc));
            let base = Window::new(&class.0, WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                WS_POPUP, [0, 0, 640, 480], std::ptr::null_mut());
            unsafe { ShowWindow(base.0, SW_SHOWNA); }
            let dialog = Window::new(&class.0, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                WS_POPUP, [360, 280, 220, 140], std::ptr::null_mut());
            let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
            let button = Window::new(&button_class, 0, WS_CHILD | WS_VISIBLE,
                [20, 20, 100, 40], dialog.0);
            ready.send((base.0 as usize, button.0 as usize, unsafe { GetCurrentThreadId() })).unwrap();
            while !stopped.load(Ordering::Acquire) {
                pump();
                match requests.recv_timeout(Duration::from_millis(2)) {
                    Ok(Request::Hit(x, y)) => {
                        // Crucially this is NOT the overlay's thread. Directly
                        // sending WM_NCHITTEST would not test OS window selection.
                        reply.send(unsafe { WindowFromPoint(POINT { x, y }) } as usize).unwrap();
                    }
                    Ok(Request::Dialog(show)) => {
                        if show { show_topmost(dialog.0); }
                        else { hide_for_desktop(dialog.0).unwrap(); }
                        reply.send(0).unwrap();
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        // Install cleanup before waiting: setup failure must fail the test,
        // not leak a thread or silently turn an unavailable desktop into a pass.
        let mut worker = Self { stop, thread: Some(thread), tx, rx,
            base: std::ptr::null_mut(), button: std::ptr::null_mut(), tid: 0 };
        let (base, button, tid) = startup.recv_timeout(Duration::from_secs(5)).expect("target setup failed");
        worker.base = base as HWND;
        worker.button = button as HWND;
        worker.tid = tid;
        worker
    }

    fn request(&self, request: Request) -> usize {
        self.tx.send(request).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            pump();
            match self.rx.recv_timeout(Duration::from_millis(2)) {
                Ok(value) => return value,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "native query timed out");
                }
                Err(error) => panic!("native query failed: {error}"),
            }
        }
    }

    fn assert_hit(&self, x: i32, y: i32, expected: HWND, case: &str) {
        assert_eq!(self.request(Request::Hit(x, y)), expected as usize,
            "{case}: WindowFromPoint({x}, {y}); not hardware input");
    }
}

impl Drop for TargetThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            // Keep dispatching while the foreign HWNDs are destroyed.
            while !thread.is_finished() {
                pump();
                std::thread::sleep(Duration::from_millis(2));
            }
            let result = thread.join();
            if !std::thread::panicking() { result.unwrap(); }
        }
    }
}

fn assert_passive_attributes(hwnd: HWND) {
    assert_eq!(unsafe { GetPropW(hwnd, NON_RUDE_HWND) } as usize, 1);
    assert_eq!(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & DRAW_EX_STYLE,
        DRAW_EX_STYLE);
}

#[test]
fn non_rude_property_lifecycle_on_isolated_desktop() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetActiveWindow, GetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowRect, WM_SHOWWINDOW};
    use windows_sys::Win32::Foundation::RECT;

    static UNMARKED_SHOWS: AtomicU32 = AtomicU32::new(0);
    static CLEANED: AtomicU32 = AtomicU32::new(0);
    unsafe extern "system" fn observed_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_SHOWWINDOW && wp != 0 && unsafe { GetPropW(hwnd, NON_RUDE_HWND) } as usize != 1 {
            UNMARKED_SHOWS.fetch_add(1, Ordering::Relaxed);
        }
        let marked = msg == WM_NCDESTROY && unsafe { GetPropW(hwnd, NON_RUDE_HWND) } as usize == 1;
        let result = unsafe { overlay_wnd_proc(hwnd, msg, wp, lp) };
        if marked && unsafe { GetPropW(hwnd, NON_RUDE_HWND) }.is_null() {
            CLEANED.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    let desktop = Desktop::new();
    let desktop_handle = desktop.0 as usize;
    std::thread::spawn(move || {
        attach(desktop_handle);
        UNMARKED_SHOWS.store(0, Ordering::Relaxed);
        CLEANED.store(0, Ordering::Relaxed);
        let class = Class::new(Some(observed_proc));
        // No Shell/COM, engine, capture or GPU initialization.
        let window = Window::new(&class.0, DRAW_EX_STYLE, WS_POPUP,
            [0, 0, 640, 480], std::ptr::null_mut());
        assert_eq!(unsafe { IsWindowVisible(window.0) }, 0);
        assert!(unsafe { GetPropW(window.0, NON_RUDE_HWND) }.is_null());
        assert!(unsafe { GetActiveWindow() }.is_null());
        assert!(unsafe { GetFocus() }.is_null());
        for size in [(640, 480), (600, 440), (640, 480)] {
            initialize_passive_window(window.0, size).unwrap();
            assert_eq!(unsafe { IsWindowVisible(window.0) }, 0, "initialization must not show");
            assert_passive_attributes(window.0); // already marked BEFORE show
            let mut rect = RECT::default();
            assert_ne!(unsafe { GetWindowRect(window.0, &mut rect) }, 0);
            assert_eq!((rect.left, rect.top), (0, 0));
            assert_eq!((rect.right, rect.bottom), (size.0 as i32, size.1 as i32));
            show_topmost(window.0);
            pump();
            assert_passive_attributes(window.0);
            assert!(unsafe { GetActiveWindow() }.is_null(), "must not activate on private thread");
            assert!(unsafe { GetFocus() }.is_null(), "must not focus on private thread");
            hide_for_desktop(window.0).unwrap();
            assert_passive_attributes(window.0); // survives hide, not just resize setup
        }
        drop(window);
        assert_eq!(UNMARKED_SHOWS.load(Ordering::Relaxed), 0);
        assert_eq!(CLEANED.load(Ordering::Relaxed), 1, "wndproc removes property during destruction");
        let error = initialize_passive_window(std::ptr::null_mut(), (640, 480)).unwrap_err();
        assert!(error.contains("SetPropW(NonRudeHWND)"), "fail before ULW/show: {error}");
    }).join().unwrap();
}

#[test]
fn passive_overlay_cross_thread_window_from_point_on_isolated_desktop() {
    let desktop = Desktop::new();
    let desktop_handle = desktop.0 as usize;
    // The test harness thread never changes desktop. RAII destroys HWNDs on
    // their owner threads, joins both threads, then closes the desktop handle.
    std::thread::spawn(move || {
        attach(desktop_handle);
        let target = TargetThread::new(desktop_handle);
        let tid = unsafe { GetCurrentThreadId() };
        assert_ne!(target.tid, tid);
        assert_eq!(unsafe { GetWindowThreadProcessId(target.base, std::ptr::null_mut()) }, target.tid);
        target.assert_hit(100, 100, target.base, "baseline");

        // Same foreign querying thread, same rect and fully opaque backing.
        // Without TRANSPARENT the OS must select this window: a negative control
        // for accidentally testing empty/hidden windows or same-thread fallback.
        let control_class = Class::new(Some(overlay_wnd_proc));
        let control = Window::new(&control_class.0, DRAW_EX_STYLE & !WS_EX_TRANSPARENT,
            WS_POPUP, [0, 0, 640, 480], std::ptr::null_mut());
        layered_backing(control.0, (640, 480), 0xff336699).unwrap();
        show_topmost(control.0);
        target.assert_hit(100, 100, control.0, "opaque HTTRANSPARENT-only negative control");
        initialize_passive_window(control.0, (640, 480)).unwrap();
        target.assert_hit(100, 100, target.base, "ULW zero-alpha backing without TRANSPARENT");
        layered_backing(control.0, (640, 480), 0xff336699).unwrap();
        target.assert_hit(100, 100, control.0, "ULW restores nonzero alpha without style toggles");
        drop(control);
        drop(control_class);

        let class = Class::new(Some(overlay_wnd_proc));
        let overlay = Window::new(&class.0, DRAW_EX_STYLE, WS_POPUP,
            [0, 0, 640, 480], std::ptr::null_mut());
        assert_eq!(unsafe { GetWindowThreadProcessId(overlay.0, std::ptr::null_mut()) }, tid);
        initialize_passive_window(overlay.0, (640, 480)).unwrap();
        show_topmost(overlay.0);
        assert_passive_attributes(overlay.0);
        target.assert_hit(100, 100, target.base, "transparent backing");

        // Prove WS_EX_TRANSPARENT works even at nonzero-alpha pixels (as in
        // actual cards), rather than passing solely through a zero-alpha DIB.
        layered_backing(overlay.0, (640, 480), 0xff336699).unwrap();
        for (x, y) in [(1, 1), (100, 100), (320, 240), (638, 478)] {
            target.assert_hit(x, y, target.base, "opaque passive overlay");
        }
        target.request(Request::Dialog(true));
        show_topmost(overlay.0); // overlay above the foreign native button
        target.assert_hit(400, 320, target.button, "native button under overlay");
        target.assert_hit(100, 100, target.base, "outside dialog");
        assert_passive_attributes(overlay.0);

        hide_for_desktop(overlay.0).unwrap();
        target.assert_hit(400, 320, target.button, "overlay hidden");
        initialize_passive_window(overlay.0, (600, 440)).unwrap();
        show_topmost(overlay.0);
        assert_passive_attributes(overlay.0);
        target.assert_hit(400, 320, target.button, "resized transparent backing");
        target.request(Request::Dialog(false));
        target.assert_hit(400, 320, target.base, "dialog removed");
    }).join().unwrap();
    drop(desktop);
}

/// GPU integration, still never on the input desktop. Explicitly opt in because
/// DX12/DComp may not be available on CI; failure is not silently a pass.
#[test]
#[ignore = "requires DX12/DComp on an isolated desktop"]
fn surface_retry_preserves_egui_textures_and_invalidates_readiness() {
    let desktop = Desktop::new();
    let desktop_handle = desktop.0 as usize;
    std::thread::spawn(move || {
        attach(desktop_handle);
        let draws = Arc::new(AtomicU32::new(0));
        let calls = draws.clone();
        let cards: CardSlot = Arc::new(Mutex::new(BTreeMap::from([(
            card_id::MINI,
            Card::passive([20.0, 20.0, 200.0, 100.0], move |ui| {
                calls.fetch_add(1, Ordering::Relaxed);
                ui.label("First frame font atlas");
            }),
        )])));
        // Construct Gfx directly: no engine, capture worker or app runtime.
        let mut gfx = Gfx::new(Arc::new(Mutex::new(CaptureState::default())), cards).unwrap();
        assert_eq!(unsafe { IsWindowVisible(gfx.hwnd) }, 0);
        assert_passive_attributes(gfx.hwnd);
        let image = gfx.egui_ctx.load_texture("pending-image",
            egui::ColorImage::filled([2, 2], egui::Color32::RED), Default::default());
        let level = AtomicU32::new(0);
        for failure in [
            wgpu::CurrentSurfaceTexture::Timeout,
            wgpu::CurrentSurfaceTexture::Occluded,
            wgpu::CurrentSurfaceTexture::Outdated,
            wgpu::CurrentSurfaceTexture::Lost,
        ] {
            assert_eq!(gfx.render_with_acquire(&level, |_| failure).unwrap(), RenderOutcome::Skipped);
            assert_eq!(draws.load(Ordering::Relaxed), 0, "failed acquire must not run egui");
            assert!(!gfx.presentation.ready);
            assert_eq!(unsafe { IsWindowVisible(gfx.hwnd) }, 0);
        }
        assert!(gfx.render_with_acquire(&level, |_| wgpu::CurrentSurfaceTexture::Validation).is_err());
        assert_eq!(draws.load(Ordering::Relaxed), 0);
        // Real acquire + egui upload + submit/present on the private desktop.
        assert_eq!(gfx.render(&level).unwrap(), RenderOutcome::Presented);
        assert!(gfx.presentation.can_show(RenderOutcome::Presented));
        assert!(gfx.egui_renderer.texture(&image.id()).is_some(), "image delta survived retries");
        assert!(gfx.egui_renderer.texture(&egui::TextureId::Managed(0)).is_some(), "font atlas uploaded");
        show_topmost(gfx.hwnd);
        gfx.presentation.shown = true;
        gfx.refresh_display(true).unwrap();
        assert_passive_attributes(gfx.hwnd);
        assert!(!gfx.presentation.ready && !gfx.presentation.shown);
        assert_eq!(unsafe { IsWindowVisible(gfx.hwnd) }, 0);
        assert_eq!(gfx.render(&level).unwrap(), RenderOutcome::Presented);
        assert!(gfx.presentation.can_show(RenderOutcome::Presented));
        gfx.cards.lock().unwrap().clear();
        gfx.invalidate_presentation();
        assert_eq!(gfx.render_with_acquire(&level, |_| panic!("empty render must skip acquire")).unwrap(), RenderOutcome::Skipped);
        assert!(!gfx.presentation.ready);
    }).join().unwrap();
}
