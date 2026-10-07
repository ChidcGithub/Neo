//! 迷你窗：AI 后台执行期间的屏幕角落小窗。
//!
//! - 主窗藏到托盘且 AI 正在生成 / 执行工具时，贴在屏幕右上角；
//! - AI 一截屏就暂时消失（`neo_tools::tools::screen::SCREENSHOT_AT` 置位），
//!   截完带着一个轻微的淡入回来 —— 截图里永远不会有 Neo 自己的窗；
//! - AI 的鼠标移进小窗区域时，小窗非线性躲到左上角，鼠标离开后弹回；
//! - AI 执行期间，无论前后台或是否使用桌面工具，用户点击屏幕弹出「打断确认」，
//!   确认即 [`AppState::cancel`]。
//!
//! 实现要点：
//! - deferred 视口**每帧无条件重注册**：egui 会回收真渲染 pass 里没被触碰的
//!   子视口，不注册就等着被销毁；
//! - 显隐走双通道：builder 的 `with_visible` 由 patch 在真 pass 收增量，
//!   显式 `ViewportCommand::Visible` 兜底主窗托盘后的 logic-only 路径；
//! - 主窗托盘后主视口的 repaint 被 eframe 节流到 ~10fps，所以**动画全部在
//!   小窗视口自己的回调里自驱**（避让 / 淡入 / 边缘流光），主视口只管
//!   10fps 的内容快照；窗口位置也由回调发 `OuterPosition`，builder 不碰。

use crate::i18n::tr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, Context, Id, Pos2, Rect, Stroke, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId, WindowLevel,
};
use neo_theme::{SquirclePaint, Theme};
use neo_ui::{Button, Design};

use crate::state::{AppState, Role, ToolState};

/// 避让动画时长（秒）：右上 ↔ 左上。
const AVOID_SECS: f32 = 0.35;
/// 截屏后重新露面的淡入时长（秒）。
const REFADE_SECS: f32 = 0.25;
/// 截屏信号置位后小窗保持隐藏的时长（`capture()` 内已先等 300ms 再抓帧）。
/// 950ms = 截屏闪光（450ms 起亮 + 450ms 播放）结束后再回归 ——
/// 闪光与小窗淡入叠在一起，看着就像「小窗一弹出屏幕就闪一下」。
const SHOT_HIDE: Duration = Duration::from_millis(950);
/// 小窗开着时的轮询节拍（避让 / 截屏信号 / 打断点击都靠它）。
/// 16ms ≈ 60fps：避让、淡入、呼吸点都是动画，再低肉眼能看出顿。
const POLL: Duration = Duration::from_millis(16);
/// 任务执行完后小窗继续驻留的时长：老师扫一眼角落就知道「做完了」，
/// 然后播淡出动画消失（见 `FADEOUT_SECS`），而不是无声瞬没。
const LINGER: Duration = Duration::from_secs(5);
/// 驻留结束的淡出时长（透明度渐隐 + 轻轻上飘）。
const FADEOUT_SECS: f32 = 0.45;
/// 卡片高度自适应动画时长（朝测量出的目标高缓动）。
const HEIGHT_ANIM_SECS: f32 = 0.22;
/// 工具流水最多保留的步数（旧的滚出，最近的排最下）。
const MAX_STEPS: usize = 4;
/// 被动视口的休眠位：常态缩 1x1 移出屏幕，避免透明 surface 恢复时闪黑。
/// 桌面屏障仍可隐藏它们；交互弹窗不使用此休眠机制，关闭后撤销整个视口。
pub(crate) const OFFSCREEN: Pos2 = Pos2::new(-16000.0, -16000.0);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-miniwin")
}

/// overlay 的点是全局物理坐标 / 主屏 DPI；不能减虚拟桌面原点，
/// 原点平移由 overlay 自己完成。deferred 视口仍使用自己的原有坐标系。
#[derive(Clone, Copy)]
pub(crate) struct ScreenGeometry {
    pub monitor: Vec2,
    ppp: f32,
}

impl ScreenGeometry {
    fn from_physical(size: Vec2, ppp: f32) -> Self {
        Self {
            monitor: size / ppp,
            ppp,
        }
    }

    fn point(self, physical: Pos2) -> Pos2 {
        physical / self.ppp
    }

    fn rect(self, physical: neo_tools::tools::screen::Rect, origin: Pos2) -> Rect {
        Rect::from_min_size(
            self.point(Pos2::new(physical.x as f32, physical.y as f32) - origin.to_vec2()),
            Vec2::new(physical.width as f32, physical.height as f32) / self.ppp,
        )
    }
}

#[cfg(all(windows, not(test)))]
fn primary_geometry() -> ScreenGeometry {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    // 屏幕分辨率/DPI 在运行时几乎不变，缓存结果避免每帧 2 次 Win32 调用。
    // 不监听 WM_DISPLAYCHANGE：变化概率极低，下次启动自然纠正。
    static CACHED: std::sync::OnceLock<ScreenGeometry> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        let _dpi = neo_tools::tools::screen::physical_pixels().ok();
        let ppp = neo_tools::tools::screen::dpi_scale_at(0, 0).unwrap_or(1.0) as f32;
        let size = unsafe {
            Vec2::new(
                GetSystemMetrics(SM_CXSCREEN) as f32,
                GetSystemMetrics(SM_CYSCREEN) as f32,
            )
        };
        ScreenGeometry::from_physical(size, ppp)
    })
}

#[cfg(any(not(windows), test))]
fn primary_geometry() -> ScreenGeometry {
    ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), 1.0)
}

pub(crate) fn screen_geometry(ctx: &Context, using_overlay: bool) -> ScreenGeometry {
    if using_overlay {
        primary_geometry()
    } else {
        ctx.input(|i| ScreenGeometry {
            monitor: i
                .viewport()
                .monitor_size
                .unwrap_or(Vec2::new(1920.0, 1080.0)),
            ppp: i.viewport().native_pixels_per_point.unwrap_or(1.0),
        })
    }
}

/// fallback 的屏幕点只属于 child；这里只缓存动画位置和 DPI，不参与输入排除。
#[derive(Clone, Copy)]
struct FallbackGeometry {
    screen: ScreenGeometry,
    position: Pos2,
}

impl FallbackGeometry {
    fn new(screen: ScreenGeometry, position: Pos2) -> Self {
        Self { screen, position }
    }

    fn screen(ctx: &Context) -> ScreenGeometry {
        let zoom = ctx.zoom_factor();
        ctx.input(|i| {
            i.raw.viewports.get(&viewport_id()).and_then(|v| {
                Some(ScreenGeometry {
                    monitor: v.monitor_size?,
                    ppp: v.native_pixels_per_point? * zoom,
                })
            })
        })
        .or_else(|| {
            ctx.data(|d| d.get_temp::<Self>(Id::new("neo-miniwin-pos")))
                .map(|cached| cached.screen)
        })
        .unwrap_or_else(|| {
            let primary = primary_geometry();
            ScreenGeometry {
                monitor: primary.monitor / zoom,
                ppp: primary.ppp * zoom,
            }
        })
    }

    fn store(self, ctx: &Context) {
        let id = Id::new("neo-miniwin-pos");
        let last = ctx.data(|d| d.get_temp::<Self>(id));
        // 相同点坐标在 DPI / egui zoom 变化后不是同一个物理位置，必须重发。
        let moved = last.is_none_or(|last| {
            last.position != self.position || last.screen.ppp != self.screen.ppp
        });
        ctx.data_mut(|d| d.insert_temp(id, self));
        if moved {
            ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(self.position));
        }
    }
}

/// 缓出三次方：避让与淡入共用这条「先快后慢」的曲线。
fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

#[cfg(any(windows, test))]
mod mouse_hook;

/// 全局鼠标状态：光标位置（屏幕物理像素）+ 左键此刻是否按下。
/// 主窗隐藏后 egui 拿不到任何输入，只能绕到系统 API 轮询。
#[cfg(all(windows, not(test)))]
fn global_cursor() -> Option<(Pos2, bool)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) == 0 {
            return None;
        }
        // 0x8000 = 此刻按着；0x0001 = 自上次调用以来按过 —— 托盘态 ~10fps 的
        // 采样会漏掉比间隔还快的点击（打断确认不弹），两个位都要。
        let lmb = (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8001) != 0;
        Some((Pos2::new(point.x as f32, point.y as f32), lmb))
    }
}

/// 非 Windows 没有全局输入轮询（小窗退化为只读状态牌）。
#[cfg(any(not(windows), test))]
fn global_cursor() -> Option<(Pos2, bool)> {
    None
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 回调（60fps）和 tick 共用采样器：GetAsyncKeyState 的低位不可靠且读后清除，
/// 任一调用读到的沿都必须保留给 tick。即使截屏/桌面屏障暂停绘制，tick 仍采样。
/// 命中检测使用按下时的位置，不能用消费时已移走的光标判断课堂卡点击。
#[derive(Clone, Copy, Default)]
struct GlobalInput {
    cursor: Option<Pos2>,
    down: bool,
    edge: Option<(u64, Pos2)>,
}

impl GlobalInput {
    fn sample(&mut self, sample: Option<(Pos2, bool)>, now: u64, synthetic_at: u64) {
        let down = sample.is_some_and(|(_, down)| down);
        if down && !self.down && !synthetic_click_at(now, synthetic_at) {
            self.edge = sample.map(|(pos, _)| (now, pos));
        }
        // 合成按下也更新基线，避免窗口过期后把仍按住的拖动当成用户新点击。
        self.down = down;
        self.cursor = sample.map(|(pos, _)| pos);
    }
}

static GLOBAL_INPUT: Mutex<GlobalInput> = Mutex::new(GlobalInput {
    cursor: None,
    down: false,
    edge: None,
});

fn poll_global_input() -> GlobalInput {
    let mut input = GLOBAL_INPUT.lock().unwrap_or_else(|p| p.into_inner());
    let sample = global_cursor();
    let synthetic_at = neo_tools::tools::screen::SYNTHETIC_INPUT_AT.load(Ordering::Relaxed);
    input.sample(sample, now_ms(), synthetic_at);
    *input
}

/// GetAsyncKeyState 不区分注入来源；沿用工具层的 1s 保守排除窗口。
/// 必须在采样时过滤，不能等 tick 时窗口已过期再把合成沿认作用户点击。
fn synthetic_click_at(now: u64, last: u64) -> bool {
    last != 0 && now.saturating_sub(last) < 1000
}

/// 留尾部 max 个字符（最新的内容），被裁就补个省略号。
fn tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s
        .chars()
        .rev()
        .take(max)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{kept}")
}

/// 留头部 max 个字符（动作名在开头），被裁就补个省略号。
/// 工具摘要用它：「读文件 crates/…/main.rs」砍尾比砍头可读。
fn head(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let kept: String = s.chars().take(max).collect();
    format!("{kept}…")
}

/// 工具流水里一步的状态色（与主窗工具卡片的成败判定一致）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum StepTone {
    /// 执行中 / 待确认：accent 呼吸点 + 亮文本。
    Active,
    /// 已成功：success 点。
    Ok,
    /// 失败 / 被拒：error 点。
    Failed,
    /// 被打断：中性灰（取消不是失败）。
    Muted,
}

/// 工具流水里的一步：单行摘要 + 状态色。
#[derive(Clone)]
struct Step {
    line: String,
    tone: StepTone,
}

/// 取最后一段正文（markdown 段落按空行分隔；代码栅栏内不拆 —
/// 栅栏数配不平就向前合并，直到成对）。
fn last_paragraph(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = text.split("\n\n").collect();
    let mut start = parts.len() - 1;
    let mut fences = parts[start].matches("```").count();
    while fences % 2 == 1 && start > 0 {
        start -= 1;
        fences += parts[start].matches("```").count();
    }
    parts[start..].join("\n\n")
}

/// 一帧要画的内容。视口回调是 `'static` 的，数据必须整份搬走。
///
/// 注意**没有** fade、位置与高度：主视口藏托盘后被 eframe 节流到 10fps，
/// 这些动画量改由回调自驱（fade 走共享槽、位置走 `OuterPosition`、
/// 高度朝 `target_h` 做 egui 动画）。
#[derive(Clone)]
struct Snapshot {
    theme: Theme,
    /// 卡片宽（避让端点判定要用）。
    width: f32,
    /// 目标高：tick 按上帧量到的内容高算出（自适应正文），回调朝它动画。
    target_h: f32,
    /// 主显示器尺寸（点）：渲染层里 `ctx` 的 monitor_size 不可靠
    /// （那是整层窗口），避让端点判定由快照下发。
    monitor: Vec2,
    /// 已完成驻留态（忙完后的 5s）：正文切到最后一段完整渲染。
    done: bool,
    steps: Vec<Step>,
    /// 正文（markdown 源串，由 `markdown::render` 渲染，支持 LaTeX）。
    body: String,
    /// 正文是占位文案（「正在处理…」）时的淡色标记。
    body_dim: bool,
    streaming: bool,
}

impl Snapshot {
    fn build(
        state: &AppState,
        theme: Theme,
        width: f32,
        target_h: f32,
        monitor: Vec2,
        done: bool,
    ) -> Self {
        let last_assistant = state
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant);
        let streaming = last_assistant.is_some_and(|msg| msg.streaming);
        // 正文只放正式回复；content 还空着时给占位。
        let (body_src, body_dim) = match last_assistant {
            Some(msg) if !msg.content.trim().is_empty() => (msg.content.trim(), false),
            _ => (tr("正在处理…"), true),
        };
        // 本轮工具流水（自最后一条用户消息起）：最近 MAX_STEPS 步，旧的在上。
        // 倒序收集到量即停，再翻回正序 —— 老消息成堆时不全扫。
        let m = theme.metrics;
        let inner_w = m.s(340.0 - 28.0);
        let step_px = theme.typo.caption.max(1.0);
        let step_chars = ((inner_w - m.s(12.0)) / step_px).floor().max(6.0) as usize;
        let mut steps: Vec<Step> = Vec::new();
        for msg in state.messages.iter().rev() {
            if msg.role == Role::User {
                break;
            }
            if let Some(tool) = &msg.tool {
                let tone = match tool.state {
                    ToolState::Running | ToolState::AwaitingConfirm => StepTone::Active,
                    ToolState::Cancelled => StepTone::Muted,
                    ToolState::Denied => StepTone::Failed,
                    ToolState::Done => {
                        if tool.ok() {
                            StepTone::Ok
                        } else {
                            StepTone::Failed
                        }
                    }
                };
                steps.push(Step {
                    // 工具摘要多行命令（如 `$paths = @(\n…`）必须压成单行：
                    // 行高按一行算，多行会溢出去盖住下一条。
                    line: head(&tool.line().replace(['\r', '\n'], " "), step_chars),
                    tone,
                });
                if steps.len() >= MAX_STEPS {
                    break;
                }
            }
        }
        steps.reverse();
        // 正文：执行中只留尾巴三行（流式一瞥）；完成后换成**最后一段**的
        // 完整 markdown（卡片高度会自适应到放得下它）。
        let body = if done {
            last_paragraph(body_src)
        } else {
            let body_px = theme.typo.body.max(1.0);
            let per_line = (inner_w / body_px).floor().max(6.0) as usize;
            tail(body_src, per_line * 3)
        };
        Self {
            theme,
            width,
            target_h,
            monitor,
            done,
            steps,
            body,
            body_dim,
            streaming,
        }
    }
}

fn interrupt_builder(theme: Theme, monitor: Vec2) -> ViewportBuilder {
    let m = theme.metrics;
    let size = Vec2::new(m.s(380.0), m.s(280.0))
        .min((monitor - Vec2::splat(m.s(32.0))).max(Vec2::splat(1.0)));
    let position = Pos2::new((monitor.x - size.x - m.s(16.0)).max(0.0), m.s(16.0));
    ViewportBuilder::default()
        .with_title(tr("Neo · 打断执行？"))
        .with_decorations(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_always_on_top()
        .with_active(false)
        // 整个客户区拦截输入，不能因透明像素/圆角把正文点击送到桌面。
        .with_transparent(false)
        .with_mouse_passthrough(false)
        .with_visible(true)
        .with_inner_size(size)
        .with_position(position)
}

fn paint_interrupt(ui: &mut egui::Ui, theme: Theme, generation: u64, answer: &AtomicU8) {
    theme.apply(ui.ctx());
    let d = Design::new(theme);
    let m = d.m();
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, d.p().bg_layer_1);
    let inner = rect.shrink(m.s(18.0));
    super::at(ui, inner, |ui| {
        ui.label(egui::RichText::new(tr("打断执行？")).font(d.font_bold(d.t().headline)));
        ui.add_space(m.s(12.0));
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr("AI 本轮正在执行任务。打断会立即取消当前任务。"))
                    .font(d.font(d.t().body))
                    .color(d.p().label_secondary),
            )
            .wrap(),
        );
    });
    let footer = Rect::from_min_max(
        Pos2::new(inner.left(), inner.bottom() - m.s(48.0)),
        inner.max,
    );
    super::at(ui, footer, |ui| {
        ui.spacing_mut().item_spacing.x = m.s(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (button, value) in [
                (Button::new(tr("打断")).danger(), 1u8),
                (Button::new(tr("继续")).elevated(), 2u8),
            ] {
                let response = button
                    .id_salt(("neo-interrupt", generation, value))
                    .enabled(answer.load(Ordering::Acquire) == 0)
                    .show(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(Id::new(("neo-interrupt-button", value)), response.rect)
                });
                if response.clicked() {
                    let _ = answer.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire);
                }
            }
        });
    });
}

/// 画一帧小窗内容：工具流水 + 正文（markdown / LaTeX 渲染）。
///
/// 正文用组件布局（不再是纯 painter 排版），量到的内容高写进 `measure`
/// 槽 —— tick 下一帧据此算卡片目标高（自适应最后一段正文）。
fn paint(ui: &mut egui::Ui, snap: &Snapshot, fade: f32, measure: &Arc<Mutex<f32>>) {
    let d = Design::new(snap.theme);
    let p = d.p();
    let m = d.m();
    // 卡片矩形：旧视口里 = 视口内容区；渲染层里 = Area 钉住的卡矩形。
    // max_rect 在两个世界都恰好是它（content_rect 在层里会是整层窗口）。
    let rect = ui.max_rect();
    let painter = ui.painter().clone();
    let tint = |c: Color32| c.gamma_multiply(fade);

    // 假阴影 + 卡片：透明视口上先垫一层偏移的暗 squircle，浮起来的层次感。
    painter.squircle_filled(
        rect.translate(Vec2::new(0.0, m.s(2.0))),
        m.s(18.0),
        tint(Color32::from_black_alpha(26)),
    );
    painter.squircle_filled(rect, m.s(18.0), tint(p.bg_layer_1));
    painter.squircle_stroked(rect, m.s(18.0), Stroke::new(1.0, tint(p.border_l1)));

    let inner = rect.shrink(m.s(14.0));
    let step_lh = d.t().caption * 1.55;
    super::at(ui, inner, |ui| {
        ui.set_opacity(fade);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.set_max_width(inner.width());

        // 工具流水：状态圆点 + 单行摘要，最近的在最下。
        for step in &snap.steps {
            let (row, _) =
                ui.allocate_exact_size(Vec2::new(inner.width(), step_lh), egui::Sense::hover());
            let dot = match step.tone {
                StepTone::Active => p.accent,
                StepTone::Ok => p.success,
                StepTone::Failed => p.error,
                StepTone::Muted => p.label_caption,
            };
            let ink = match step.tone {
                StepTone::Active => p.label_primary,
                _ => p.label_secondary,
            };
            ui.painter().circle_filled(
                Pos2::new(row.left() + m.s(3.0), row.center().y),
                m.s(3.0),
                tint(dot),
            );
            ui.painter().text(
                Pos2::new(row.left() + m.s(12.0), row.top()),
                Align2::LEFT_TOP,
                &step.line,
                d.font(d.t().caption),
                tint(ink),
            );
        }
        if !snap.steps.is_empty() {
            ui.add_space(m.s(6.0));
        }

        // 正文：占位文案淡色直排；正式内容走 markdown（CommonMark + LaTeX，
        // 与主界面同一条渲染管线）。
        if snap.body_dim {
            ui.label(
                egui::RichText::new(&snap.body)
                    .font(d.font(d.t().body))
                    .color(p.label_secondary),
            );
        } else {
            let whale = crate::brand::WhaleMark::cached(ui.ctx());
            let skin = super::Skin::new(snap.theme, &whale);
            super::markdown::render(ui, &skin, &snap.body, snap.streaming);
        }

        // 量到的内容高写回共享槽：卡片高度自适应的依据（tick 下一帧读取）。
        *measure.lock().unwrap() = ui.min_rect().height();
    });

    // 边缘流光 = 「还在跑」的持续信号：完成驻留与淡出时不再播。
    if !snap.done {
        let now = ui.ctx().input(|i| i.time);
        paint_border_beam(&painter, rect, m.s(18.0), now, tint(p.accent), m.s(1.4));
    }
}

#[derive(Clone, Copy, PartialEq)]
struct BeamKey {
    size: Vec2,
    radius: f32,
    superellipse: f32,
    segments: usize,
}

impl BeamKey {
    fn new(size: Vec2, radius: f32) -> Self {
        Self {
            size,
            radius,
            superellipse: neo_theme::HARNESS_SUPERELLIPSE,
            segments: neo_theme::squircle::DEFAULT_SEGMENTS,
        }
    }
}

/// 只缓存局部坐标；避让、淡出上飘和窗口原点变化都不重建几何。
#[derive(Default)]
struct BeamGeometry {
    key: Option<BeamKey>,
    points: Vec<Pos2>,
    cumulative: Vec<f32>,
    total: f32,
}

impl BeamGeometry {
    fn ensure(&mut self, key: BeamKey) -> bool {
        if self.key == Some(key) {
            return false;
        }
        self.points = neo_theme::squircle::squircle_points(
            Rect::from_min_size(Pos2::ZERO, key.size).shrink(0.5),
            key.radius,
            key.superellipse,
            key.segments,
        );
        self.cumulative.clear();
        self.cumulative.reserve(self.points.len() + 1);
        self.cumulative.push(0.0);
        for i in 0..self.points.len() {
            self.cumulative.push(
                self.cumulative[i]
                    + self.points[i].distance(self.points[(i + 1) % self.points.len()]),
            );
        }
        self.total = *self.cumulative.last().unwrap();
        self.key = Some(key);
        true
    }

    fn at(&self, s: f32) -> Pos2 {
        let s = s.rem_euclid(self.total);
        let idx = match self
            .cumulative
            .binary_search_by(|c| c.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
        .min(self.points.len() - 1);
        let seg = (self.cumulative[idx + 1] - self.cumulative[idx]).max(f32::EPSILON);
        let t = ((s - self.cumulative[idx]) / seg).clamp(0.0, 1.0);
        self.points[idx] + (self.points[(idx + 1) % self.points.len()] - self.points[idx]) * t
    }

    fn tail_points(&self, now: f64) -> [Pos2; 29] {
        let head = ((now / 2.4).fract() as f32) * self.total;
        let tail = self.total * 0.30;
        std::array::from_fn(|i| self.at(head - tail + tail * (i as f32 / 28.0)))
    }
}

/// 边缘流光：沿弧长匀速循环；每帧只更新相位、平移与颜色。
fn paint_border_beam(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    now: f64,
    base: Color32,
    width: f32,
) {
    // Context 独立持有缓存，tick 重建绘制闭包不会丢失它；只克隆 Arc，不克隆 Vec。
    let cache = painter.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Arc<Mutex<BeamGeometry>>>(Id::new("neo-miniwin-beam"))
            .clone()
    });
    let mut geometry = cache.lock().unwrap();
    geometry.ensure(BeamKey::new(rect.size(), radius));
    if geometry.points.len() < 2 || geometry.total <= 0.0 {
        return;
    }
    // 相邻线段复用端点：56 次二分变为 29 次，栈数组不分配。
    let points = geometry.tail_points(now);
    for (i, pair) in points.windows(2).enumerate() {
        let k = (i + 1) as f32 / 28.0;
        painter.line_segment(
            [pair[0] + rect.min.to_vec2(), pair[1] + rect.min.to_vec2()],
            Stroke::new(width, base.gamma_multiply(k * k)),
        );
    }
}

fn suspend_answer(answer: &AtomicU8) {
    let _ = answer.compare_exchange(0, u8::MAX, Ordering::AcqRel, Ordering::Acquire);
}

/// 任务身份由 state 的显式入口提供，与消息内容、位置和流的轮次无关。
#[derive(Clone, Copy, PartialEq, Eq)]
struct TaskIdentity {
    session_epoch: u64,
    task_epoch: u64,
}

impl TaskIdentity {
    fn of(state: &AppState) -> Self {
        Self {
            session_epoch: state.session_epoch,
            task_epoch: state.task_epoch,
        }
    }
}

/// 快照缓存：内容指纹 + 构建好的 Snapshot。
/// 指纹只跟踪「内容会不会变」的字段，不含几何/动画参数。
///
/// 不走 `Default`：`Option<CachedSnapshot>` 的 `None` 就是默认态。
struct CachedSnapshot {
    /// 最后一条 Assistant 消息的指针（内容、流式态、错误、工具流水都挂在它后面）。
    last_assistant_ptr: usize,
    /// 最后一条 Assistant 消息的内容长度（流式期间每帧都在长）。
    last_assistant_len: usize,
    /// 最后一条 Assistant 消息的流式标记。
    last_assistant_streaming: bool,
    /// 消息总数（新消息到来时指纹变）。
    messages_len: usize,
    /// 本轮工具状态指纹（自最后一条 User 起的 ToolState 序列）。
    tool_states: Vec<ToolState>,
    /// 完成后驻留标记（切换时正文从流式尾巴换成最后一段完整渲染）。
    done: bool,
    /// 构建好的快照。
    snapshot: Snapshot,
}

/// 迷你窗运行时状态，挂在 `NeoApp` 上，每帧由 [`MiniWin::tick`] 驱动。
#[derive(Default)]
pub struct MiniWin {
    /// 上一帧是否已发 `Visible(true)`（显隐改由显式命令驱动，沿检测用）。
    shown: bool,
    using_overlay: bool,
    /// 已见到的截屏时间戳（变了 = AI 又截了一次屏）。
    shot_seen: u64,
    /// 截屏隐藏的截止时刻。
    shot_hide_until: Option<Instant>,
    /// 截屏后重新露面的淡入起点。**与视口回调共享**：主窗藏托盘后主视口的
    /// repaint 被 eframe 节流到 10fps（`INVISIBLE_WINDOW_REPAINT_INTERVAL`），
    /// 250ms 的淡入若靠快照下发只剩两三帧 —— 改由回调里现取现算。
    refade_since: Arc<Mutex<Option<Instant>>>,
    /// 未决打断请求；挂起时关闭交互视口，但保留已经提交的同任务回执。
    interrupt_open: bool,
    /// 0 未决 / 1 打断 / 2 继续 / MAX 已撤销。旧闭包只能 CAS 自己的 token。
    interrupt_result: Arc<AtomicU8>,
    interrupt_viewport: Option<ViewportId>,
    interrupt_generation: u64,
    /// 显式任务代次隔离同一消息的重试；工具回灌仍属于同一请求。
    interrupt_task: Option<TaskIdentity>,
    input_task: Option<TaskIdentity>,
    session_epoch: u64,
    /// 已消费的左键按下沿时刻（同一沿在 250ms 窗口内不重复触发）。
    lmb_edge_consumed: u64,
    /// 仅监听已经开始的执行；启动/审批点击不能成为打断请求。
    input_armed: bool,
    #[cfg(any(windows, test))]
    mouse_hook: Option<mouse_hook::MouseHook>,
    #[cfg(any(windows, test))]
    hook_attempted: bool,
    #[cfg(any(windows, test))]
    physical_sequence: u64,
    #[cfg(test)]
    mock_clicks: Option<Arc<mouse_hook::ClickSlot>>,
    /// 忙完后的驻留截止时刻（忙时持续刷新，闲下来那一刻起算 5s）。
    linger_until: Option<Instant>,
    /// 淡出起点（驻留结束 → 播完才收窗）；回调里读它算透明度与上飘，
    /// 所以走共享槽（与 refade 同理：主视口托盘态只有 10fps）。
    fadeout_since: Arc<Mutex<Option<Instant>>>,
    /// 内容高度测量槽：paint 写（回调里现量）、tick 读（算目标高）。
    content_h: Arc<Mutex<f32>>,
    /// 测试回退路径最近下发的视口尺寸（没变就不重发 InnerSize）。
    legacy_sent: Vec2,
    /// 渲染层卡片的共享矩形（主屏点）：tick 写静止位、绘制闭包写避让位。
    /// 仅供层的 Area 定位；穿透的状态牌不能成为打断请求的排除区。
    layer_rect: Arc<Mutex<[f32; 4]>>,
    /// 快照缓存：消息指针 + 工具状态指纹，没变就不重建 Snapshot。
    cached_snap: Option<CachedSnapshot>,
}

impl Drop for MiniWin {
    fn drop(&mut self) {
        self.interrupt_result.store(u8::MAX, Ordering::Release);
    }
}

impl MiniWin {
    fn consume_click(&mut self, input: GlobalInput, now: u64) -> bool {
        let Some((at, _)) = input.edge else {
            return false;
        };
        if at == 0 || at == self.lmb_edge_consumed {
            return false;
        }
        // 不合格/过期的沿也消费，不能在确认卡关闭或下一轮开始后重放。
        self.lmb_edge_consumed = at;
        now.saturating_sub(at) < 250
    }

    fn arm_input(&mut self, state: &AppState) -> bool {
        let eligible = (state.generating || state.tool_open || state.tool_round)
            && state.awaiting_tool().is_none();
        let task = TaskIdentity::of(state);
        let armed = self.input_armed && self.input_task.as_ref() == Some(&task);
        self.input_task = Some(task);
        self.input_armed = eligible;
        armed && eligible
    }

    fn task_click(&mut self, state: &AppState, input: GlobalInput, now: u64) -> bool {
        let armed = self.arm_input(state);
        // 启动前/首帧的沿照常消费，按住发送按钮也必须先松开才能再触发。
        self.consume_click(input, now) && armed
    }

    #[cfg(any(windows, test))]
    fn physical_click(&mut self, state: &AppState, click: Option<mouse_hook::Click>) -> bool {
        let armed = self.arm_input(state);
        let Some(click) = click else { return false };
        if click.sequence == self.physical_sequence {
            return false;
        }
        // 不使用毫秒时间戳/250ms过期窗口：UI 暂避或忙碌不能丢掉真实请求。
        self.physical_sequence = click.sequence;
        armed
    }

    /// 外层 None = hook 不可用；内层 None = 可用但尚无真实点击。
    #[cfg(any(windows, test))]
    fn hook_click(&mut self) -> Option<Option<mouse_hook::Click>> {
        #[cfg(test)]
        if let Some(slot) = &self.mock_clicks {
            return Some(slot.latest());
        }
        if !self.hook_attempted {
            self.hook_attempted = true;
            #[cfg(all(windows, not(test)))]
            match mouse_hook::MouseHook::start() {
                Ok(hook) => {
                    self.mouse_hook = Some(hook);
                    self.input_armed = false;
                }
                Err(error) => {
                    eprintln!("{error}；退回鼠标轮询（注入后 1s 内的真实点击可能被忽略）")
                }
            }
        }
        if self
            .mouse_hook
            .as_ref()
            .is_some_and(mouse_hook::MouseHook::finished)
        {
            eprintln!("鼠标 hook 线程已退出；退回鼠标轮询（注入后 1s 内的真实点击可能被忽略）");
            self.mouse_hook = None;
            self.input_armed = false; // 切换输入源不能重放回退采样器的旧沿。
        }
        self.mouse_hook.as_ref().map(mouse_hook::MouseHook::latest)
    }

    fn poll_task_click(&mut self, state: &AppState) -> (Option<Pos2>, bool, bool) {
        #[cfg(any(windows, test))]
        if let Some(click) = self.hook_click() {
            return (
                click.map(|c| c.position),
                self.physical_click(state, click),
                false,
            );
        }
        let input = poll_global_input();
        let synthetic = synthetic_click_at(
            now_ms(),
            neo_tools::tools::screen::SYNTHETIC_INPUT_AT.load(Ordering::Relaxed),
        );
        (
            input.edge.map(|(_, pos)| pos),
            self.task_click(state, input, now_ms()),
            synthetic,
        )
    }

    fn request_interrupt(&mut self, state: &AppState, armed: bool, on_card: bool, synthetic: bool) {
        if armed
            && (state.generating || state.tool_open || state.tool_round)
            && !self.interrupt_open
            && state.awaiting_tool().is_none() // 包括 ask_user 提问卡。
            && !on_card
            && !synthetic
        {
            self.interrupt_open = true;
            self.interrupt_task = Some(TaskIdentity::of(state));
            self.interrupt_result.store(u8::MAX, Ordering::Release);
            self.interrupt_result = Arc::new(AtomicU8::new(0));
        }
    }

    fn switch_backend(&mut self, ctx: &Context, using_overlay: bool) {
        if self.using_overlay != using_overlay {
            // 打断弹窗独立于视觉后端；切换 Card/fallback 不能丢失回执。
            self.using_overlay = using_overlay;
            self.shown = false;
            self.legacy_sent = Vec2::ZERO;
            *self.layer_rect.lock().unwrap() = [0.0; 4];
            ctx.data_mut(|d| {
                d.remove::<FallbackGeometry>(Id::new("neo-miniwin-pos"));
            });
            if using_overlay {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::Close);
            }
        }
    }

    /// 撤销先于显隐命令：即使旧 deferred callback 排队未执行也不能回答新请求。
    fn retire_interrupt(&mut self, ctx: &Context) {
        self.interrupt_result.store(u8::MAX, Ordering::Release);
        self.close_interrupt_viewport(ctx);
    }

    fn suspend_interrupt(&mut self, ctx: &Context) {
        // CAS 和按钮提交竞争：先提交的有效回执必须留给 root，不能被隐藏覆盖。
        suspend_answer(&self.interrupt_result);
        self.close_interrupt_viewport(ctx);
    }

    fn close_interrupt_viewport(&mut self, ctx: &Context) {
        if let Some(id) = self.interrupt_viewport.take() {
            ctx.send_viewport_cmd_to(id, ViewportCommand::Visible(false));
            ctx.send_viewport_cmd_to(id, ViewportCommand::Close);
            // 不再注册该视口，下一次完整 pass 回收 HWND 和 egui 的按下状态。
            ctx.request_repaint_of(ViewportId::ROOT);
        }
    }

    fn show_interrupt(&mut self, ctx: &Context, theme: Theme) {
        let id = *self.interrupt_viewport.get_or_insert_with(|| {
            self.interrupt_generation += 1;
            ViewportId::from_hash_of(("neo-miniwin-interrupt", self.interrupt_generation))
        });
        let generation = self.interrupt_generation;
        let answer = self.interrupt_result.clone();
        let screen = primary_geometry();
        let builder = interrupt_builder(theme, screen.monitor / ctx.zoom_factor());
        ctx.show_viewport_deferred(id, builder, move |ui, _| {
            if crate::app::desktop_suspended(ui.ctx()) || answer.load(Ordering::Acquire) == u8::MAX
            {
                suspend_answer(&answer);
                ui.ctx()
                    .send_viewport_cmd_to(id, ViewportCommand::Visible(false));
                ui.ctx().request_repaint_of(ViewportId::ROOT);
                return;
            }
            if ui.input(|i| i.viewport().close_requested()) {
                let _ = answer.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
            }
            paint_interrupt(ui, theme, generation, &answer);
            if answer.load(Ordering::Acquire) != 0 {
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }

    /// 视口 builder：尺寸按显隐给（休眠 1x1），patch 自己收成增量命令。
    ///
    /// 两个反直觉处：
    /// - **永不切 `Visible`**（恒 true）：透明视口从隐藏切回可见的首帧
    ///   surface 无内容，DWM 补一帧黑 —— 休眠改走「1x1 + 屏幕外」；
    /// - **不带位置**：位置的动画在回调里以 `OuterPosition` 命令自驱，
    ///   builder 给了就会被 patch 拉回（`None` = 不碰）。创建时的初始
    ///   位置是系统默认，但彼时 1x1 全透明，无感。
    fn builder(size: Vec2, open: bool) -> ViewportBuilder {
        ViewportBuilder::default()
            .with_title("Neo")
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_active(false)
            // 透明：squircle 卡片四个角外不该是一块直角底色。
            .with_transparent(true)
            .with_mouse_passthrough(true)
            .with_visible(true)
            .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
    }

    /// 获取内容快照：指纹没变就复用缓存，变了才重建。
    ///
    /// 快照依赖 `state.messages` 的内容（最后一条 Assistant + 工具流水）。
    /// 流式生成期间内容每帧都在变，指纹自然不命中，走全量构建；
    /// 但驻留/淡出/静息期间内容不变，指纹命中，零字符串分配。
    fn snapshot_for(
        &mut self,
        state: &AppState,
        theme: Theme,
        width: f32,
        target_h: f32,
        monitor: Vec2,
        done: bool,
    ) -> Snapshot {
        let last_assistant = state
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant);
        let ptr = last_assistant.map_or(0, |m| m as *const _ as usize);
        let len = last_assistant.map_or(0, |m| m.content.len());
        let streaming = last_assistant.is_some_and(|m| m.streaming);
        let messages_len = state.messages.len();
        // 本轮工具状态指纹（自最后一条 User 起，最多 MAX_STEPS 个）。
        let mut tool_states = Vec::new();
        for msg in state.messages.iter().rev() {
            if msg.role == Role::User {
                break;
            }
            if let Some(tool) = &msg.tool {
                tool_states.push(tool.state);
                if tool_states.len() >= MAX_STEPS {
                    break;
                }
            }
        }
        tool_states.reverse();

        // 指纹比对：全等才复用。
        if let Some(cached) = &self.cached_snap {
            if cached.last_assistant_ptr == ptr
                && cached.last_assistant_len == len
                && cached.last_assistant_streaming == streaming
                && cached.messages_len == messages_len
                && cached.tool_states == tool_states
                && cached.done == done
            {
                // 几何参数（宽高/显示器）可能变了，更新它们但不重建内容。
                let mut snap = cached.snapshot.clone();
                snap.theme = theme;
                snap.width = width;
                snap.target_h = target_h;
                snap.monitor = monitor;
                self.cached_snap = Some(CachedSnapshot {
                    last_assistant_ptr: ptr,
                    last_assistant_len: len,
                    last_assistant_streaming: streaming,
                    messages_len,
                    tool_states,
                    done,
                    snapshot: snap.clone(),
                });
                return snap;
            }
        }

        let snapshot = Snapshot::build(state, theme, width, target_h, monitor, done);
        self.cached_snap = Some(CachedSnapshot {
            last_assistant_ptr: ptr,
            last_assistant_len: len,
            last_assistant_streaming: streaming,
            messages_len,
            tool_states,
            done,
            snapshot: snapshot.clone(),
        });
        snapshot
    }

    /// 每帧驱动一次。放在 `tick()` 里而不是 `render()` 里：
    /// 主窗隐藏时渲染循环走 logic-only 路径，两者都经过 `tick()`。
    ///
    /// `hidden_to_tray` 也包含桌面工具导致的主窗自动暂避。
    /// `classwin_rect`：`ClassWin::visible_rect_physical(ctx)` 返回的全局物理
    /// 客户区；不可传布局目标矩形或 ROOT 的逻辑坐标。
    pub fn tick(
        &mut self,
        ctx: &Context,
        state: &mut AppState,
        theme: Theme,
        hidden_to_tray: bool,
        classwin_rect: Option<Rect>,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        let overlay = overlay.filter(|layer| layer.is_alive());
        self.switch_backend(ctx, overlay.is_some());
        if self.session_epoch != state.session_epoch {
            self.session_epoch = state.session_epoch;
            self.input_armed = false;
            self.interrupt_open = false;
            self.retire_interrupt(ctx);
        }
        let task = TaskIdentity::of(state);
        if self.interrupt_open && self.interrupt_task.as_ref() != Some(&task) {
            self.interrupt_open = false;
            self.retire_interrupt(ctx);
        }
        let geometry = if overlay.is_some() {
            primary_geometry()
        } else {
            FallbackGeometry::screen(ctx)
        };

        // 1. 截屏信号：时间戳一变就进入短暂隐藏；隐藏结束后播淡入。
        let shot_at = neo_tools::tools::screen::SCREENSHOT_AT.load(Ordering::Relaxed);
        if shot_at != self.shot_seen {
            self.shot_seen = shot_at;
            if shot_at != 0 {
                self.shot_hide_until = Some(Instant::now() + SHOT_HIDE);
            }
        }
        if self
            .shot_hide_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.shot_hide_until = None;
            *self.refade_since.lock().unwrap() = Some(Instant::now());
        }
        let shot_hiding = self.shot_hide_until.is_some();

        // 2. 状态牌用于主窗隐藏时；打断确认在前后台都可展示。
        //    忙完后驻留 LINGER（5s）播完成态，驻留结束再播一段淡出才收窗。
        let busy = state.generating || state.tool_open || state.tool_round;
        if busy {
            self.linger_until = Some(Instant::now() + LINGER);
            // 重新忙起来：进行中的淡出立即取消（接着播内容）。
            *self.fadeout_since.lock().unwrap() = None;
        }
        let lingering = !busy && self.linger_until.is_some_and(|t| Instant::now() < t);
        if !busy {
            // 完成态可以驻留，但未决确认不能跨越任务结束后继续有效。
            self.interrupt_open = false;
            self.retire_interrupt(ctx);
        }
        // 驻留结束 → 淡出（回调里播透明度 + 上飘），播完才真正收窗。
        // 只在「曾露过面」时起播：小窗没开过的静默期不该刷出一张淡出中的卡。
        let mut fadeout = self.fadeout_since.lock().unwrap();
        if !busy && !lingering && fadeout.is_none() && self.shown && !shot_hiding {
            *fadeout = Some(Instant::now());
        }
        let fading = fadeout.is_some();
        let faded = fadeout.is_some_and(|t| t.elapsed().as_secs_f32() >= FADEOUT_SECS);
        if faded {
            *fadeout = None;
            self.linger_until = None;
        }
        drop(fadeout);

        // 3. hook 记录真实按下的物理位置/序号，桌面工具注入期间也能触发。
        //    安装失败才退回原采样；回调仍轮询光标用于避让，不负责打断来源。
        let (cursor, lmb_edge, synthetic) = self.poll_task_click(state);

        // 4. 几何：常态贴右上、避让时躲左上。宽度固定；高度**自适应内容** ——
        //    paint 量到的内容高（上一帧）+ 边距，钳制后由回调朝它做缓动。

        let m = theme.metrics;
        let monitor = geometry.monitor;
        let content = *self.content_h.lock().unwrap();
        let (width, target_h) = (
            m.s(340.0),
            (content + m.s(28.0)).clamp(m.s(96.0), (monitor.y - m.s(32.0)) * 0.62),
        );
        let size = Vec2::new(width, target_h);
        let margin = m.s(16.0);
        let home = Pos2::new((monitor.x - width - margin).max(margin), margin);

        // 5. 只排除真实交互窗口；状态牌的 actual/home/away 都穿透到桌面。
        //    确认/提问等待态与已打开的打断弹窗由 request_interrupt 排除。
        if lmb_edge {
            // hook 留存的是按下时的物理坐标；矩形是最新视口采样，非点击时归属。
            let on_classwin = cursor
                .zip(classwin_rect)
                .is_some_and(|(c, r)| r.contains(c));
            self.request_interrupt(state, true, on_classwin, synthetic);
        }
        // 截屏/桌面屏障期间仍可记住真实桌面点击请求，但不接受新的弹窗回答。
        if busy {
            ctx.request_repaint_after(POLL);
        }
        let can_show = !shot_hiding && !crate::app::desktop_suspended(ctx);
        // 先验证任务，再消费回执；桌面挂起/截图/审批不能抹掉已经有效的回答。
        if self.interrupt_open {
            if can_show
                && state.awaiting_tool().is_none()
                && self.interrupt_viewport.is_some_and(|id| {
                    ctx.input(|i| {
                        i.raw
                            .viewports
                            .get(&id)
                            .is_some_and(|v| v.close_requested())
                    })
                })
            {
                let _ = self.interrupt_result.compare_exchange(
                    0,
                    2,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            match self.interrupt_result.load(Ordering::Acquire) {
                1 => {
                    state.cancel();
                    self.interrupt_open = false;
                }
                2 => self.interrupt_open = false,
                _ => {}
            }
        }
        if !self.interrupt_open {
            self.retire_interrupt(ctx);
        } else if !can_show || state.awaiting_tool().is_some() {
            self.suspend_interrupt(ctx);
        } else {
            if self.interrupt_result.load(Ordering::Acquire) == u8::MAX {
                // callback 也可能已暂停 token；新的按钮必须使用新 HWND/代次。
                self.close_interrupt_viewport(ctx);
                self.interrupt_result = Arc::new(AtomicU8::new(0));
            }
            self.show_interrupt(ctx, theme);
        }

        // 状态牌和弹窗互斥，不能在同一位置叠两种输入角色。
        let open = can_show
            && !self.interrupt_open
            && hidden_to_tray
            && (busy || lingering || (fading && !faded));

        // 6. 显隐与内容。
        //
        //    渲染层路径：每帧覆写卡片（与旧视口「每帧注册防回收」同构），
        //    避让/淡入动画由卡片的绘制闭包在层里以 vsync 自驱 —— 主视口
        //    托盘态 10fps 的节拍只负责内容快照，与旧架构的权责划分一致。
        //
        //    快照有内容指纹：消息没变时不重建，省去每帧的字符串克隆与遍历。
        let done = !busy && (lingering || fading);
        let snapshot = if open {
            Some(self.snapshot_for(state, theme, width, target_h, monitor, done))
        } else {
            // 隐藏中不供内容：回调空转，Visible(false) 生效前的缝隙帧
            // 不会画出一张空卡片。
            None
        };

        if let Some(layer) = overlay {
            if open {
                if !self.shown {
                    // 露面沿：先落静止位 + 播淡入（下一帧起避让/高度动画由闭包覆写）。
                    *self.layer_rect.lock().unwrap() = [home.x, home.y, width, target_h];
                    *self.refade_since.lock().unwrap() = Some(Instant::now());
                }
                let snap = snapshot.expect("open 必有快照");

                let refade = Arc::clone(&self.refade_since);
                let rect_slot = Arc::clone(&self.layer_rect);
                let fadeout = Arc::clone(&self.fadeout_since);
                let measure = Arc::clone(&self.content_h);
                let card = neo_overlay::Card {
                    rect: Arc::clone(&self.layer_rect),

                    draw: Box::new(move |ui| {
                        let ctx = ui.ctx().clone();
                        // 淡入：起点由 tick 写进共享槽，插值在这里现算。
                        let fade_in = {
                            let mut slot = refade.lock().unwrap();
                            match *slot {
                                Some(t0) => {
                                    let k = t0.elapsed().as_secs_f32() / REFADE_SECS;
                                    if k >= 1.0 {
                                        *slot = None;
                                        1.0
                                    } else {
                                        ease_out_cubic(k)
                                    }
                                }
                                None => 1.0,
                            }
                        };
                        // 淡出：驻留结束后透明度渐隐 + 轻轻上飘。
                        let fade_out = fadeout
                            .lock()
                            .unwrap()
                            .map(|t0| (t0.elapsed().as_secs_f32() / FADEOUT_SECS).clamp(0.0, 1.0))
                            .unwrap_or(0.0);
                        let fade = fade_in * (1.0 - ease_out_cubic(fade_out));
                        // 避让：只盯「静止位」判定（卡片移动本身不会让光标
                        // 反复进出，否则会在两个角之间振荡）。
                        let m = snap.theme.metrics;
                        let margin = m.s(16.0);
                        let monitor = snap.monitor;
                        // 高度自适应：朝 tick 按内容测量算出的目标高缓动。
                        let h = ctx.animate_value_with_time(
                            Id::new("neo-miniwin-h"),
                            snap.target_h,
                            HEIGHT_ANIM_SECS,
                        );
                        let size = Vec2::new(snap.width, h);
                        let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
                        let away = Pos2::new(margin, margin);
                        let cursor = poll_global_input().cursor.map(|p| geometry.point(p));
                        let dodge =
                            cursor.is_some_and(|c| Rect::from_min_size(home, size).contains(c));
                        let t = ctx.animate_value_with_time(
                            Id::new("neo-miniwin-avoid"),
                            if dodge { 1.0 } else { 0.0 },
                            AVOID_SECS,
                        );
                        let mut pos = home + (away - home) * ease_out_cubic(t);
                        pos.y -= m.s(10.0) * ease_out_cubic(fade_out);
                        // 写回共享矩形，供层的 Area 下一帧定位。
                        *rect_slot.lock().unwrap() = [pos.x, pos.y, size.x, size.y];
                        paint(ui, &snap, fade, &measure);
                    }),
                };
                layer.set_card(neo_overlay::card_id::MINI, Some(card));
            } else {
                layer.set_card(neo_overlay::card_id::MINI, None);
            }
            self.shown = open;
            // 主视口侧的刷新请求只负责「内容快照」（流式文字 / 工具流水）。
            if open {
                ctx.request_repaint_after(POLL);
            }
            return;
        }

        // ---- 测试回退路径：独立视口（无渲染层时） ----
        //
        //    **每帧无条件注册视口**：egui 在真渲染 pass 结束时会回收本帧没被
        //    `show_viewport_deferred` 触碰的子视口（"never used this pass"）。
        //    曾经「open 才注册」的写法让小窗视口在主窗可见的第二帧就被销毁，
        //    此后托盘里的显隐命令全部打空 —— 这就是「小窗只在主窗显示时出现过、
        //    主窗一藏反而不来」的根因。每帧注册还带来自愈：即便极端时序下被
        //    回收，下一个真 pass 也会按 builder 原样重建窗口。
        //
        //    显隐不切 `Visible`（恒 true）：露面 = 恢复尺寸 + 挪回屏幕内，
        //    休眠 = 缩 1x1 + 挪去 OFFSCREEN；透明视口的「隐藏→可见」首帧会
        //    闪黑（surface 空），尺寸/位置切换不经过那一帧。显式命令兜底
        //    logic-only（patch 不跑），与 builder patch 双通道幂等。小窗一旦
        //    露面，`is_viewport_or_descendant_visible` 反向强制主窗跑真 pass，
        //    内容快照的增量更新随之恢复（但被托盘节流到 10fps，只够刷文字）。
        if open != self.shown {
            ctx.data_mut(|d| {
                d.remove::<FallbackGeometry>(Id::new("neo-miniwin-pos"));
            });
            if open {
                // 先落位再恢复尺寸，免得在屏幕内从 1x1 长大被瞥见；
                // 每次露面都重新提到顶层，盖过同屏后到的其它 topmost 窗口。
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(home));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
                // 露面淡入（与层内路径同通道）。
                *self.refade_since.lock().unwrap() = Some(Instant::now());
                self.legacy_sent = size;
            } else {
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)),
                );
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
                self.legacy_sent = Vec2::ZERO;
            }
            self.shown = open;
        } else if open {
            // 高度自适应（测试回退路径不播动画，量到多少直接贴）。
            if (size - self.legacy_sent).length() > 1.0 {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                self.legacy_sent = size;
            }
        }
        let refade = Arc::clone(&self.refade_since);
        let fadeout = Arc::clone(&self.fadeout_since);
        let measure = Arc::clone(&self.content_h);
        let suspended = crate::app::desktop_viewport(ctx, viewport_id());
        ctx.show_viewport_deferred(
            viewport_id(),
            Self::builder(size, open).with_visible(!suspended),
            move |ui, _class| {
                if crate::app::desktop_suspended(ui.ctx()) {
                    return;
                }
                let Some(snapshot) = &snapshot else { return };
                let ctx = ui.ctx().clone();
                // ---- 回调自驱的动画（主视口托盘态被 eframe 节流到 10fps，
                // 这些量若靠快照下发全会卡；本视口可见、不被节流）----
                // 淡入：起点由 tick 写进共享槽，插值在这里现算。
                let fade_in = {
                    let mut slot = refade.lock().unwrap();
                    match *slot {
                        Some(t0) => {
                            let k = t0.elapsed().as_secs_f32() / REFADE_SECS;
                            if k >= 1.0 {
                                *slot = None;
                                1.0
                            } else {
                                ease_out_cubic(k)
                            }
                        }
                        None => 1.0,
                    }
                };
                // 淡出：驻留结束后透明度渐隐 + 轻轻上飘。
                let fade_out = fadeout
                    .lock()
                    .unwrap()
                    .map(|t0| (t0.elapsed().as_secs_f32() / FADEOUT_SECS).clamp(0.0, 1.0))
                    .unwrap_or(0.0);
                let fade = fade_in * (1.0 - ease_out_cubic(fade_out));
                // 避让：只盯「静止位」判定（窗口移动本身不会让光标反复进出，
                // 否则会在两个角之间振荡）。
                let m = snapshot.theme.metrics;
                let size = Vec2::new(snapshot.width, snapshot.target_h);
                let margin = m.s(16.0);
                let geometry = ScreenGeometry {
                    monitor: ctx
                        .input(|i| i.viewport().monitor_size)
                        .unwrap_or(snapshot.monitor),
                    ppp: ctx.pixels_per_point(),
                };
                let home = Pos2::new((geometry.monitor.x - size.x - margin).max(margin), margin);
                let away = Pos2::new(margin, margin);
                let cursor = poll_global_input().cursor.map(|p| geometry.point(p));
                let dodge = cursor.is_some_and(|c| Rect::from_min_size(home, size).contains(c));
                let t = ctx.animate_value_with_time(
                    Id::new("neo-miniwin-avoid"),
                    if dodge { 1.0 } else { 0.0 },
                    AVOID_SECS,
                );
                let mut pos = home + (away - home) * ease_out_cubic(t);
                pos.y -= m.s(10.0) * ease_out_cubic(fade_out);
                FallbackGeometry::new(geometry, pos).store(&ctx);
                paint(ui, snapshot, fade, &measure);
                // 本视口自驱 60fps：流光 / 避让 / 淡入淡出全靠它。
                ctx.request_repaint_after(POLL);
            },
        );

        // 7. 主视口侧的刷新请求只负责「内容快照」（流式文字 / 工具流水）。
        //    托盘态下它被 eframe 节流到 ~10fps —— 刷文字够用；动画不指望它。
        if open {
            ctx.request_repaint_after(POLL);
        }
    }
}

#[cfg(test)]
#[path = "miniwin_beam_tests.rs"]
mod beam_tests;

#[cfg(test)]
#[path = "miniwin_layout_tests.rs"]
mod layout_tests;

// ---------------------------------------------------------------------------
// 截屏闪光
// ---------------------------------------------------------------------------

/// 最早起闪时间；它不是抓帧完成的证明。桌面租约未释放时必须保留排程，
/// 等屏障解除后才开始动画计时，不能在隐藏期间把整段闪光耗尽。
const FLASH_DELAY: Duration = Duration::from_millis(450);
/// 闪光总时长（前 13% 快速淡入，之后缓出）。
const FLASH_SECS: f32 = 0.45;

fn flash_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-shotflash")
}

/// 回调与排程共享时钟；生产逐帧读单调时钟，测试可无 sleep 地推进 deferred 绘制。
#[derive(Clone, Default)]
struct FlashClock {
    #[cfg(test)]
    time: Arc<Mutex<Option<Instant>>>,
}

impl FlashClock {
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = *self.time.lock().unwrap() {
            return now;
        }
        Instant::now()
    }

    #[cfg(test)]
    fn set(&self, now: Instant) {
        *self.time.lock().unwrap() = Some(now);
    }
}

/// 截屏闪光：AI 抓屏后，在被抓区域的边缘闪一道白框（整屏截图 = 全屏边框）。
///
/// 视口是一整块盖满虚拟屏的透明窗，平时隐藏；机制与迷你窗相同：
/// 每帧注册防回收，显隐走 builder patch + 显式 `Visible` 命令双通道。
/// 与迷你窗的差别：主窗可见时也闪 —— 用户在主窗里发消息，AI 截了屏，
/// 同样该有个看得见的反馈。
#[derive(Default)]
pub struct ShotFlash {
    /// 已见到的截屏时间戳（变了 = AI 又截了一帧）。
    shot_seen: u64,
    /// 本次截图的区域（虚拟屏物理像素），信号变化那一刻锁定。
    region: Option<neo_tools::tools::screen::Rect>,
    /// 闪光排程到的时刻（None = 无排程）。
    flash_at: Option<Instant>,
    /// 闪光起点（Some = 正在闪）。
    flashing_since: Option<Instant>,
    /// 上一帧是否在闪（沿检测用）。
    shown: bool,
    using_overlay: bool,
    /// 作废挂起前/上一张截图的 deferred 回调，恢复后也不能重放旧帧。
    frame_generation: Arc<std::sync::atomic::AtomicU64>,
    clock: FlashClock,
}

impl ShotFlash {
    /// 每帧驱动一次，挂在 `NeoApp::tick` 里（与迷你窗同路）。
    pub fn tick(&mut self, ctx: &Context, overlay: Option<&neo_overlay::OverlayHandle>) {
        use neo_tools::tools::screen;

        let shot_at = screen::SCREENSHOT_AT.load(Ordering::Relaxed);
        let region = if shot_at != self.shot_seen && shot_at != 0 {
            screen::SCREENSHOT_RECT.lock().ok().and_then(|slot| *slot)
        } else {
            self.region
        };
        self.tick_at(ctx, overlay, Instant::now(), (shot_at, region));
    }

    fn advance(
        &mut self,
        now: Instant,
        shot: (u64, Option<neo_tools::tools::screen::Rect>),
        suspended: bool,
    ) -> bool {
        if shot.0 != self.shot_seen {
            self.shot_seen = shot.0;
            if shot.0 != 0 {
                self.region = shot.1;
                self.flash_at = Some(now + FLASH_DELAY);
                self.flashing_since = None;
                self.frame_generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        // 上一帧可能还没清理已到期动画；不能把它误当成被新租约打断的反馈。
        if self
            .flashing_since
            .is_some_and(|t0| now.duration_since(t0).as_secs_f32() >= FLASH_SECS)
        {
            self.flashing_since = None;
            self.frame_generation.fetch_add(1, Ordering::AcqRel);
        }
        if suspended {
            if self.flashing_since.take().is_some() {
                self.flash_at.get_or_insert(now);
                self.frame_generation.fetch_add(1, Ordering::AcqRel);
            }
            return false;
        }
        if self.flash_at.is_some_and(|at| now >= at) {
            self.flash_at = None;
            self.flashing_since = Some(now);
        }
        self.flashing_since.is_some()
    }

    fn card(&self, ctx: &Context, rect: [f32; 4], t0: Instant) -> neo_overlay::Card {
        let ctx = ctx.clone();
        let generation = self.frame_generation.clone();
        let expected = generation.load(Ordering::Acquire);
        let clock = self.clock.clone();
        neo_overlay::Card::passive(rect, move |ui| {
            if crate::app::desktop_suspended(&ctx) || generation.load(Ordering::Acquire) != expected
            {
                return;
            }
            paint_flash_at(ui, Some((ui.max_rect(), t0)), clock.now());
        })
    }

    /// 与生产 tick 共用的时钟/信号入口，回归不写全局信号、不抓屏或启动原生层。
    pub(crate) fn tick_at(
        &mut self,
        ctx: &Context,
        overlay: Option<&neo_overlay::OverlayHandle>,
        now: Instant,
        shot: (u64, Option<neo_tools::tools::screen::Rect>),
    ) {
        #[cfg(test)]
        self.clock.set(now);
        let overlay = overlay.filter(|layer| layer.is_alive());
        if self.using_overlay != overlay.is_some() {
            self.using_overlay = overlay.is_some();
            self.shown = false;
            self.frame_generation.fetch_add(1, Ordering::AcqRel);
            if self.using_overlay {
                ctx.send_viewport_cmd_to(flash_viewport_id(), ViewportCommand::Close);
            }
        }
        let geometry = screen_geometry(ctx, overlay.is_some());
        // 两个后端共用排程；屏障解除表示抓屏 dispatch 已退出，不靠固定延时猜完成。
        let on = self.advance(now, shot, crate::app::desktop_suspended(ctx));

        // 2. 物理矩形只换算一次；overlay 使用全局点，fallback 绘制时才减原点。
        let vs = neo_tools::tools::screen::virtual_screen();
        let desktop = geometry.rect(vs, Pos2::ZERO);
        let vs_size = desktop.size().max(Vec2::splat(1.0));
        let origin = desktop.min;

        // 3a. 渲染层：闪光是一张 passive 卡（只画不收点击），区域即卡矩形。
        if let Some(layer) = overlay {
            if on {
                let region = self.region.unwrap_or(vs);
                let rect = geometry.rect(region, Pos2::ZERO);
                let rect = [rect.min.x, rect.min.y, rect.width(), rect.height()];
                let t0 = self.flashing_since.unwrap_or_else(Instant::now);
                let card = self.card(ctx, rect, t0);
                layer.set_card(neo_overlay::card_id::FLASH, Some(card));
            } else {
                layer.set_card(neo_overlay::card_id::FLASH, None);
            }
            self.shown = on;
            // 闪光与排程期间保持帧率（起燃时刻靠它）。
            if on || self.flash_at.is_some() {
                ctx.request_repaint_after(POLL);
            }
            return;
        }

        // ---- 测试回退路径：独立视口（无渲染层时） ----
        // 3b. 显隐沿：不切 `Visible`（恒 true）——全屏透明视口的「隐藏→可见」
        //    首帧 surface 无内容，DWM 补一帧全屏黑，正是「截屏时屏幕闪黑」。
        //    起闪 = 恢复全屏尺寸 + 挪到虚拟屏原点；熄闪 = 缩 1x1 回 OFFSCREEN。
        if on != self.shown {
            if on {
                ctx.send_viewport_cmd_to(
                    flash_viewport_id(),
                    ViewportCommand::OuterPosition(origin),
                );
                ctx.send_viewport_cmd_to(flash_viewport_id(), ViewportCommand::InnerSize(vs_size));
                ctx.send_viewport_cmd_to(
                    flash_viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
            } else {
                ctx.send_viewport_cmd_to(
                    flash_viewport_id(),
                    ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)),
                );
                ctx.send_viewport_cmd_to(
                    flash_viewport_id(),
                    ViewportCommand::OuterPosition(OFFSCREEN),
                );
            }
            self.shown = on;
        }

        // 4. 每帧注册（防回收 + 刷新动画帧）。区域换算成视口本地逻辑坐标。
        let frame = if on {
            let region = self.region.unwrap_or(vs);
            Some((
                geometry.rect(region, Pos2::new(vs.x as f32, vs.y as f32)),
                self.flashing_since.unwrap_or_else(Instant::now),
            ))
        } else {
            None
        };
        let suspended = crate::app::desktop_viewport(ctx, flash_viewport_id());
        let generation = self.frame_generation.clone();
        let expected = generation.load(Ordering::Acquire);
        let clock = self.clock.clone();
        ctx.show_viewport_deferred(
            flash_viewport_id(),
            ViewportBuilder::default()
                .with_title("Neo")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                .with_active(false)
                .with_transparent(true)
                .with_mouse_passthrough(true)
                // 恒可见（见 OFFSCREEN 注释）：熄闪态是 1x1 屏幕外。
                .with_visible(!suspended)
                .with_inner_size(if on { vs_size } else { Vec2::new(1.0, 1.0) })
                .with_position(if on { origin } else { OFFSCREEN }),
            move |ui, _class| {
                if crate::app::desktop_suspended(ui.ctx())
                    || frame.is_none()
                    || generation.load(Ordering::Acquire) != expected
                {
                    return;
                }
                paint_flash_at(ui, frame, clock.now());
            },
        );

        // 5. 闪光与排程期间保持帧率（起燃时刻与动画都靠它）。
        if on || self.flash_at.is_some() {
            ctx.request_repaint_after(POLL);
        }
    }
}

/// 画一帧闪光：区域边缘一道白框 + 极淡的白色填充，快速淡入后缓出。
fn paint_flash_at(ui: &mut egui::Ui, frame: Option<(Rect, Instant)>, now: Instant) {
    let Some((rect, t0)) = frame else { return };
    let elapsed = now.saturating_duration_since(t0).as_secs_f32();
    if elapsed >= FLASH_SECS {
        return;
    }
    // 子视口自行拉帧；到期即停止绘制/重绘，不依赖可能被托盘节流的 ROOT tick。
    ui.ctx().request_repaint_after(POLL);
    let k = (elapsed / FLASH_SECS).clamp(0.0, 1.0);
    let alpha = if k < 0.13 {
        k / 0.13
    } else {
        ease_out_cubic(1.0 - (k - 0.13) / 0.87)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::WHITE.gamma_multiply(alpha * 0.08));
    painter.rect_stroke(
        rect.shrink(1.0),
        0.0,
        Stroke::new(2.0, Color32::WHITE.gamma_multiply(alpha * 0.85)),
        egui::StrokeKind::Inside,
    );
}
