//! 原生启动提示与覆盖进程生命周期的单实例守卫。
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc, Mutex, OnceLock,
    },
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
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    thread.join().is_ok()
}

fn wait_splash_closed(runtime: &Runtime, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while runtime.splash_running.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return false;
        }
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
            && OpenOptions::new()
                .create(true)
                .append(true)
                .open(path.join("startup.log"))
                .is_ok()
    })
}

fn append_log(dir: &Path, line: &str) -> io::Result<()> {
    // 锁独立文件而非被重命名的日志；跨进程轮转互斥，异常退出自动释放。
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("startup.lock"))?;
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "startup log busy",
                ))
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    let path = dir.join("startup.log");
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > LOG_LIMIT {
        let oldest = dir.join(format!("startup.{LOG_BACKUPS}.log"));
        if oldest.exists() {
            fs::remove_file(oldest)?;
        }
        for index in (1..LOG_BACKUPS).rev() {
            let from = dir.join(format!("startup.{index}.log"));
            if from.exists() {
                fs::rename(from, dir.join(format!("startup.{}.log", index + 1)))?;
            }
        }
        if path.exists() {
            fs::rename(&path, dir.join("startup.1.log"))?;
        }
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
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!(
        "timestamp={timestamp} version={} phase={} component={} category={}\n",
        env!("CARGO_PKG_VERSION"),
        if starting { "startup" } else { "runtime" },
        safe_component(component),
        safe_category(category)
    )
}

fn record_category(component: &str, category: &str) {
    let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) else {
        return;
    };
    // 写日志期间的 panic 不能在自己的钩子中再次等待这把锁。
    let Ok(mut directory) = runtime.log.try_lock() else {
        return;
    };
    let line = log_line(
        component,
        category,
        runtime.starting.load(Ordering::Acquire),
    );
    if let Some(dir) = directory.as_ref() {
        if append_log(dir, &line).is_ok() {
            return;
        }
    }
    *directory = prepare_log(directory_candidates().into_iter().rev());
    if let Some(dir) = directory.as_ref() {
        let _ = append_log(dir, &line);
    }
}

/// 仅记录固定失败阶段，不持久化可能含密钥、数据库内容或路径的原始错误。
pub fn record_error(component: &str, _message: &str) {
    record_category(
        component,
        match safe_component(component) {
            "database" => "database_open_failed",
            "settings" => "preference_read_failed",
            "window" => "surface_failed",
            "instance" => "instance_failed",
            "updates" => "update_check_failed",
            _ => "operation_failed",
        },
    );
}

pub fn log_dir() -> Option<PathBuf> {
    let runtime = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire))?;
    runtime.log.try_lock().ok()?.clone()
}

pub fn set_silent_policy(classroom_safe: bool, silent_startup_errors: bool) {
    if let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) {
        runtime.policy.store(
            u8::from(classroom_safe) | (u8::from(silent_startup_errors) << 1),
            Ordering::Release,
        );
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
    let Some(runtime) = RUNTIME.get().filter(|r| r.active.load(Ordering::Acquire)) else {
        return;
    };
    record_category(component, "fatal_failure");
    finish_splash();
    if suppress_dialog(runtime.policy.load(Ordering::Acquire)) {
        return;
    }
    // 确认窗口线程已销毁窗口后再打开模态框。超时只留日志，避免窗口叠置或死锁。
    if !wait_splash_closed(runtime, Duration::from_millis(500)) {
        record_category("window", "splash_close_timeout");
        return;
    }
    #[cfg(windows)]
    native::error_dialog(log_dir());
}

#[cfg(test)]
fn early_policy(path: PathBuf, timeout: Duration) -> Result<u8, &'static str> {
    early_settings(path, timeout).map(|(policy, _)| policy)
}

pub(crate) fn early_settings(
    path: PathBuf,
    timeout: Duration,
) -> Result<(u8, crate::i18n::Language), &'static str> {
    let (sender, receiver) = mpsc::sync_channel(1);
    // 只读连接仍可能遇到 SQLite 锁等待；主线程只等待固定预算，超时保持严格默认。
    // 工作线程不更新全局策略，迟到结果不会覆盖用户之后在界面修改的设置。
    std::thread::Builder::new()
        .name("neo-startup-settings".into())
        .spawn(move || {
            let result = (|| {
                if !path.is_absolute() || !path.is_file() {
                    return Err("preference_open_failed");
                }
                let store = neo_store::ReadOnlySettings::open(&path)
                    .map_err(|_| "preference_open_failed")?;
                let classroom = store
                    .setting("classroom_safe")
                    .map_err(|_| "preference_query_failed")?;
                let silent = store
                    .setting("silent_startup_errors")
                    .map_err(|_| "preference_query_failed")?;
                let language = store
                    .setting("language")
                    .map_err(|_| "preference_query_failed")?;
                Ok((
                    policy(classroom.as_deref(), silent.as_deref()),
                    crate::i18n::Language::from_code(language.as_deref().unwrap_or_default()),
                ))
            })();
            let _ = sender.send(result);
        })
        .map_err(|_| "preference_thread_failed")?;
    receiver
        .recv_timeout(timeout)
        .map_err(|_| "preference_timeout")?
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
        if RUNTIME
            .get()
            .is_some_and(|r| r.active.load(Ordering::Acquire) && r.starting.load(Ordering::Acquire))
        {
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
            RUNTIME
                .get()
                .unwrap()
                .active
                .store(false, Ordering::Release);
            return Ok(None);
        }
    };
    #[cfg(windows)]
    {
        *RUNTIME
            .get()
            .unwrap()
            .events
            .lock()
            .map_err(|_| "instance state unavailable")? =
            Some((instance.request.0, instance.ack.0));
    }
    // 在 splash 线程读取文案前恢复语言；超时结果不会异步覆盖运行中的设置。
    let path = neo_store::default_db_path();
    if path.is_absolute() && path.is_file() {
        match early_settings(path, Duration::from_millis(300)) {
            Ok((value, language)) => {
                RUNTIME
                    .get()
                    .unwrap()
                    .policy
                    .store(value, Ordering::Release);
                crate::i18n::set_language(language);
            }
            Err(category) => record_category("settings", category),
        }
    }
    // 先置运行标志，避免 fatal 抢在建窗前弹出。
    #[cfg(windows)]
    let splash = {
        let runtime = RUNTIME.get().unwrap();
        runtime.splash_running.store(true, Ordering::Release);
        match std::thread::Builder::new()
            .name("neo-startup".into())
            .spawn(|| {
                if !native::surface(false) {
                    record_error("window", "splash unavailable");
                }
                RUNTIME
                    .get()
                    .unwrap()
                    .splash_running
                    .store(false, Ordering::Release);
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
    let guard = Startup {
        splash,
        #[cfg(windows)]
        _instance: instance,
    };

    Ok(Some(guard))
}

#[cfg(windows)]
mod native {
    use super::*;
    use crate::i18n::{tf, tr};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Security::{Authorization::ConvertSidToStringSidW, *},
        System::{LibraryLoader::GetModuleHandleW, Threading::*},
        UI::{HiDpi::*, WindowsAndMessaging::*},
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub(super) struct Handle(pub usize);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }
    fn handle(raw: HANDLE) -> Result<Handle, String> {
        if raw.is_null() {
            Err("native instance resource unavailable".into())
        } else {
            Ok(Handle(raw as usize))
        }
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
            let mut storage =
                vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
            if GetTokenInformation(
                token.0 as HANDLE,
                TokenUser,
                storage.as_mut_ptr().cast(),
                needed,
                &mut needed,
            ) == 0
            {
                return Err("user identity unavailable".into());
            }
            let user = &*(storage.as_ptr().cast::<TOKEN_USER>());
            let mut sid = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err("user identity unavailable".into());
            }
            let mut len = 0;
            while *sid.add(len) != 0 {
                len += 1;
            }
            let name = format!(
                "Local\\Neo.Desktop.{}",
                String::from_utf16_lossy(std::slice::from_raw_parts(sid, len))
            );
            LocalFree(sid.cast());
            Ok(name)
        }
    }

    #[derive(Debug, PartialEq)]
    enum Ownership {
        Primary,
        Secondary,
        Failed,
    }
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
        fn drop(&mut self) {
            unsafe {
                ReleaseMutex(self.mutex.0 as HANDLE);
            }
        }
    }
    pub(super) enum Acquisition {
        Primary(Instance),
        Secondary,
    }

    impl Instance {
        pub fn acquire(name: &str) -> Result<Acquisition, String> {
            unsafe {
                // 两方都能创建事件，消除创建互斥量与事件之间的竞态窗口。
                let request = handle(CreateEventW(
                    null(),
                    0,
                    0,
                    wide(&format!("{name}.request")).as_ptr(),
                ))?;
                let ack = handle(CreateEventW(
                    null(),
                    0,
                    0,
                    wide(&format!("{name}.ack")).as_ptr(),
                ))?;
                let mutex = handle(CreateMutexW(
                    null(),
                    0,
                    wide(&format!("{name}.mutex")).as_ptr(),
                ))?;
                match ownership(WaitForSingleObject(mutex.0 as HANDLE, 0)) {
                    Ownership::Primary => {
                        // 旧次实例可能仍持有事件句柄，新主实例不能继承上轮残留请求。
                        ResetEvent(request.0 as HANDLE);
                        ResetEvent(ack.0 as HANDLE);
                        Ok(Acquisition::Primary(Self {
                            mutex,
                            request,
                            ack,
                            _owner_thread: std::marker::PhantomData,
                        }))
                    }
                    Ownership::Secondary => {
                        // 丢弃上次遗留的确认。并发请求允许合并；确认只用于尽力避免重复提示，
                        // 绝不能决定实例所有权。所有次实例都退出且不读取数据库或加载模型。
                        ResetEvent(ack.0 as HANDLE);
                        if SetEvent(request.0 as HANDLE) == 0 {
                            return Err("duplicate signal unavailable".into());
                        }
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
            } else {
                false
            }
        }
    }

    pub(super) fn error_dialog(directory: Option<PathBuf>) {
        let location = directory
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| tr("日志目录不可写，未能保存日志").into());
        let text = wide(&tf("Neo 无法完成启动或创建窗口。\n请尝试重新启动，或将日志交给管理员。\n\n日志位置：{location}", &[("location", location)]));
        unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                wide(tr("Neo 启动失败")).as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    mod glass {
        include!("startup_glass.rs");
    }

    struct Card {
        duplicate: bool,
        started: Instant,
        scale: f32,
        backdrop: std::cell::Cell<Backdrop>,
        glass: std::cell::RefCell<Option<Vec<u32>>>,
        layered: std::cell::Cell<bool>,
        failed: std::cell::Cell<bool>,
    }

    struct Layout {
        width: i32,
        height: i32,
        brand: RECT,
        caption: RECT,
        progress: RECT,
    }
    /// Size against the usable screen, not resolution multiplied by DPI again.
    /// DPI only sets comfortable bounds; the final fit wins on small displays.
    fn screen_scale(area: &RECT, dpi: u32) -> Option<f32> {
        let width = area.right.checked_sub(area.left)?;
        let height = area.bottom.checked_sub(area.top)?;
        if width <= 0 || height <= 0 {
            return None;
        }
        let dpi_scale = (if dpi == 0 { 96 } else { dpi } as f32 / 96.).clamp(0.75, 4.);
        let desired = (width as f32 * 0.22).clamp(280. * dpi_scale, 640. * dpi_scale);
        let fitted = desired
            .min(1440.)
            .min(width as f32 * 0.85)
            .min(height as f32 * 0.4 * (360. / 96.));
        // Skip an unusably tiny splash rather than clipping text or allocating a
        // surface from invalid monitor geometry. Main-window startup continues.
        (fitted >= 120.).then_some(fitted / 360.)
    }

    fn layout(scale: f32) -> Layout {
        let px = |n: f32| (n * scale).round() as i32;
        let rect = |l, t, r, b| RECT {
            left: px(l),
            top: px(t),
            right: px(r),
            bottom: px(b),
        };
        let height = px(96.);
        let width = px(360.);
        Layout {
            width,
            height,
            brand: rect(24., 10., 336., 52.),
            caption: rect(24., 56., 336., 80.),
            // 从底边反推上边，分数 DPI 下也保持贴边且厚度一致。
            progress: RECT {
                left: 0,
                top: height - px(3.),
                right: width,
                bottom: height,
            },
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Backdrop {
        Glass,
        Opaque,
        HighContrast,
    }
    fn backdrop_choice(
        settings_read: bool,
        high_contrast: bool,
        glass_available: bool,
    ) -> Backdrop {
        if !settings_read || high_contrast {
            Backdrop::HighContrast
        } else if glass_available {
            Backdrop::Glass
        } else {
            Backdrop::Opaque
        }
    }

    unsafe fn configure_backdrop(card: &Card) {
        #[repr(C)]
        struct HighContrast {
            size: u32,
            flags: u32,
            scheme: *mut u16,
        }
        let mut contrast = HighContrast {
            size: std::mem::size_of::<HighContrast>() as u32,
            flags: 0,
            scheme: null_mut(),
        };
        let readable = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            contrast.size,
            (&mut contrast as *mut HighContrast).cast(),
            0,
        ) != 0;
        let high_contrast = contrast.flags & 1 != 0;
        if !readable || high_contrast {
            card.glass.borrow_mut().take();
        }
        card.backdrop.set(backdrop_choice(
            readable,
            high_contrast,
            card.glass.borrow().is_some(),
        ));
    }

    #[derive(Clone, Copy)]
    struct Palette {
        body: [u8; 3],
        brand: [u8; 3],
        caption: [u8; 3],
        track: [u8; 3],
    }
    impl Palette {
        fn light() -> Self {
            Self {
                body: [232, 234, 237],
                brand: [77, 107, 254],
                caption: [70, 80, 103],
                track: [174, 188, 229],
            }
        }
        unsafe fn for_backdrop(mode: Backdrop) -> Self {
            match mode {
                Backdrop::Glass | Backdrop::Opaque => return Self::light(),
                Backdrop::HighContrast => {}
            }
            let color = |index| {
                let value = GetSysColor(index);
                [value as u8, (value >> 8) as u8, (value >> 16) as u8]
            };
            let body = color(COLOR_WINDOW);
            let ink = color(COLOR_WINDOWTEXT);
            let track = std::array::from_fn(|i| ((body[i] as u16 + ink[i] as u16) / 2) as u8);
            Self {
                body,
                brand: ink,
                caption: ink,
                track,
            }
        }
    }

    fn rgb(r: u8, g: u8, b: u8) -> u32 {
        r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
    }

    fn premultiplied(color: [u8; 3], coverage: f32) -> u32 {
        let alpha = (coverage.clamp(0., 1.) * 255.).round() as u32;
        let channel = |c: u8| (c as u32 * alpha + 127) / 255;
        (alpha << 24) | (channel(color[0]) << 16) | (channel(color[1]) << 8) | channel(color[2])
    }

    fn animation_phase(elapsed: f32, duplicate: bool) -> f32 {
        if duplicate {
            0.
        } else {
            (elapsed * 2.2).rem_euclid(std::f32::consts::TAU)
        }
    }

    fn progress_segment(geometry: &Layout, phase: f32) -> RECT {
        let track = geometry.progress;
        let width = ((track.right - track.left) / 4).max(1);
        // 定长色块往返表示等待，不把初始化时间伪装成完成百分比。
        let offset =
            ((1. - phase.cos()) * 0.5 * (track.right - track.left - width) as f32).round() as i32;
        RECT {
            left: track.left + offset,
            right: track.left + offset + width,
            ..track
        }
    }

    fn card_pixel(
        geometry: &Layout,
        x: i32,
        y: i32,
        glyph: u8,
        segment: &RECT,
        duplicate: bool,
        palette: &Palette,
    ) -> u32 {
        if x < 0 || x >= geometry.width || y < 0 || y >= geometry.height {
            return 0;
        }
        let contains = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if contains(&geometry.progress) {
            let color = if !duplicate && contains(segment) {
                palette.brand
            } else {
                palette.track
            };
            return premultiplied(color, 1.);
        }
        let ink = if contains(&geometry.caption) {
            palette.caption
        } else {
            palette.brand
        };
        let channel = |i: usize| {
            ((ink[i] as u32 * glyph as u32 + palette.body[i] as u32 * (255 - glyph as u32) + 127)
                / 255) as u8
        };
        premultiplied([channel(0), channel(1), channel(2)], 1.)
    }

    fn text_halo(mask: &[u8], width: usize, index: usize, radius: usize) -> u8 {
        let height = mask.len() / width;
        let (x, y) = (index % width, index / width);
        let mut coverage = mask[index];
        for yy in y.saturating_sub(radius)..=(y + radius).min(height - 1) {
            for xx in x.saturating_sub(radius)..=(x + radius).min(width - 1) {
                coverage = coverage.max(mask[yy * width + xx]);
            }
        }
        coverage
    }

    fn readable_text_palette(mut palette: Palette, halo: u8) -> Palette {
        palette.brand = [32, 43, 84];
        palette.caption = [30, 35, 44];
        // A small light keyline, not a panel-wide scrim: untouched glass pixels
        // retain their exact backdrop color and 50% backdrop/tint weighting.
        let alpha = halo as u32 * 152 / 255;
        palette.body = palette
            .body
            .map(|c| ((c as u32 * (255 - alpha) + 255 * alpha + 127) / 255) as u8);
        palette
    }

    unsafe fn fallback_region(geometry: &Layout) -> HRGN {
        CreateRectRgn(0, 0, geometry.width, geometry.height)
    }

    fn opaque_pixel(pixel: u32, background: [u8; 3]) -> u32 {
        let missing = 255 - (pixel >> 24);
        let channel =
            |shift: u32, color: u8| ((pixel >> shift) & 255) + (color as u32 * missing + 127) / 255;
        0xff000000
            | (channel(16, background[0]) << 16)
            | (channel(8, background[1]) << 8)
            | channel(0, background[2])
    }

    unsafe fn text(dc: HDC, value: &str, rect: RECT, height: i32, weight: i32, color: u32) -> bool {
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            ANTIALIASED_QUALITY as u32,
            0,
            wide("Segoe UI").as_ptr(),
        );
        if font.is_null() {
            return false;
        }
        let old = SelectObject(dc, font);
        if old.is_null() || old as isize == GDI_ERROR as isize {
            DeleteObject(font);
            return false;
        }
        SetTextColor(dc, color);
        SetBkMode(dc, TRANSPARENT as i32);
        let value = wide(value);
        let mut rect = rect;
        let drawn = DrawTextW(
            dc,
            value.as_ptr(),
            (value.len() - 1) as i32,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        ) != 0;
        SelectObject(dc, old);
        DeleteObject(font);
        drawn
    }

    unsafe fn paint(hwnd: HWND, card: &Card) {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        let target = BeginPaint(hwnd, &mut ps);
        let geometry = layout(card.scale);
        let (width, height) = (geometry.width, geometry.height);
        let dc = CreateCompatibleDC(target);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = null_mut();
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if target.is_null() || dc.is_null() || bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                DeleteObject(bitmap);
            }
            if !dc.is_null() {
                DeleteDC(dc);
            }
            EndPaint(hwnd, &ps);
            card.failed.set(true);
            return;
        }
        let old_bitmap = SelectObject(dc, bitmap);
        if old_bitmap.is_null() || old_bitmap as isize == GDI_ERROR as isize {
            DeleteObject(bitmap);
            DeleteDC(dc);
            EndPaint(hwnd, &ps);
            card.failed.set(true);
            return;
        }
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize);
        pixels.fill(0);
        let px = |n: f32| (n * card.scale).round() as i32;
        // GDI 只生成白字黑底的灰度覆盖率，不依赖其未定义的 alpha 字节，也不用色键抠字。
        let drawn = text(dc, "Neo", geometry.brand, px(32.), 650, rgb(255, 255, 255))
            && text(
                dc,
                if card.duplicate {
                    tr("Neo 已经在运行")
                } else {
                    tr("正在启动")
                },
                geometry.caption,
                px(14.),
                600,
                rgb(255, 255, 255),
            );
        // CPU 读取 DIB 前等待 GDI 批处理完成，再合成文字与玻璃色层。
        if GdiFlush() == 0 || !drawn {
            card.failed.set(true);
        }
        let palette = Palette::for_backdrop(card.backdrop.get());
        let phase = animation_phase(card.started.elapsed().as_secs_f32(), card.duplicate);
        let segment = progress_segment(&geometry, phase);
        let glass = card.glass.borrow();
        let mask: Vec<u8> = pixels.iter().map(|pixel| (*pixel & 255) as u8).collect();
        let radius = px(1.).clamp(1, 4);
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let mut palette = palette;
            if card.backdrop.get() == Backdrop::Glass {
                if let Some(background) = glass.as_ref().and_then(|pixels| pixels.get(index)) {
                    palette.body = [
                        (background >> 16) as u8,
                        (background >> 8) as u8,
                        *background as u8,
                    ];
                }
            }
            let (x, y) = (index as i32 % width, index as i32 / width);
            if card.backdrop.get() != Backdrop::HighContrast && y < geometry.progress.top {
                let near_text = [geometry.brand, geometry.caption].iter().any(|r| {
                    x >= r.left - radius
                        && x < r.right + radius
                        && y >= r.top - radius
                        && y < r.bottom + radius
                });
                let halo = if near_text {
                    text_halo(&mask, width as usize, index, radius as usize)
                } else {
                    0
                };
                palette = readable_text_palette(palette, halo);
            }
            *pixel = card_pixel(
                &geometry,
                x,
                y,
                mask[index],
                &segment,
                card.duplicate,
                &palette,
            );
        }
        if card.layered.get() {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE {
                cx: width,
                cy: height,
            };
            let origin = POINT { x: 0, y: 0 };
            if UpdateLayeredWindow(
                hwnd,
                null_mut(),
                null(),
                &size,
                dc,
                &origin,
                0,
                &blend,
                ULW_ALPHA,
            ) == 0
            {
                // 普通 GDI 降级路径使用同尺寸直角矩形，保留底边的完整进度条。
                let region = fallback_region(&geometry);
                if region.is_null() || SetWindowRgn(hwnd, region, 0) == 0 {
                    if !region.is_null() {
                        DeleteObject(region);
                    }
                    card.failed.set(true);
                } else {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style & !(WS_EX_LAYERED as isize));
                    card.layered
                        .set(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED as isize != 0);
                    if card.layered.get() {
                        card.failed.set(true);
                    }
                }
            }
        }
        if !card.layered.get() && !card.failed.get() {
            for pixel in pixels.iter_mut() {
                *pixel = opaque_pixel(*pixel, palette.body);
            }
            if BitBlt(target, 0, 0, width, height, dc, 0, 0, SRCCOPY) == 0 {
                card.failed.set(true);
            }
        }
        SelectObject(dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(dc);
        EndPaint(hwnd, &ps);
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
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
                    configure_backdrop(&*card);
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            // WS_DISABLED keeps this display-only window out of input dispatch,
            // including the non-layered fallback; HTTRANSPARENT alone only
            // forwards hit tests within the same thread. Error dialogs are separate.
            WM_NCHITTEST => HTTRANSPARENT as isize,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                let card = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Card;
                if !card.is_null() {
                    paint(hwnd, &*card);
                }
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }

    pub(super) fn surface(duplicate: bool) -> bool {
        if !duplicate
            && RUNTIME
                .get()
                .is_none_or(|r| !r.starting.load(Ordering::Acquire))
        {
            return true;
        }
        unsafe {
            // 仅修改当前线程的 DPI 上下文，不改变 eframe 的进程和窗口配置。
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                if !previous.is_null() {
                    SetThreadDpiAwarenessContext(previous);
                }
                return false;
            }
            let (mut xdpi, mut ydpi) = (96, 96);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut xdpi, &mut ydpi) < 0 {
                xdpi = 96;
            }
            let Some(scale) = screen_scale(&info.rcWork, xdpi) else {
                if !previous.is_null() {
                    SetThreadDpiAwarenessContext(previous);
                }
                return false;
            };
            let geometry = layout(scale);
            let (width, height) = (geometry.width, geometry.height);
            let instance = GetModuleHandleW(null());
            let class = wide("Neo.Startup.Card.v1");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            let registered = RegisterClassW(&wc) != 0;
            if !registered && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                if !previous.is_null() {
                    SetThreadDpiAwarenessContext(previous);
                }
                return false;
            }
            let mut card = Card {
                duplicate,
                started: Instant::now(),
                scale,
                backdrop: std::cell::Cell::new(Backdrop::Opaque),
                layered: std::cell::Cell::new(true),
                failed: std::cell::Cell::new(false),
                glass: std::cell::RefCell::new(None),
            };
            let area = info.rcWork;
            let x = area.left + (area.right - area.left - width) / 2;
            let y = area.top + (area.bottom - area.top - height) / 2;
            configure_backdrop(&card);
            if card.backdrop.get() != Backdrop::HighContrast {
                *card.glass.borrow_mut() = glass::capture(x, y, width, height);
                configure_backdrop(&card);
            }
            // Initialization may finish while the one-shot CPU blur is running.
            // Do not flash a late splash over the already-ready main window.
            if !duplicate
                && RUNTIME
                    .get()
                    .is_none_or(|r| !r.starting.load(Ordering::Acquire))
            {
                if registered {
                    UnregisterClassW(class.as_ptr(), instance);
                }
                if !previous.is_null() {
                    SetThreadDpiAwarenessContext(previous);
                }
                return true;
            }
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                class.as_ptr(),
                wide("Neo").as_ptr(),
                WS_POPUP | WS_DISABLED,
                x,
                y,
                width,
                height,
                null_mut(),
                null_mut(),
                instance,
                (&mut card as *mut Card).cast(),
            );
            if hwnd.is_null() {
                if registered {
                    UnregisterClassW(class.as_ptr(), instance);
                }
                if !previous.is_null() {
                    SetThreadDpiAwarenessContext(previous);
                }
                return false;
            }
            // Submit the frozen glass and sharp text before showing; no recurring
            // capture, no capture of the card itself, no dependency on DWM effects.
            InvalidateRect(hwnd, null(), 0);
            paint(hwnd, &card);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            InvalidateRect(hwnd, null(), 0);
            UpdateWindow(hwnd);
            'surface: loop {
                let done = if duplicate {
                    card.started.elapsed() >= Duration::from_millis(1300)
                } else {
                    RUNTIME
                        .get()
                        .is_none_or(|r| !r.starting.load(Ordering::Acquire))
                };
                if done || card.failed.get() || IsWindow(hwnd) == 0 {
                    break;
                }
                let mut message: MSG = std::mem::zeroed();
                // 每轮限制消息数，避免消息洪水饿死退出检查。
                for _ in 0..64 {
                    if PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) == 0 {
                        break;
                    }
                    if message.message == WM_QUIT {
                        break 'surface;
                    }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                    if IsWindow(hwnd) == 0 {
                        break 'surface;
                    }
                }
                if !duplicate {
                    InvalidateRect(hwnd, null(), 0);
                }
                // 最高 25 帧，限制 CPU 占用；不依赖 GPU 或主界面线程。
                std::thread::sleep(Duration::from_millis(40));
            }
            if IsWindow(hwnd) != 0 {
                DestroyWindow(hwnd);
            }
            if registered {
                UnregisterClassW(class.as_ptr(), instance);
            }
            if !previous.is_null() {
                SetThreadDpiAwarenessContext(previous);
            }
            !card.failed.get()
        }
    }

    #[cfg(test)]
    mod tests {
        include!("startup_native_tests.rs");
    }
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
