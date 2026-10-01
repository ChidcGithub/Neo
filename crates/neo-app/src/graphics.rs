//! 驱动可能直接访问违例：只在独立进程内试 DX12 / Glow，绝不枚举 Vulkan。
//! 主窗口首帧前 run_native 失败保留 pending；本次退出，下次启动才尝试后备。
//! 两者均失败可在 Neo 退出后运行 `neo --reset-renderer`，单实例检查后清缓存重探测。
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};

const PROBE_ARG: &str = "--neo-render-probe";
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);
const SCREENSHOT_RETRY: Duration = Duration::from_millis(500);
const CACHE_VERSION: &str = concat!("1/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend { Dx12, Glow }

impl Backend {
    fn name(self) -> &'static str {
        match self { Self::Dx12 => "dx12", Self::Glow => "glow" }
    }
}

pub fn options(backend: Backend) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer: match backend {
            Backend::Dx12 => eframe::Renderer::Wgpu,
            Backend::Glow => eframe::Renderer::Glow,
        },
        // Glow 不使用 wgpu；仍限定配置，防止未来误用默认后端枚举。
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: crate::window_wgpu_setup().into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn parse_probe(args: &[OsString]) -> Option<Result<Backend, ()>> {
    if !args.iter().any(|arg| arg == PROBE_ARG) { return None; }
    Some(match args {
        [flag, backend] if flag == PROBE_ARG && backend == "dx12" => Ok(Backend::Dx12),
        [flag, backend] if flag == PROBE_ARG && backend == "glow" => Ok(Backend::Glow),
        _ => Err(()),
    })
}

/// Some 表示内部命令（含非法参数），调用方必须立即退出，不进入 startup。
pub fn probe_command() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    parse_probe(&args).map(|backend| match backend {
        Ok(backend) => if run_probe(backend) { 0 } else { 1 },
        Err(()) => 2,
    })
}

// eframe 0.36 没有 App::after_rendering。Screenshot 事件是在实际绘制、
// GPU 回读后送回的；不能以 ui 调用次数或 AppCreator 返回判定渲染成功。
#[derive(Default)]
struct FirstFrame {
    started: Option<Instant>,
    last_request: Option<Instant>,
    received: bool,
    timed_out: bool,
}
struct FrameReceipt;

impl FirstFrame {
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        self.poll_at(ctx, Instant::now())
    }

    fn poll_at(&mut self, ctx: &egui::Context, now: Instant) -> bool {
        if self.received { return true; }
        let started = *self.started.get_or_insert(now);
        // 超时为终态，迟到回执不能重新放行业务或覆盖 pending。
        self.timed_out |= now.saturating_duration_since(started) >= FRAME_TIMEOUT;
        if !self.timed_out {
            self.received = ctx.input(|input| input.events.iter().any(|event| {
                matches!(event, egui::Event::Screenshot { viewport_id, user_data, image }
                    if *viewport_id == egui::ViewportId::ROOT
                        && user_data.data.as_ref().is_some_and(|data| data.is::<FrameReceipt>())
                        && image.size[0] > 0 && image.size[1] > 0)
            }));
        }
        self.received
    }

    fn request(&mut self, ctx: &egui::Context) {
        self.request_at(ctx, Instant::now());
    }

    fn request_at(&mut self, ctx: &egui::Context, now: Instant) {
        if self.received || self.timed_out { return; }
        let started = *self.started.get_or_insert(now);
        if now.saturating_duration_since(started) >= FRAME_TIMEOUT { return; }
        // Surface Lost/Timeout 可能吞掉截图命令；限频重试，不能每帧堆积 GPU 回读。
        if self.last_request.is_none_or(|last| now.saturating_duration_since(last) >= SCREENSHOT_RETRY) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(FrameReceipt)));
            self.last_request = Some(now);
        }
        ctx.request_repaint_after(Duration::from_millis(20));
    }
}

struct ProbeApp { frame: FirstFrame, rendered: Arc<AtomicBool> }

impl eframe::App for ProbeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        ui.painter().rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(30, 90, 180));
        ui.label("Neo GPU"); // 同时覆盖字体纹理上传和几何绘制。
        if self.frame.poll(ui.ctx()) {
            self.rendered.store(true, Ordering::Relaxed);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.frame.timed_out {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            self.frame.request(ui.ctx());
        }
    }
}

fn run_probe(backend: Backend) -> bool {
    let rendered = Arc::new(AtomicBool::new(false));
    let result = rendered.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([96.0, 64.0])
            // 保持可渲染，不能 Visible(false)；屏幕外避免探测图案闪现。
            .with_position([-32000.0, -32000.0])
            .with_decorations(false)
            .with_resizable(false)
            .with_active(false)
            .with_taskbar(false)
            .with_mouse_passthrough(true),
        persist_window: false,
        ..options(backend)
    };
    eframe::run_native("Neo renderer probe", options, Box::new(move |_| {
        Ok(Box::new(ProbeApp { frame: FirstFrame::default(), rendered }))
    })).is_ok() && result.load(Ordering::Relaxed)
}

fn probe(backend: Backend) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false; };
    let mut command = Command::new(exe);
    command.args([PROBE_ARG, backend.name()])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW：debug 构建也不闪控制台。
    }
    let Ok(mut child) = command.spawn() else { return false; };
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                // 无论超时还是 wait 错误，都终止并回收；不可遗留卡住的驱动进程。
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    version: String,
    ready: Option<Backend>,
    pending: Option<Backend>,
    failed: Vec<Backend>,
}

impl Cache {
    fn decode(text: &str) -> Self {
        serde_json::from_str::<Self>(text).ok()
            .filter(|cache| cache.version == CACHE_VERSION)
            .unwrap_or_else(|| Self { version: CACHE_VERSION.into(), ..Self::default() })
    }

    fn recover(&mut self) {
        if let Some(backend) = self.pending.take() {
            self.reject(backend);
        }
        if self.ready.is_some_and(|backend| self.failed.contains(&backend)) {
            self.ready = None;
        }
    }

    fn reject(&mut self, backend: Backend) {
        if !self.failed.contains(&backend) { self.failed.push(backend); }
        self.ready = None;
        self.pending = None;
    }
}

fn cache_path(home: Option<OsString>, appdata: Option<OsString>) -> Option<PathBuf> {
    home.map(PathBuf::from).filter(|p| p.is_absolute())
        .or_else(|| appdata.map(PathBuf::from).filter(|p| p.is_absolute()).map(|p| p.join("Neo")))
        .map(|p| p.join("graphics.json"))
}

fn save(path: &PathBuf, cache: &Cache) -> io::Result<()> {
    fs::create_dir_all(path.parent().expect("cache directory"))?;
    // 写前清空意味着断电至多留下坏缓存（重新探测），不会信任旧的成功记录。
    let mut file = fs::File::create(path)?;
    file.write_all(serde_json::to_string(cache)?.as_bytes())?;
    file.sync_all()
}

fn select_with(
    cache: &mut Cache,
    mut probe: impl FnMut(Backend) -> bool,
    mut persist: impl FnMut(&Cache) -> io::Result<()>,
) -> io::Result<Backend> {
    cache.recover();
    if let Some(backend) = cache.ready.take() {
        cache.pending = Some(backend);
        persist(cache)?;
        return Ok(backend);
    }
    for backend in [Backend::Dx12, Backend::Glow] {
        if cache.failed.contains(&backend) { continue; }
        cache.pending = Some(backend);
        // 在启动子进程之前落盘，连主进程被强退也不会下次重试同一驱动。
        persist(cache)?;
        if probe(backend) { return Ok(backend); }
        cache.reject(backend);
        persist(cache)?;
    }
    persist(cache)?;
    Err(io::Error::other("DX12 and Glow unavailable; run neo --reset-renderer to retry"))
}

pub struct Selection { pub backend: Backend, path: PathBuf, cache: Cache }

/// 必须在持有单实例守卫时重置；重复启动不会触碰正在运行实例的缓存。
pub fn select(_startup: &crate::startup::Startup) -> io::Result<Selection> {
    let path = cache_path(std::env::var_os("NEO_HOME"), std::env::var_os("APPDATA"))
        .ok_or_else(|| io::Error::other("NEO_HOME or APPDATA must be an absolute path"))?;
    if std::env::args_os().skip(1).any(|arg| arg == "--reset-renderer") {
        match fs::remove_file(&path) {
            Ok(()) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::InvalidData) => String::new(),
        Err(error) => return Err(error),
    };
    let mut cache = Cache::decode(&text);
    let backend = select_with(&mut cache, |backend| {
        let ok = probe(backend);
        if !ok {
            eprintln!("[neo] renderer probe failed: {}", backend.name());
            crate::startup::record_error("window", "renderer probe failed");
        }
        ok
    }, |cache| save(&path, cache))?;
    eprintln!("[neo] renderer: {}", backend.name());
    Ok(Selection { backend, path, cache })
}

/// startup::fatal 的对话框只显示日志位置；把固定恢复指引放在那里，
/// 不另弹对话框绕过课堂静默策略，也不持久化原始错误中的用户路径。
pub fn startup_failed(reason: &str) {
    const HELP: &str = "Neo 渲染初始化失败。\n\
若主窗口首帧前失败，请重新启动一次，Neo 会跳过该后端并尝试后备。\n\
若两个后端均失败，更新显卡驱动后退出 Neo，运行 neo.exe --reset-renderer 重新探测。\n\
若 graphics.json 无法读写，为避免重复进入故障驱动，Neo 本次停止启动。\n\
请确保 NEO_HOME（若设置）或 %APPDATA%/Neo 可写，或将 NEO_HOME 设置到可写的绝对路径。\n\
缓存位置：NEO_HOME/graphics.json，否则 %APPDATA%/Neo/graphics.json。\n";
    eprintln!("[neo] graphics: {reason}\n{HELP}");
    if let Some(dir) = crate::startup::log_dir() {
        let _ = fs::write(dir.join("graphics-help.txt"), HELP);
    }
    crate::startup::fatal("window", "renderer initialization failed");
}

impl Selection {
    pub fn track(self, app: crate::app::NeoApp) -> impl eframe::App {
        TrackedApp { app, selection: Some(self), frame: FirstFrame::default() }
    }
}

// 只包装当前 NeoApp 实现的三个 App 方法；不改动 app.rs 或业务生命周期。
struct TrackedApp<A> {
    app: A,
    selection: Option<Selection>,
    frame: FirstFrame,
}

impl<A> TrackedApp<A> {
    fn ready(&mut self, ctx: &egui::Context) -> bool {
        let was_timed_out = self.frame.timed_out;
        if !self.frame.poll(ctx) {
            if self.frame.timed_out {
                if !was_timed_out {
                    crate::startup::record_error("window", "first frame receipt timed out");
                    eprintln!("[neo] first frame receipt timed out; restart to try fallback");
                    crate::startup::finish_splash();
                }
                // 不调 modal fatal / process::exit：正常退回 main，让 Startup 守卫释放。
                // selection 不落盘，原 pending 保留供下次启动选择后备。
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
            return false;
        }
        if self.selection.is_some() {
            let mut selection = self.selection.take().unwrap();
            selection.cache.pending = None;
            selection.cache.ready = Some(selection.backend);
            if save(&selection.path, &selection.cache).is_err() {
                crate::startup::record_error("window", "renderer cache write failed");
            }
        }
        true
    }
}

impl<A: eframe::App> eframe::App for TrackedApp<A> {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // logic 先于 ui，start_in_tray / 桌面工具都不能提前隐藏主窗口。
        if self.ready(ctx) { self.app.logic(ctx, frame); }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.ready(ui.ctx()) {
            self.app.ui(ui, frame);
        } else {
            ui.painter().rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
            self.frame.request(ui.ctx());
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;
