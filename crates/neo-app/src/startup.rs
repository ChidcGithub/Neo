//! 原生启动提示与覆盖进程生命周期的单实例守卫。
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, AtomicU8, Ordering}, mpsc, Mutex, OnceLock},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LOG_LIMIT: u64 = 64 * 1024;
const LOG_BACKUPS: usize = 3;

struct Runtime {
    active: AtomicBool,
    starting: AtomicBool,
    splash_running: AtomicBool,
    policy: AtomicU8,
    log: Mutex<Option<PathBuf>>,
    #[cfg(windows)]
    events: Mutex<Option<(usize, usize)>>,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub struct Startup {
    splash: Option<JoinHandle<()>>,
    #[cfg(windows)]
    _instance: native::Instance,
}

impl Drop for Startup {
    fn drop(&mut self) {
        finish_splash();
        if let Some(runtime) = RUNTIME.get() {
            runtime.active.store(false, Ordering::Release);
            #[cfg(windows)]
            if let Ok(mut events) = runtime.events.lock() {
                *events = None;
            }
        }
        if let Some(thread) = self.splash.take() {
            // 系统窗口调用可能阻塞；退出不应无限等待，也不强杀线程。
            let _ = join_bounded(thread, Duration::from_millis(500));
        }
    }
}

fn join_bounded(thread: JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !thread.is_finished() {
        if Instant::now() >= deadline { return false; }
        std::thread::sleep(Duration::from_millis(5));
    }
    thread.join().is_ok()
}

fn wait_splash_closed(runtime: &Runtime, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while runtime.splash_running.load(Ordering::Acquire) {
        if Instant::now() >= deadline { return false; }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

fn policy(classroom: Option<&str>, silent: Option<&str>) -> u8 {
    u8::from(classroom != Some("0")) | (u8::from(silent == Some("1")) << 1)
}

fn suppress_dialog(policy: u8) -> bool {
    policy == 3
}

fn directory_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (key, suffix) in [("NEO_HOME", "log"), ("APPDATA", "Neo/log")] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            // 环境变量必须是绝对路径，不能悄悄回退到当前工作目录。
            if path.is_absolute() {
                paths.push(path.join(suffix));
            }
        }
    }
    let temp = std::env::temp_dir();
    if temp.is_absolute() {
        paths.push(temp.join("Neo/log"));
    }
    paths
}

fn prepare_log(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| {
        fs::create_dir_all(path).is_ok()
            && OpenOptions::new().create(true).append(true).open(path.join("startup.log")).is_ok()
    })
}

fn append_log(dir: &Path, line: &str) -> io::Result<()> {
    // 锁独立文件而非被重命名的日志；跨进程轮转互斥，异常退出自动释放。
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("startup.lock"))?;
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline =>
                std::thread::sleep(Duration::from_millis(5)),
            Err(std::fs::TryLockError::WouldBlock) =>
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "startup log busy")),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    let path = dir.join("startup.log");
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > LOG_LIMIT {
        let oldest = dir.join(format!("startup.{LOG_BACKUPS}.log"));
        if oldest.exists() { fs::remove_file(oldest)?; }
        for index in (1..LOG_BACKUPS).rev() {
            let from = dir.join(format!("startup.{index}.log"));
            if from.exists() { fs::rename(from, dir.join(format!("startup.{}.log", index + 1)))?; }
        }
        if path.exists() { fs::rename(&path, dir.join("startup.1.log"))?; }
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

fn safe_component(component: &str) -> &'static str {
    match component {
        "startup" => "startup",
        "window" | "wgpu" | "eframe" => "window",
        "instance" => "instance",
        "settings" => "settings",
        "updates" | "update" => "updates",
        "store" | "storage" | "database" => "database",
        _ => "application",
    }
}

fn safe_category(category: &str) -> &'static str {
    match category {
        "database_open_failed" => "database_open_failed",
        "preference_read_failed" => "preference_read_failed",
        "preference_open_failed" => "preference_open_failed",
        "preference_query_failed" => "preference_query_failed",
        "preference_thread_failed" => "preference_thread_failed",
        "preference_timeout" => "preference_timeout",
        "surface_failed" => "surface_failed",
        "splash_close_timeout" => "splash_close_timeout",
        "splash_thread_failed" => "splash_thread_failed",
        "instance_failed" => "instance_failed",
        "update_check_failed" => "update_check_failed",
        "fatal_failure" => "fatal_failure",
        "panic" => "panic",
        _ => "operation_failed",
    }
}

fn log_line(component: &str, category: &str, starting: bool) -> String {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("timestamp={timestamp} version={} phase={} component={} category={}\n",
        env!("CARGO_PKG_VERSION"), if starting { "startup" } else { "runtime" },
        safe_component(component), safe_category(category))
}

fn record_category(component: &str, category: &str) {
    let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) else { return };
    // 写日志期间的 panic 不能在自己的钩子中再次等待这把锁。
    let Ok(mut directory) = runtime.log.try_lock() else { return };
    let line = log_line(component, category, runtime.starting.load(Ordering::Acquire));
    if let Some(dir) = directory.as_ref() {
        if append_log(dir, &line).is_ok() { return; }
    }
    *directory = prepare_log(directory_candidates().into_iter().rev());
    if let Some(dir) = directory.as_ref() { let _ = append_log(dir, &line); }
}

/// 仅记录固定失败阶段，不持久化可能含密钥、数据库内容或路径的原始错误。
pub fn record_error(component: &str, _message: &str) {
    record_category(component, match safe_component(component) {
        "database" => "database_open_failed",
        "settings" => "preference_read_failed",
        "window" => "surface_failed",
        "instance" => "instance_failed",
        "updates" => "update_check_failed",
        _ => "operation_failed",
    });
}

pub fn log_dir() -> Option<PathBuf> {
    let runtime = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire))?;
    runtime.log.try_lock().ok()?.clone()
}

pub fn set_silent_policy(classroom_safe: bool, silent_startup_errors: bool) {
    if let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) {
        runtime.policy.store(u8::from(classroom_safe) | (u8::from(silent_startup_errors) << 1), Ordering::Release);
    }
}

pub fn finish_splash() {
    if let Some(runtime) = RUNTIME.get() {
        runtime.starting.store(false, Ordering::Release);
    }
}

pub fn poll_duplicate() -> bool {
    #[cfg(windows)]
    if let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) {
        if let Ok(events) = runtime.events.lock() {
            if let Some((request, ack)) = *events {
                return native::poll(request, ack);
            }
        }
    }
    false
}

pub fn fatal(component: &str, _message: &str) {
    let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) else { return };
    record_category(component, "fatal_failure");
    finish_splash();
    if suppress_dialog(runtime.policy.load(Ordering::Acquire)) { return; }
    // 确认窗口线程已销毁窗口后再打开模态框。超时只留日志，避免窗口叠置或死锁。
    if !wait_splash_closed(runtime, Duration::from_millis(500)) {
        record_category("window", "splash_close_timeout");
        return;
    }
    #[cfg(windows)]
    native::error_dialog(log_dir());
}

fn early_policy(path: PathBuf, timeout: Duration) -> Result<u8, &'static str> {
    let (sender, receiver) = mpsc::sync_channel(1);
    // Store 的打开过程包含 SQLite 锁等待；主线程只等待固定预算，超时保持严格默认。
    // 工作线程不更新全局策略，迟到结果不会覆盖用户之后在界面修改的设置。
    std::thread::Builder::new().name("neo-startup-settings".into()).spawn(move || {
        let result = (|| {
            let store = neo_store::Store::open(&path).map_err(|_| "preference_open_failed")?;
            let classroom = store.setting("classroom_safe").map_err(|_| "preference_query_failed")?;
            let silent = store.setting("silent_startup_errors").map_err(|_| "preference_query_failed")?;
            Ok(policy(classroom.as_deref(), silent.as_deref()))
        })();
        let _ = sender.send(result);
    }).map_err(|_| "preference_thread_failed")?;
    receiver.recv_timeout(timeout).map_err(|_| "preference_timeout")?
}

pub fn begin() -> Result<Option<Startup>, String> {
    RUNTIME.get_or_init(|| Runtime {
        active: AtomicBool::new(true),
        starting: AtomicBool::new(true),
        splash_running: AtomicBool::new(false),
        policy: AtomicU8::new(policy(None, None)),
        log: Mutex::new(prepare_log(directory_candidates())),
        #[cfg(windows)]
        events: Mutex::new(None),
    });
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if RUNTIME.get().is_some_and(|r| r.active.load(Ordering::Acquire) && r.starting.load(Ordering::Acquire)) {
            // 不保存 panic 内容、堆栈、文件路径或用户数据；panic=abort 时也执行。
            // 只处理启动期提示，不声称恢复运行期 panic，也不绕过持久化屏障。
            record_category("startup", "panic");
            fatal("startup", "panic");
        } else {
            previous(info);
        }
    }));

    #[cfg(windows)]
    let instance = match native::Instance::acquire(&native::instance_name()?)? {
        native::Acquisition::Primary(instance) => instance,
        native::Acquisition::Secondary => {
            finish_splash();
            RUNTIME.get().unwrap().active.store(false, Ordering::Release);
            return Ok(None);
        }
    };
    #[cfg(windows)]
    {
        *RUNTIME.get().unwrap().events.lock().map_err(|_| "instance state unavailable")? =
            Some((instance.request.0, instance.ack.0));
    }
    // 启动动画独立于设备、模型及偏好初始化；先置运行标志，避免 fatal 抢在建窗前弹出。
    #[cfg(windows)]
    let splash = {
        let runtime = RUNTIME.get().unwrap();
        runtime.splash_running.store(true, Ordering::Release);
        match std::thread::Builder::new().name("neo-startup".into()).spawn(|| {
            if !native::surface(false) { record_error("window", "splash unavailable"); }
            RUNTIME.get().unwrap().splash_running.store(false, Ordering::Release);
        }) {
            Ok(thread) => Some(thread),
            Err(_) => {
                runtime.splash_running.store(false, Ordering::Release);
                record_category("window", "splash_thread_failed");
                None
            }
        }
    };
    #[cfg(not(windows))]
    let splash = None;
    let guard = Startup { splash, #[cfg(windows)] _instance: instance };

    // 仅主实例读取持久化设置；失败不递归弹窗，也不阻塞图形界面的初始化。
    let path = neo_store::default_db_path();
    if path.is_absolute() && path.exists() {
        match early_policy(path, Duration::from_millis(300)) {
            Ok(value) => RUNTIME.get().unwrap().policy.store(value, Ordering::Release),
            Err(category) => record_category("settings", category),
        }
    }
    Ok(Some(guard))
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Security::{*, Authorization::ConvertSidToStringSidW},
        System::{LibraryLoader::{GetModuleHandleW, GetProcAddress}, Threading::*},
        UI::{HiDpi::*, WindowsAndMessaging::*},
    };

    fn wide(text: &str) -> Vec<u16> { text.encode_utf16().chain(Some(0)).collect() }

    pub(super) struct Handle(pub usize);
    impl Drop for Handle {
        fn drop(&mut self) { unsafe { CloseHandle(self.0 as HANDLE); } }
    }
    fn handle(raw: HANDLE) -> Result<Handle, String> {
        if raw.is_null() { Err("native instance resource unavailable".into()) } else { Ok(Handle(raw as usize)) }
    }

    pub(super) fn instance_name() -> Result<String, String> {
        unsafe {
            let mut token = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err("user identity unavailable".into());
            }
            let token = handle(token)?;
            let mut needed = 0;
            GetTokenInformation(token.0 as HANDLE, TokenUser, null_mut(), 0, &mut needed);
            // 用 usize 数组保证 TOKEN_USER 中指针字段需要的内存对齐。
            let mut storage = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(token.0 as HANDLE, TokenUser, storage.as_mut_ptr().cast(), needed, &mut needed) == 0 {
                return Err("user identity unavailable".into());
            }
            let user = &*(storage.as_ptr().cast::<TOKEN_USER>());
            let mut sid = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 { return Err("user identity unavailable".into()); }
            let mut len = 0;
            while *sid.add(len) != 0 { len += 1; }
            let name = format!("Local\\Neo.Desktop.{}", String::from_utf16_lossy(std::slice::from_raw_parts(sid, len)));
            LocalFree(sid.cast());
            Ok(name)
        }
    }

    #[derive(Debug, PartialEq)]
    enum Ownership { Primary, Secondary, Failed }
    fn ownership(wait: u32) -> Ownership {
        match wait {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ownership::Primary,
            WAIT_TIMEOUT => Ownership::Secondary,
            _ => Ownership::Failed,
        }
    }

    pub(super) struct Instance {
        mutex: Handle,
        pub request: Handle,
        pub ack: Handle,
        // Windows 互斥量归属线程；禁止将守卫移到其他线程释放。
        _owner_thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Instance {
        fn drop(&mut self) { unsafe { ReleaseMutex(self.mutex.0 as HANDLE); } }
    }
    pub(super) enum Acquisition { Primary(Instance), Secondary }

    impl Instance {
        pub fn acquire(name: &str) -> Result<Acquisition, String> {
            unsafe {
                // 两方都能创建事件，消除创建互斥量与事件之间的竞态窗口。
                let request = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.request")).as_ptr()))?;
                let ack = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.ack")).as_ptr()))?;
                let mutex = handle(CreateMutexW(null(), 0, wide(&format!("{name}.mutex")).as_ptr()))?;
                match ownership(WaitForSingleObject(mutex.0 as HANDLE, 0)) {
                    Ownership::Primary => {
                        // 旧次实例可能仍持有事件句柄，新主实例不能继承上轮残留请求。
                        ResetEvent(request.0 as HANDLE);
                        ResetEvent(ack.0 as HANDLE);
                        Ok(Acquisition::Primary(Self { mutex, request, ack, _owner_thread: std::marker::PhantomData }))
                    },
                    Ownership::Secondary => {
                        // 丢弃上次遗留的确认。并发请求允许合并；确认只用于尽力避免重复提示，
                        // 绝不能决定实例所有权。所有次实例都退出且不读取数据库或加载模型。
                        ResetEvent(ack.0 as HANDLE);
                        if SetEvent(request.0 as HANDLE) == 0 { return Err("duplicate signal unavailable".into()); }
                        // 主界面隐藏时也每隔 500 毫秒轮询。
                        if WaitForSingleObject(ack.0 as HANDLE, 750) != WAIT_OBJECT_0 {
                            // 无需注册 WinRT，即使主实例启动卡住也不抢焦点。
                            surface(true);
                        }
                        Ok(Acquisition::Secondary)
                    }
                    Ownership::Failed => Err("single-instance lock unavailable".into()),
                }
            }
        }
    }

    pub(super) fn poll(request: usize, ack: usize) -> bool {
        unsafe {
            if WaitForSingleObject(request as HANDLE, 0) == WAIT_OBJECT_0 {
                SetEvent(ack as HANDLE);
                true
            } else { false }
        }
    }

    pub(super) fn error_dialog(directory: Option<PathBuf>) {
        let location = directory.map(|p| p.display().to_string()).unwrap_or_else(|| "日志目录不可写，未能保存日志".into());
        let text = wide(&format!("Neo 无法完成启动或创建窗口。\n请尝试重新启动，或将日志交给管理员。\n\n日志位置：{location}"));
        unsafe { MessageBoxW(null_mut(), text.as_ptr(), wide("Neo 启动失败").as_ptr(), MB_OK | MB_ICONERROR); }
    }

    struct Card {
        duplicate: bool,
        started: Instant,
        scale: f32,
        backdrop: std::cell::Cell<Backdrop>,
        layered: std::cell::Cell<bool>,
        failed: std::cell::Cell<bool>,
    }

    struct Layout { width: i32, height: i32, diameter: i32, brand: RECT, caption: RECT, progress: RECT }
    fn layout(duplicate: bool, scale: f32) -> Layout {
        let px = |n: f32| (n * scale).round() as i32;
        let rect = |l, t, r, b| RECT { left: px(l), top: px(t), right: px(r), bottom: px(b) };
        if duplicate {
            Layout { width: px(288.), height: px(154.), diameter: px(16.),
                brand: rect(0., 18., 288., 69.), caption: rect(0., 73., 288., 103.),
                progress: rect(32., 126., 256., 130.) }
        } else {
            Layout { width: px(420.), height: px(128.), diameter: px(16.),
                brand: rect(28., 25., 128., 79.), caption: rect(150., 35., 392., 69.),
                progress: rect(28., 99., 392., 102.) }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Backdrop { Blur, Opaque }
    fn backdrop_choice(allowed: bool, accent_succeeded: bool) -> Backdrop {
        if allowed && accent_succeeded { Backdrop::Blur } else { Backdrop::Opaque }
    }
    impl Backdrop {
        fn alpha(self) -> u8 { if self == Self::Blur { 190 } else { 255 } }
    }

    #[repr(C)]
    struct AccentPolicy { state: i32, flags: u32, color: u32, animation: u32 }
    #[repr(C)]
    struct CompositionData { attribute: i32, data: *mut std::ffi::c_void, size: usize }

    unsafe fn accent(hwnd: HWND, enabled: bool) -> bool {
        // user32 已由窗口系统加载；不从工作目录加载 DLL，也不静态依赖未公开的 accent 接口。
        let module = GetModuleHandleW(wide("user32.dll").as_ptr());
        if module.is_null() { return false; }
        let Some(proc) = GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast()) else { return false };
        let set: unsafe extern "system" fn(HWND, *const CompositionData) -> i32 = std::mem::transmute(proc);
        // WCA_ACCENT_POLICY=19，ACCENT_ENABLE_BLURBEHIND=3。着色由前景 DIB 负责。
        // 不使用 Win11 专属 backdrop，也不把 Win8 后无模糊效果的 DwmEnableBlurBehindWindow 当成成功。
        let mut policy = AccentPolicy { state: if enabled { 3 } else { 0 }, flags: 0, color: 0, animation: 0 };
        let data = CompositionData { attribute: 19, data: (&mut policy as *mut AccentPolicy).cast(),
            size: std::mem::size_of::<AccentPolicy>() };
        set(hwnd, &data) != 0
    }

    unsafe fn configure_backdrop(hwnd: HWND, card: &Card) {
        // HIGHCONTRASTW 的 ABI；无需为一次系统设置查询扩充依赖 feature。
        #[repr(C)]
        struct HighContrast { size: u32, flags: u32, scheme: *mut u16 }
        let mut contrast = HighContrast { size: std::mem::size_of::<HighContrast>() as u32,
            flags: 0, scheme: null_mut() };
        let readable = SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.size,
            (&mut contrast as *mut HighContrast).cast(), 0) != 0 && contrast.flags & 1 == 0;
        let allowed = card.layered.get() && readable && GetSystemMetrics(SM_REMOTESESSION) == 0;
        let mode = backdrop_choice(allowed, allowed && accent(hwnd, true));
        if mode == Backdrop::Opaque { accent(hwnd, false); }
        // 接口接受请求不保证系统策略一定显示模糊；系统可自行关闭特效，文字仍保持可读。
        card.backdrop.set(mode);
    }

    fn rgb(r: u8, g: u8, b: u8) -> u32 { r as u32 | ((g as u32) << 8) | ((b as u32) << 16) }

    fn composed_pixel(coverage: u8, color: [u8; 3], background_alpha: u8) -> u32 {
        // 灰度字形覆盖率叠到半透明浅底上；文字实心处 alpha=255，不会随底色一起变淡。
        let coverage = coverage as u32;
        let remaining = (background_alpha as u32 * (255 - coverage) + 127) / 255;
        let alpha = coverage + remaining;
        let channel = |foreground: u8, background: u32| (foreground as u32 * coverage + background * remaining + 127) / 255;
        (alpha << 24) | (channel(color[0], 248) << 16) | (channel(color[1], 250) << 8) | channel(color[2], 255)
    }

    unsafe fn text(dc: HDC, value: &str, rect: RECT, height: i32, weight: i32, color: u32) {
        let font = CreateFontW(-height, 0, 0, 0, weight, 0, 0, 0, DEFAULT_CHARSET as u32,
            0, 0, ANTIALIASED_QUALITY as u32, 0, wide("Segoe UI").as_ptr());
        let old = SelectObject(dc, font);
        SetTextColor(dc, color);
        SetBkMode(dc, TRANSPARENT as i32);
        let value = wide(value);
        let mut rect = rect;
        DrawTextW(dc, value.as_ptr(), (value.len() - 1) as i32, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(dc, old);
        DeleteObject(font);
    }

    unsafe fn paint(hwnd: HWND, card: &Card) {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let target = BeginPaint(hwnd, &mut ps);
        let geometry = layout(card.duplicate, card.scale);
        let (width, height) = (geometry.width, geometry.height);
        let dc = CreateCompatibleDC(target);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB,
            ..std::mem::zeroed() };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if target.is_null() || dc.is_null() || bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() { DeleteObject(bitmap); }
            if !dc.is_null() { DeleteDC(dc); }
            EndPaint(hwnd, &ps);
            card.failed.set(true);
            return;
        }
        let old_bitmap = SelectObject(dc, bitmap);
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize);
        pixels.fill(0);
        let px = |n: f32| (n * card.scale).round() as i32;
        // GDI 只生成白字黑底的灰度覆盖率，不依赖其未定义的 alpha 字节，也不用色键抠字。
        text(dc, "Neo", geometry.brand, px(36.), 650, rgb(255, 255, 255));
        text(dc, if card.duplicate { "Neo 已经在运行" } else { "正在准备你的课堂助手" },
            geometry.caption, px(14.), 400, rgb(255, 255, 255));
        if !card.duplicate {
            let track = CreateSolidBrush(rgb(36, 36, 36));
            let fill = CreateSolidBrush(rgb(255, 255, 255));
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let old_brush = SelectObject(dc, track);
            let r = geometry.progress;
            RoundRect(dc, r.left, r.top, r.right, r.bottom, px(3.), px(3.));
            SelectObject(dc, fill);
            let phase = card.started.elapsed().as_secs_f32() * 2.2;
            let offset = ((phase.sin() + 1.) * 0.5 * (r.right - r.left - px(68.)) as f32) as i32;
            RoundRect(dc, r.left + offset, r.top, r.left + offset + px(68.), r.bottom, px(3.), px(3.));
            SelectObject(dc, old_brush);
            SelectObject(dc, old_pen);
            DeleteObject(track);
            DeleteObject(fill);
        }
        // CPU 读取 DIB 前等待 GDI 批处理完成。浅底保留透光，文字和进度实心部分完全不透明。
        GdiFlush();
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let x = index as i32 % width;
            let y = index as i32 / width;
            let r = geometry.caption;
            let color = if x >= r.left && x < r.right && y >= r.top && y < r.bottom { [70, 80, 103] }
                else { [77, 107, 254] };
            *pixel = composed_pixel((*pixel & 255) as u8, color, card.backdrop.get().alpha());
        }
        if card.layered.get() {
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0,
                SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let size = SIZE { cx: width, cy: height };
            let origin = POINT { x: 0, y: 0 };
            if UpdateLayeredWindow(hwnd, null_mut(), null(), &size, dc, &origin, 0, &blend, ULW_ALPHA) == 0 {
                // 分层提交失败就关闭 accent 和 layered，回到普通 GDI；不循环重试特效。
                accent(hwnd, false);
                card.backdrop.set(Backdrop::Opaque);
                card.layered.set(false);
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
                for pixel in pixels.iter_mut() {
                    let missing = 255 - (*pixel >> 24);
                    let channel = |shift: u32, background: u32| ((*pixel >> shift) & 255) + (background * missing + 127) / 255;
                    *pixel = 0xff000000 | (channel(16, 248) << 16) | (channel(8, 250) << 8) | channel(0, 255);
                }
            }
        }
        if !card.layered.get() && BitBlt(target, 0, 0, width, height, dc, 0, 0, SRCCOPY) == 0 {
            card.failed.set(true);
        }
        SelectObject(dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(dc);
        EndPaint(hwnd, &ps);
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        match message {
            WM_NCCREATE => {
                let create = &*(l as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
                1
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, message, w, l)
            }
            WM_SETTINGCHANGE | WM_DWMCOMPOSITIONCHANGED | WM_THEMECHANGED => {
                let card = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Card;
                if !card.is_null() {
                    configure_backdrop(hwnd, &*card);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let card = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Card;
                if !card.is_null() { paint(hwnd, &*card); }
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }

    pub(super) fn surface(duplicate: bool) -> bool {
        if !duplicate && RUNTIME.get().is_none_or(|r| !r.starting.load(Ordering::Acquire)) {
            return true;
        }
        unsafe {
            // 仅修改当前线程的 DPI 上下文，不改变 eframe 的进程和窗口配置。
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(monitor, &mut info);
            let (mut xdpi, mut ydpi) = (96, 96);
            GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut xdpi, &mut ydpi);
            let scale = xdpi as f32 / 96.;
            let geometry = layout(duplicate, scale);
            let (width, height) = (geometry.width, geometry.height);
            let instance = GetModuleHandleW(null());
            let class = wide("Neo.Startup.Card.v1");
            let wc = WNDCLASSW { lpfnWndProc: Some(window_proc), hInstance: instance, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            let registered = RegisterClassW(&wc) != 0;
            if !registered && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                if !previous.is_null() { SetThreadDpiAwarenessContext(previous); }
                return false;
            }
            let mut card = Card { duplicate, started: Instant::now(), scale,
                backdrop: std::cell::Cell::new(Backdrop::Opaque), layered: std::cell::Cell::new(true),
                failed: std::cell::Cell::new(false) };
            let area = info.rcWork;
            let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_LAYERED,
                class.as_ptr(), wide("Neo").as_ptr(), WS_POPUP,
                area.left + (area.right - area.left - width) / 2,
                area.top + (area.bottom - area.top - height) / 2, width, height,
                null_mut(), null_mut(), instance, (&mut card as *mut Card).cast());
            if hwnd.is_null() {
                if registered { UnregisterClassW(class.as_ptr(), instance); }
                if !previous.is_null() { SetThreadDpiAwarenessContext(previous); }
                return false;
            }
            let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, geometry.diameter, geometry.diameter);
            if SetWindowRgn(hwnd, region, 0) == 0 { DeleteObject(region); }
            configure_backdrop(hwnd, &card);
            // 首帧先提交位图，再显示；避免 layered 窗口没有有效表面时短暂空白。
            InvalidateRect(hwnd, null(), 0);
            paint(hwnd, &card);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(hwnd);
            'surface: loop {
                let done = if duplicate { card.started.elapsed() >= Duration::from_millis(1300) }
                    else { RUNTIME.get().is_none_or(|r| !r.starting.load(Ordering::Acquire)) };
                if done || card.failed.get() || IsWindow(hwnd) == 0 { break; }
                let mut message: MSG = std::mem::zeroed();
                // 每轮限制消息数，避免消息洪水饿死退出检查。
                for _ in 0..64 {
                    if PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) == 0 { break; }
                    if message.message == WM_QUIT { break 'surface; }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                    if IsWindow(hwnd) == 0 { break 'surface; }
                }
                if !duplicate { InvalidateRect(hwnd, null(), 0); }
                // 最高 25 帧，限制 CPU 占用；不依赖 GPU 或主界面线程。
                std::thread::sleep(Duration::from_millis(40));
            }
            if IsWindow(hwnd) != 0 { DestroyWindow(hwnd); }
            if registered { UnregisterClassW(class.as_ptr(), instance); }
            if !previous.is_null() { SetThreadDpiAwarenessContext(previous); }
            !card.failed.get()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn elongated_layout_scales_without_overlap() {
            for scale in [1., 1.25, 1.5, 2., 3.] {
                let card = layout(false, scale);
                assert_eq!(card.width, (420. * scale).round() as i32);
                assert_eq!(card.height, (128. * scale).round() as i32);
                assert_eq!(card.diameter, (16. * scale).round() as i32);
                assert!(card.width > card.height * 3);
                assert!(card.brand.right < card.caption.left);
                assert!(card.brand.bottom < card.progress.top);
                assert!(card.caption.bottom < card.progress.top);
                for r in [card.brand, card.caption, card.progress] {
                    assert!(r.left >= 0 && r.top >= 0 && r.right <= card.width && r.bottom <= card.height);
                    assert!(r.left < r.right && r.top < r.bottom);
                }
            }
            let toast = layout(true, 1.);
            assert_eq!((toast.width, toast.height), (288, 154));
        }
        #[test]
        fn blur_failure_or_policy_denial_is_opaque() {
            assert_eq!(backdrop_choice(true, true), Backdrop::Blur);
            for (allowed, success) in [(false, false), (false, true), (true, false)] {
                let mode = backdrop_choice(allowed, success);
                assert_eq!(mode, Backdrop::Opaque);
                assert_eq!(mode.alpha(), 255);
            }
        }
        #[test]
        fn coverage_produces_premultiplied_alpha_without_fading_text() {
            for mode in [Backdrop::Blur, Backdrop::Opaque] {
                for coverage in 0..=255 {
                    let pixel = composed_pixel(coverage, [77, 107, 254], mode.alpha());
                    let alpha = pixel >> 24;
                    for shift in [0, 8, 16] { assert!((pixel >> shift) & 255 <= alpha); }
                    if mode == Backdrop::Opaque { assert_eq!(alpha, 255); }
                }
                assert_eq!(composed_pixel(255, [77, 107, 254], mode.alpha()), 0xff4d6bfe);
                assert_eq!(composed_pixel(0, [77, 107, 254], mode.alpha()) >> 24, mode.alpha() as u32);
            }
        }
        #[test]
        fn mutex_wait_state_machine() {
            assert_eq!(ownership(WAIT_OBJECT_0), Ownership::Primary);
            assert_eq!(ownership(WAIT_ABANDONED), Ownership::Primary);
            assert_eq!(ownership(WAIT_TIMEOUT), Ownership::Secondary);
            assert_eq!(ownership(WAIT_FAILED), Ownership::Failed);
        }
        #[test]
        fn isolated_kernel_mutex_and_event_lifecycle() {
            let name = format!("Local\\Neo.Startup.Test.{}.{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
            let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else { panic!("primary expected") };
            let request = primary.request.0;
            let ack = primary.ack.0;
            let name2 = name.clone();
            // 竞争者只用内核原语，测试绝不创建通知或其他真实窗口。
            std::thread::spawn(move || unsafe {
                let mutex = handle(CreateMutexW(null(), 0, wide(&format!("{name2}.mutex")).as_ptr())).unwrap();
                assert_eq!(ownership(WaitForSingleObject(mutex.0 as HANDLE, 0)), Ownership::Secondary);
                assert_ne!(SetEvent(request as HANDLE), 0);
            }).join().unwrap();
            assert!(poll(request, ack));
            assert!(!poll(request, ack));
            drop(primary);
            assert!(matches!(Instance::acquire(&name).unwrap(), Acquisition::Primary(_)));
        }
        #[test]
        fn concurrent_contenders_have_exactly_one_owner() {
            let name = format!("Local\\Neo.Startup.Race.{}.{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let threads: Vec<_> = (0..8).map(|_| {
                let name = name.clone();
                let barrier = barrier.clone();
                let winners = winners.clone();
                std::thread::spawn(move || unsafe {
                    let mutex = handle(CreateMutexW(null(), 0, wide(&name).as_ptr())).unwrap();
                    barrier.wait();
                    let result = ownership(WaitForSingleObject(mutex.0 as HANDLE, 0));
                    assert_ne!(result, Ownership::Failed);
                    if result == Ownership::Primary { winners.fetch_add(1, Ordering::SeqCst); }
                    barrier.wait();
                    if result == Ownership::Primary { ReleaseMutex(mutex.0 as HANDLE); }
                })
            }).collect();
            for thread in threads { thread.join().unwrap(); }
            assert_eq!(winners.load(Ordering::SeqCst), 1);
        }
        #[test]
        fn new_primary_acquires_while_old_contender_keeps_handles() {
            let name = format!("Local\\Neo.Startup.Retained.{}", std::process::id());
            let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else { panic!("primary expected") };
            unsafe {
                let observer = handle(CreateMutexW(null(), 0, wide(&format!("{name}.mutex")).as_ptr())).unwrap();
                let request = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.request")).as_ptr())).unwrap();
                let ack = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.ack")).as_ptr())).unwrap();
                SetEvent(request.0 as HANDLE);
                SetEvent(ack.0 as HANDLE);
                drop(primary);
                let Acquisition::Primary(next) = Instance::acquire(&name).unwrap() else { panic!("next primary expected") };
                assert!(!poll(next.request.0, next.ack.0));
                assert_eq!(WaitForSingleObject(ack.0 as HANDLE, 0), WAIT_TIMEOUT);
                drop(next);
                drop(observer);
            }
        }
        #[test]
        fn abandoned_owner_can_be_recovered() {
            let name = format!("Local\\Neo.Startup.Abandon.{}", std::process::id());
            unsafe {
                let observer = handle(CreateMutexW(null(), 0, wide(&name).as_ptr())).unwrap();
                let raw = observer.0;
                std::thread::spawn(move || { assert_eq!(WaitForSingleObject(raw as HANDLE, 0), WAIT_OBJECT_0); }).join().unwrap();
                assert_eq!(ownership(WaitForSingleObject(observer.0 as HANDLE, 0)), Ownership::Primary);
                assert_ne!(ReleaseMutex(observer.0 as HANDLE), 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalled_thread_does_not_block_shutdown() {
        let (release, wait) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = wait.recv();
            let _ = done.send(());
        });
        assert!(!join_bounded(worker, Duration::from_millis(20)));
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(join_bounded(std::thread::spawn(|| {}), Duration::from_secs(1)));
    }
    #[test]
    fn modal_requires_confirmed_splash_shutdown() {
        let runtime = Runtime {
            active: AtomicBool::new(true), starting: AtomicBool::new(false),
            splash_running: AtomicBool::new(true), policy: AtomicU8::new(1),
            log: Mutex::new(None),
            #[cfg(windows)]
            events: Mutex::new(None),
        };
        assert!(!wait_splash_closed(&runtime, Duration::ZERO));
        runtime.splash_running.store(false, Ordering::Release);
        assert!(wait_splash_closed(&runtime, Duration::ZERO));
    }
    #[test]
    fn early_preferences_are_strict_and_errors_are_categorized() {
        let dir = std::env::temp_dir().join(format!("neo-startup-policy-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let store = neo_store::Store::open(&path).unwrap();
        store.set_setting("classroom_safe", "true").unwrap();
        store.set_setting("silent_startup_errors", "true").unwrap();
        assert_eq!(early_policy(path.clone(), Duration::from_secs(2)), Ok(1));
        store.set_setting("silent_startup_errors", "1").unwrap();
        assert_eq!(early_policy(path.clone(), Duration::from_secs(2)), Ok(3));
        drop(store);
        fs::write(dir.join("invalid.db"), "invalid sqlite").unwrap();
        assert_eq!(early_policy(dir.join("invalid.db"), Duration::from_secs(2)), Err("preference_open_failed"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn strict_early_policy_and_dialog_matrix() {
        for classroom in [None, Some("1"), Some("0"), Some("true"), Some("")] {
            for silent in [None, Some("1"), Some("0"), Some("true"), Some("")] {
                assert_eq!(suppress_dialog(policy(classroom, silent)), classroom != Some("0") && silent == Some("1"));
            }
        }
    }
    #[test]
    fn uninitialized_api_is_inert() {
        assert!(RUNTIME.get().is_none());
        set_silent_policy(true, true);
        finish_splash();
        record_error("settings", "secret");
        fatal("window", "secret");
        assert!(!poll_duplicate());
        assert!(log_dir().is_none());
    }
    #[test]
    fn log_writer_subprocess() {
        let Some(dir) = std::env::var_os("NEO_STARTUP_LOG_TEST_DIR") else { return };
        let dir = PathBuf::from(dir);
        for _ in 0..800 {
            append_log(&dir, &log_line("database", "database_open_failed", true)).unwrap();
        }
    }
    #[test]
    fn concurrent_process_log_rotation() {
        let dir = std::env::temp_dir().join(format!("neo-startup-process-log-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let mut children: Vec<_> = (0..3).map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "startup::tests::log_writer_subprocess", "--test-threads=1"])
                .env("NEO_STARTUP_LOG_TEST_DIR", &dir)
                .stdout(std::process::Stdio::null()).spawn().unwrap()
        }).collect();
        for child in &mut children { assert!(child.wait().unwrap().success()); }
        for entry in fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            if entry.path().extension().is_some_and(|extension| extension == "log") {
                assert!(entry.metadata().unwrap().len() <= LOG_LIMIT);
                for line in fs::read_to_string(entry.path()).unwrap().lines() {
                    assert!(line.starts_with("timestamp="));
                    assert!(line.ends_with("component=database category=database_open_failed"));
                }
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn bounded_rotation_and_directory_fallback() {
        let dir = std::env::temp_dir().join(format!("neo-startup-log-test-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let blocked = dir.join("not-a-directory");
        fs::write(&blocked, "blocked").unwrap();
        let logs = dir.join("logs");
        assert_eq!(prepare_log([blocked.join("log"), logs.clone()]), Some(logs.clone()));
        let line = log_line("sk-sensitive\nforged=true", "sk-sensitive\nforged=true", true);
        assert!(!line.contains("sensitive"));
        assert_eq!(line.lines().count(), 1);
        for _ in 0..(LOG_LIMIT as usize * 6 / line.len()) { append_log(&logs, &line).unwrap(); }
        let files: Vec<_> = fs::read_dir(&logs).unwrap().map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "log")).collect();
        assert_eq!(files.len(), LOG_BACKUPS + 1);
        for file in files { assert!(file.metadata().unwrap().len() <= LOG_LIMIT); }
        fs::remove_dir_all(dir).unwrap();
    }
}
