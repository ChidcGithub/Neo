//! neo-overlay：全屏流光跑马灯覆盖层（对标 Apple Intelligence 满血特效）。
//!
//! 架构：独立窗口线程 + 专属 wgpu/Dx12 渲染，与 egui 主界面完全解耦。
//!
//! **为什么不用 winit**：winit 的 `EventLoop` 是进程级单例
//! （`static EVENT_LOOP_CREATED: AtomicBool`），eframe 主界面已经占掉，
//! 覆盖层只能用裸 Win32 自建窗口与消息循环。
//!
//! 关键点：
//! - DX12 透明交换链必须用 `Dx12SwapchainKind::DxgiFromVisual`（默认 DxgiFromHwnd 只有 Opaque alpha）
//! - `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 让抓屏拍不到自己，从而能持续抓桌面做实时折射
//! - `WS_EX_TRANSPARENT` 全屏窗口鼠标穿透，`WS_EX_NOACTIVATE` + `SW_SHOWNA` 显示不抢焦点
//! - 抓屏线程 ~30fps 把桌面帧喂给渲染线程，shader 做边缘折射采样
//! - 控制命令走 mpsc + `PostThreadMessageW` 唤醒（消息循环在隐藏时整块睡眠）
//! - 视觉为 VCC edgeglow v2 移植：0.6x 离屏渲染光环 + 线性放大 blit 上屏（低频内容几乎无损，省 ~65% GPU）

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use neo_tools::tools::screen::{self, Shot};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PeekMessageW,
    PostThreadMessageW, RegisterClassExW, SetWindowDisplayAffinity, ShowWindow, TranslateMessage,
    CS_HREDRAW, CS_VREDRAW, MSG, PM_REMOVE, SW_HIDE, SW_SHOWNA, WDA_EXCLUDEFROMCAPTURE, WM_APP,
    WM_QUIT, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

/// 相位目标表（VCC TARGETS 照搬）：(强度, 速度, 环流)。
const PH_IDLE: (f32, f32, f32) = (0.00, 0.50, 0.015);
const PH_LISTEN: (f32, f32, f32) = (0.76, 0.85, 0.07);
/// 离屏渲染比例（VCC RENDER_SCALE=0.6：光环是低频内容，线性放大几乎无损）。
const RENDER_SCALE: f32 = 0.6;
/// 线程消息：命令队列里有货，唤醒睡眠中的消息循环。
const WM_NEO_CMD: u32 = WM_APP + 1;

/// 发给窗口线程的命令。
#[derive(Debug)]
enum Cmd {
    Show,
    Hide,
    Shutdown,
}

/// 抓屏帧槽：抓屏线程放最新一帧，渲染线程每帧取走。
type FrameSlot = Arc<Mutex<Option<Shot>>>;

/// 跑马灯控制柄，跨线程安全。Drop 时自动关停窗口线程。
pub struct OverlayHandle {
    tx: Sender<Cmd>,
    /// 窗口线程的 Win32 线程 id（PostThreadMessage 唤醒用）。
    tid: u32,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayHandle {
    /// 显示全屏跑马灯并开始渲染。
    pub fn show(&self) {
        self.post(Cmd::Show);
    }

    /// 隐藏跑马灯（淡出完成后暂停渲染，省电）。
    pub fn hide(&self) {
        self.post(Cmd::Hide);
    }

    /// 设置音频电平（0.0..=1.0，通常取 RMS），驱动光带起伏。
    pub fn set_level(&self, v: f32) {
        self.level.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Relaxed)
    }

    /// 关停窗口线程并等待退出。
    pub fn shutdown(&mut self) {
        self.post(Cmd::Shutdown);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// 投递命令并唤醒窗口线程（线程已退出时安静地丢弃）。
    fn post(&self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            unsafe {
                PostThreadMessageW(self.tid, WM_NEO_CMD, 0, 0);
            }
        }
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 启动跑马灯引擎（窗口初始隐藏，等 `show()`）。
///
/// 窗口与 wgpu 初始化在窗口线程里同步完成，失败（无 DX12 / 建窗失败）
/// 通过 `Err` 上报，调用方降级为无跑马灯即可。
pub fn start() -> Result<OverlayHandle, String> {
    let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let visible = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    // 窗口线程初始化完成后回传线程 id（命令唤醒要靠它）。
    let (ready_tx, ready_rx) = channel::<Result<u32, String>>();
    let thread = {
        let level = level.clone();
        let visible = visible.clone();
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("neo-overlay".into())
            .spawn(move || run(ready_tx, cmd_rx, level, visible, stop))
            .map_err(|e| format!("创建 neo-overlay 线程失败: {e}"))?
    };
    let tid = ready_rx
        .recv()
        .map_err(|_| "neo-overlay 线程意外退出".to_string())??;
    Ok(OverlayHandle {
        tx: cmd_tx,
        tid,
        level,
        visible,
        stop,
        thread: Some(thread),
    })
}

/// 窗口线程主函数。
fn run(
    ready: Sender<Result<u32, String>>,
    cmd_rx: Receiver<Cmd>,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    // 抓屏线程：可见时 ~30fps 抓虚拟桌面，供 shader 折射采样。
    let slot: FrameSlot = Arc::new(Mutex::new(None));
    {
        let slot = slot.clone();
        let visible = visible.clone();
        let stop = stop.clone();
        // spawn 失败（系统线程资源枯竭等）不必带崩窗口：slot 永远为空 →
        // desktop 保持 1x1 占位纹理 → render() 里 refr 自动为 0，
        // 退回纯光环模式继续跑。
        let _ = std::thread::Builder::new()
            .name("neo-overlay-cap".into())
            .spawn(move || {
                screen::ensure_dpi_aware();
                while !stop.load(Ordering::Relaxed) {
                    if !visible.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(80));
                        continue;
                    }
                    let rect = screen::virtual_screen();
                    if let Ok(shot) = screen::capture(rect) {
                        if let Ok(mut g) = slot.lock() {
                            *g = Some(shot);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(33));
                }
            });
    }

    let tid = unsafe { GetCurrentThreadId() };
    match Gfx::new(slot) {
        Ok(gfx) => {
            if ready.send(Ok(tid)).is_err() {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            msg_loop(gfx, cmd_rx, level, visible, stop);
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// 消息循环：可见时连续渲染（交换链 vsync 限速），隐藏时整块睡眠等命令。
fn msg_loop(
    mut gfx: Gfx,
    cmd_rx: Receiver<Cmd>,
    level: Arc<AtomicU32>,
    visible: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    let hwnd = gfx.hwnd;
    let mut active = false;
    // 淡出中：渲染循环继续，等强度归零再真正隐藏窗口。
    let mut hiding = false;
    let mut shutdown = false;
    while !shutdown {
        // 先排空命令队列（PostThreadMessage 只负责唤醒，命令本体在 channel 里）
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Cmd::Show => {
                    hiding = false;
                    active = true;
                    visible.store(true, Ordering::Relaxed);
                    gfx.tgt = PH_LISTEN;
                    // SW_SHOWNA：显示但不激活，焦点不能被抢（用户可能正在打字）。
                    unsafe {
                        ShowWindow(hwnd, SW_SHOWNA);
                    }
                }
                Cmd::Hide => {
                    if active {
                        // 目标相位归零，淡出完成后在渲染循环里真正隐藏
                        hiding = true;
                        gfx.tgt = PH_IDLE;
                    } else {
                        visible.store(false, Ordering::Relaxed);
                    }
                }
                Cmd::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            break;
        }
        if active {
            // 非阻塞泵消息：线程消息不需要分发，系统消息（如 WM_QUIT）要处理
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        shutdown = true;
                        break;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            gfx.render(&level);
            // 淡出完成：真正隐藏窗口并停渲染
            if hiding && gfx.cur_i < 0.02 {
                hiding = false;
                active = false;
                visible.store(false, Ordering::Relaxed);
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
        } else {
            // 隐藏期整块睡眠，WM_NEO_CMD 到达即醒
            let mut msg = MSG::default();
            let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if r <= 0 {
                // 0 = WM_QUIT，-1 = 出错
                break;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    unsafe {
        DestroyWindow(hwnd);
    }
}

/// 窗口过程：覆盖层不响应任何交互（鼠标穿透），全部走默认处理。
unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 桌面折射纹理。
struct DesktopTex {
    tex: wgpu::Texture,
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// 0.6x 离屏光环纹理（blit 放大上屏）。
struct Offscreen {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
    blit_bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// 离屏 blit shader：全屏三角形线性放大采样（独立模块避免绑定冲突）。
const BLIT_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0)
    );
    var out: VsOut;
    let xy = p[vi];
    out.pos = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - xy.y * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(src, smp, in.uv, 0.0);
}
"#;

/// wgpu 渲染上下文。
struct Gfx {
    hwnd: HWND,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buf: wgpu::Buffer,
    desktop: DesktopTex,
    offscreen: Offscreen,
    slot: FrameSlot,
    /// 防截屏是否设置成功；失败时禁用折射（抓屏拍到的是自己，会反馈循环）。
    exclude_ok: bool,
    // 相位插值状态（VCC curI/curS/curR + tAcc 积分照搬）
    tgt: (f32, f32, f32),
    cur_i: f32,
    cur_s: f32,
    cur_r: f32,
    t_acc: f32,
    last: Instant,
    smooth_level: f32,
}

impl Gfx {
    fn new(slot: FrameSlot) -> Result<Self, String> {
        screen::ensure_dpi_aware();
        let rect = screen::virtual_screen();
        let width = rect.width.max(1) as u32;
        let height = rect.height.max(1) as u32;

        // 覆盖整个虚拟桌面（可多显示器、原点可为负）的全屏无边框置顶窗口。
        // WS_EX_TRANSPARENT = 鼠标穿透；WS_EX_NOACTIVATE = 永不抢焦点；
        // WS_EX_TOOLWINDOW = 不进任务栏与 Alt+Tab。
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class_name: Vec<u16> = "neo-overlay\0".encode_utf16().collect();
        let wc = WNDCLASSEXW {
            cbSize: core::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        // 重复注册（同进程重启引擎）返回 0 + ERROR_CLASS_ALREADY_EXISTS，正常放行；
        // 其他失败必须就地拦住，否则 CreateWindowExW 只会报出误导性的"找不到类"。
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            let err = unsafe { GetLastError() };
            if err != ERROR_CLASS_ALREADY_EXISTS {
                return Err(format!("RegisterClassExW 失败: win32 错误 {err}"));
            }
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                rect.x,
                rect.y,
                width as i32,
                height as i32,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err("CreateWindowExW 失败".into());
        }
        // 让窗口对截屏不可见（抓屏线程才能拍到干净的桌面做折射）。
        // WDA_EXCLUDEFROMCAPTURE 需 Win10 2004+；低版本或远程会话等场景下
        // 返回 FALSE。失败后抓屏会拍到跑马灯自己，折射形成反馈循环 ——
        // 记下来，整轮禁用折射（只画光环），比"越转越亮"体面得多。
        let exclude_ok = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;

        // 强制 DX12 + DirectComposition 交换链（否则窗口不支持透明）
        let desc = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            backend_options: wgpu::BackendOptions {
                dx12: wgpu::Dx12BackendOptions {
                    presentation_system: wgpu::Dx12SwapchainKind::DxgiFromVisual,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        let instance = wgpu::Instance::new(desc);
        // 裸 HWND 建 surface：没有 winit 窗口可借，走 raw handle 通道（'static）。
        let win_handle = raw_window_handle::Win32WindowHandle::new(
            core::num::NonZeroIsize::new(hwnd as isize).expect("hwnd 非空"),
        );
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_window_handle: raw_window_handle::RawWindowHandle::Win32(win_handle),
            raw_display_handle: Some(raw_window_handle::RawDisplayHandle::Windows(
                raw_window_handle::WindowsDisplayHandle::new(),
            )),
        };
        let surface =
            unsafe { instance.create_surface_unsafe(target) }.map_err(|e| e.to_string())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| e.to_string())?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| e.to_string())?;

        // 交换链配置：BGRA + 预乘 alpha
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .unwrap_or(caps.formats[0]);
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            caps.alpha_modes[0]
        };
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes[0]
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("overlay-uniform"),
            size: 48, // 10 个 f32 + 2 个 vec2（见 shader.wgsl Uniforms）
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // blit 绑定布局：离屏纹理 + 采样器
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("overlay-blit-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("overlay-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        // 光环管线输出到 0.6x 离屏纹理（Rgba8Unorm）
        let offscreen_format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: offscreen_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // blit 管线：离屏纹理线性放大到交换链
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("overlay-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("overlay-blit-pl"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-blit-pipeline"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // 初始 1x1 黑色桌面纹理占位（第一帧抓屏到达后重建）
        let desktop = {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("overlay-desktop"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &[0, 0, 0, 255],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            let view = tex.create_view(&Default::default());
            let bind = Self::make_bind_group(&device, &bind_layout, &uniform_buf, &view, &sampler);
            DesktopTex {
                tex,
                bind,
                w: 1,
                h: 1,
            }
        };

        let offscreen = Self::make_offscreen(
            &device,
            &blit_layout,
            &sampler,
            ((config.width as f32) * RENDER_SCALE).max(1.0) as u32,
            ((config.height as f32) * RENDER_SCALE).max(1.0) as u32,
        );

        Ok(Self {
            hwnd,
            surface,
            device,
            queue,
            config,
            pipeline,
            blit_pipeline,
            bind_layout,
            sampler,
            uniform_buf,
            desktop,
            offscreen,
            slot,
            exclude_ok,
            tgt: PH_IDLE,
            cur_i: PH_IDLE.0,
            cur_s: PH_IDLE.1,
            cur_r: PH_IDLE.2,
            t_acc: 0.0,
            last: Instant::now(),
            smooth_level: 0.0,
        })
    }

    fn make_offscreen(
        device: &wgpu::Device,
        blit_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        w: u32,
        h: u32,
    ) -> Offscreen {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay-offscreen"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let blit_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-blit-bg"),
            layout: blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Offscreen {
            _tex: tex,
            view,
            blit_bind,
            w,
            h,
        }
    }

    fn make_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        uniform: &wgpu::Buffer,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-bg"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// 上传最新抓屏帧；尺寸变化时重建纹理与 bind group。
    fn upload_desktop(&mut self, shot: Shot) {
        // 防截屏失败的帧是"自拍"，上传只会喂养折射反馈循环；丢弃。
        if !self.exclude_ok {
            return;
        }
        // 跨线程来的数据，长度不符时 write_texture 会直接 panic —— 宁可丢帧。
        if shot.rgba.len() != shot.width as usize * shot.height as usize * 4 {
            return;
        }
        if shot.width != self.desktop.w || shot.height != self.desktop.h {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("overlay-desktop"),
                size: wgpu::Extent3d {
                    width: shot.width,
                    height: shot.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = Self::make_bind_group(
                &self.device,
                &self.bind_layout,
                &self.uniform_buf,
                &view,
                &self.sampler,
            );
            self.desktop = DesktopTex {
                tex,
                bind,
                w: shot.width,
                h: shot.height,
            };
        }
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.desktop.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &shot.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.desktop.w),
                rows_per_image: Some(self.desktop.h),
            },
            wgpu::Extent3d {
                width: self.desktop.w,
                height: self.desktop.h,
                depth_or_array_layers: 1,
            },
        );
    }

    fn render(&mut self, level: &AtomicU32) {
        // 取最新抓屏帧
        let shot = self.slot.lock().ok().and_then(|mut g| g.take());
        if let Some(shot) = shot {
            self.upload_desktop(shot);
        }

        // 帧率无关步长
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;

        // 相位插值（VCC：k = 1-exp(-dt*3.2)，tAcc 按速度积分）
        let k = 1.0 - (-dt * 3.2).exp();
        self.cur_i += (self.tgt.0 - self.cur_i) * k;
        self.cur_s += (self.tgt.1 - self.cur_s) * k;
        self.cur_r += (self.tgt.2 - self.cur_r) * k;
        self.t_acc += dt * self.cur_s.max(0.05);

        // 音频电平包络（VCC：攻击 22/s，释放 4.5/s，帧率无关）
        let target = f32::from_bits(level.load(Ordering::Relaxed));
        let kl = if target > self.smooth_level {
            1.0 - (-dt * 22.0).exp()
        } else {
            1.0 - (-dt * 4.5).exp()
        };
        self.smooth_level += (target - self.smooth_level) * kl;

        // 抓屏帧就位后才开折射（否则采样到 1x1 黑占位纹理）
        let refr = if self.desktop.w > 1 { 1.0 } else { 0.0 };

        let uni: [f32; 12] = [
            self.t_acc,
            self.smooth_level,
            self.cur_i,
            self.cur_r,
            refr,
            0.0,
            0.0,
            0.0,
            self.offscreen.w as f32,
            self.offscreen.h as f32,
            self.desktop.w as f32,
            self.desktop.h as f32,
        ];
        self.queue
            .write_buffer(&self.uniform_buf, 0, bytemuck::cast_slice(&uni));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => {
                f
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("overlay-enc"),
            });
        // pass 1：光环 → 0.6x 离屏纹理
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.offscreen.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.desktop.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        // pass 2：离屏纹理线性放大 → 交换链
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipeline);
            pass.set_bind_group(0, &self.offscreen.blit_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(enc.finish()));
        self.queue.present(frame);
    }
}
