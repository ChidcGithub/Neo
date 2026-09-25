//! 迷你窗：AI 后台执行期间的屏幕角落小窗。
//!
//! - 主窗藏到托盘且 AI 正在生成 / 执行工具时，贴在屏幕右上角；
//! - AI 一截屏就暂时消失（`neo_tools::tools::screen::SCREENSHOT_AT` 置位），
//!   截完带着一个轻微的淡入回来 —— 截图里永远不会有 Neo 自己的窗；
//! - AI 的鼠标移进小窗区域时，小窗非线性躲到左上角，鼠标离开后弹回；
//! - 本轮 AI 用过鼠标/键盘工具时，用户点击屏幕任意处弹出「打断确认」，
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

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, Context, Id, Pos2, Rect, Stroke, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId, WindowLevel,
};
use neo_theme::{SquirclePaint, Theme};
use neo_ui::modal::Confirm;
use neo_ui::Design;

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
/// 工具流水最多保留的步数（旧的滚出，最近的排最下）。
const MAX_STEPS: usize = 4;
/// 休眠位：屏幕外远处。本文件的所有视口都**不切 `Visible`**——透明视口
/// 从隐藏切回可见的首帧还没跑过回调、surface 无内容，DWM 会补一帧黑，
/// 就是「小窗弹出 / 截屏闪光时屏幕闪一下黑」的来源。改为常态可见 +
/// 休眠时缩 1x1 挪到这里：surface 自创建起一直有（透明）内容，露面即正片。
pub(crate) const OFFSCREEN: Pos2 = Pos2::new(-16000.0, -16000.0);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-miniwin")
}

/// 缓出三次方：避让与淡入共用这条「先快后慢」的曲线。
fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// 全局鼠标状态：光标位置（屏幕物理像素）+ 左键此刻是否按下。
/// 主窗隐藏后 egui 拿不到任何输入，只能绕到系统 API 轮询。
#[cfg(windows)]
fn global_cursor() -> Option<(Pos2, bool)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut point = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut point) == 0 {
            return None;
        }
        let lmb = (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0;
        Some((Pos2::new(point.x as f32, point.y as f32), lmb))
    }
}

/// 非 Windows 没有全局输入轮询（小窗退化为只读状态牌）。
#[cfg(not(windows))]
fn global_cursor() -> Option<(Pos2, bool)> {
    None
}

/// 本轮对话（自最后一条用户消息起）AI 是否动过鼠标/键盘。
fn used_mouse_or_keyboard(state: &AppState) -> bool {
    for msg in state.messages.iter().rev() {
        if msg.role == Role::User {
            break;
        }
        if let Some(tool) = &msg.tool {
            if matches!(tool.name.as_str(), "click" | "drag") {
                return true;
            }
        }
    }
    false
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
struct Step {
    line: String,
    tone: StepTone,
}

/// 一帧要画的内容。视口回调是 `'static` 的，数据必须整份搬走。
///
/// 注意**没有** fade 与位置：主视口藏托盘后被 eframe 节流到 10fps，
/// 这两个动画量改由回调自驱（fade 走共享的 `refade_since`，位置走
/// 回调里现算现发的 `OuterPosition`）。
struct Snapshot {
    theme: Theme,
    /// 卡片尺寸（回调里的避让判定要用）。
    size: Vec2,
    interrupt_open: bool,
    title: String,
    steps: Vec<Step>,
    body: String,
    /// 正文是思考过程（或占位）时的淡色标记：content 还空着时 reasoning 顶上来。
    body_dim: bool,
    streaming: bool,
}

impl Snapshot {
    fn build(state: &AppState, theme: Theme, size: Vec2, interrupt_open: bool, busy_secs: u64) -> Self {
        let last_assistant = state
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant);
        let streaming = last_assistant.is_some_and(|msg| msg.streaming);
        // 正文优先 content；思考阶段 content 还空着时把 reasoning 顶上来 ——
        // 小窗只有一块正文区，「正在想什么」就是此刻的正文，不再是干等的
        // 「正在处理…」。
        let (body_src, body_dim) = match last_assistant {
            Some(msg) => {
                let content = msg.content.trim();
                let reasoning = msg.reasoning.trim();
                if !content.is_empty() {
                    (content, false)
                } else if !reasoning.is_empty() {
                    (reasoning, true)
                } else {
                    ("正在处理…", true)
                }
            }
            None => ("正在处理…", true),
        };
        // 正文最多三行：按 CJK 最宽情形（一字符 ≈ 一个字号）估算每行字数，留尾巴。
        let m = theme.metrics;
        let inner_w = m.s(340.0 - 28.0);
        let body_px = theme.typo.body.max(1.0);
        let per_line = (inner_w / body_px).floor().max(6.0) as usize;
        // 本轮工具流水（自最后一条用户消息起）：最近 MAX_STEPS 步，旧的在上。
        // 倒序收集到量即停，再翻回正序 —— 老消息成堆时不全扫。
        let step_px = theme.typo.caption.max(1.0);
        let step_chars =
            ((inner_w - m.s(12.0)) / step_px).floor().max(6.0) as usize;
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
                    line: head(&tool.line(), step_chars),
                    tone,
                });
                if steps.len() >= MAX_STEPS {
                    break;
                }
            }
        }
        steps.reverse();
        // 标题带已用时长：后台跑久了，一眼知道这轮已经花了多久。
        let elapsed = if busy_secs >= 60 {
            format!("{}m{:02}s", busy_secs / 60, busy_secs % 60)
        } else {
            format!("{busy_secs}s")
        };
        Self {
            theme,
            size,
            interrupt_open,
            title: format!("Neo 执行中 · {elapsed}"),
            steps,
            body: tail(body_src, per_line * 3),
            body_dim,
            streaming,
        }
    }
}

/// 画一帧小窗内容（纯 painter 自绘：这里拿不到 `&WhaleMark`，也用不上）。
fn paint(ui: &mut egui::Ui, snap: &Snapshot, result: &Arc<AtomicU8>, fade: f32) {
    let d = Design::new(snap.theme);
    let p = d.p();
    let m = d.m();
    let rect = ui.ctx().content_rect();
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

    // 打断确认：模态居中在小窗自己的视口里，遮住下方内容。
    if snap.interrupt_open {
        let answer = Confirm::new("打断执行？", "AI 本轮正在操作鼠标 / 键盘，确认打断吗？")
            .danger(true)
            .labels("打断", "继续")
            .show(ui, &d, rect.width() - m.s(32.0));
        if let Some(yes) = answer {
            result.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
        }
        return;
    }

    let inner = rect.shrink(m.s(14.0));
    let mut y = inner.top();
    // 标题行：accent 小圆点 + 标题，下面压一条细分隔线。
    let title_r = m.s(3.5);
    painter.circle_filled(
        Pos2::new(inner.left() + title_r, y + d.t().caption * 0.8),
        title_r,
        tint(p.accent),
    );
    painter.text(
        Pos2::new(inner.left() + m.s(11.0), y),
        Align2::LEFT_TOP,
        &snap.title,
        d.font_bold(d.t().caption),
        tint(p.label_tertiary),
    );
    y += d.t().caption * 1.6 + m.s(4.0);
    painter.line_segment(
        [Pos2::new(inner.left(), y), Pos2::new(inner.right(), y)],
        Stroke::new(1.0, tint(p.border_l1)),
    );
    y += m.s(6.0);

    // 工具流水：状态圆点 + 单行摘要，最近的在最下。
    let step_lh = d.t().caption * 1.55;
    let dot_r = m.s(3.0);
    let text_x = inner.left() + m.s(12.0);
    for step in &snap.steps {
        let cy = y + step_lh * 0.5;
        let dot = match step.tone {
            StepTone::Active => p.accent,
            StepTone::Ok => p.success,
            StepTone::Failed => p.error,
            StepTone::Muted => p.label_caption,
        };
        painter.circle_filled(Pos2::new(inner.left() + dot_r, cy), dot_r, tint(dot));
        let ink = match step.tone {
            StepTone::Active => p.label_primary,
            _ => p.label_secondary,
        };
        painter.text(
            Pos2::new(text_x, y),
            Align2::LEFT_TOP,
            &step.line,
            d.font(d.t().caption),
            tint(ink),
        );
        y += step_lh;
    }
    if !snap.steps.is_empty() {
        y += m.s(4.0);
    }

    let mut body = snap.body.clone();
    if snap.streaming {
        body.push('▍');
    }
    // 思考过程用淡色：与正式回答拉开层次，也暗示「还没落定」。
    let ink = if snap.body_dim {
        p.label_secondary
    } else {
        p.label_primary
    };
    let galley = painter.layout(body, d.font(d.t().body), tint(ink), inner.width());
    painter.galley(Pos2::new(inner.left(), y), galley, tint(ink));

    // 边缘流光：小窗只在后台执行中露面，这道光就是「还在跑」的持续信号。
    // 截屏淡出时随卡片一起淡（tint 已乘 fade）。
    let now = ui.ctx().input(|i| i.time);
    paint_border_beam(&painter, rect, m.s(18.0), now, tint(p.accent), m.s(1.4));
}

/// 边缘流光：一段细线沿卡片边缘顺时针匀速循环，头部实、拖尾渐隐。
///
/// 沿弧长（而非点序号）参数化，圆角与直边上的速度才一致。
fn paint_border_beam(
    painter: &egui::Painter,
    rect: Rect,
    radius: f32,
    now: f64,
    base: Color32,
    width: f32,
) {
    /// 一圈的秒数。
    const PERIOD: f64 = 2.4;
    /// 拖尾长度占周长的比例。
    const TAIL: f32 = 0.30;
    /// 拖尾分多少段渐隐。
    const SLICES: usize = 28;

    // shrink(0.5)：对齐静态描边（1px Inside）的中心线。
    let pts = neo_theme::squircle::squircle_points(
        rect.shrink(0.5),
        radius,
        neo_theme::HARNESS_SUPERELLIPSE,
        neo_theme::squircle::DEFAULT_SEGMENTS,
    );
    if pts.len() < 2 {
        return;
    }
    // 闭合路径的累计弧长（含末点绕回首点的一段）。
    let mut cum = Vec::with_capacity(pts.len() + 1);
    cum.push(0.0f32);
    for i in 0..pts.len() {
        cum.push(cum[i] + pts[i].distance(pts[(i + 1) % pts.len()]));
    }
    let total = *cum.last().unwrap();
    if total <= 0.0 {
        return;
    }
    // 弧长 s 处的点：先二分定位采样区间，再在区间内线性插值。
    let at = |s: f32| -> Pos2 {
        let s = s.rem_euclid(total);
        let idx = match cum.binary_search_by(|c| c.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Equal))
        {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
        .min(pts.len() - 1);
        let seg = (cum[idx + 1] - cum[idx]).max(f32::EPSILON);
        let t = ((s - cum[idx]) / seg).clamp(0.0, 1.0);
        pts[idx] + (pts[(idx + 1) % pts.len()] - pts[idx]) * t
    };
    let head = ((now / PERIOD).fract() as f32) * total;
    let tail_len = total * TAIL;
    for i in 0..SLICES {
        // i = 0 是最尾端（最透明），越靠头越亮；平方让尾部更快隐没。
        let s1 = head - tail_len + tail_len * (i as f32 / SLICES as f32);
        let s2 = head - tail_len + tail_len * ((i + 1) as f32 / SLICES as f32);
        let k = (i + 1) as f32 / SLICES as f32;
        painter.line_segment(
            [at(s1), at(s2)],
            Stroke::new(width, base.gamma_multiply(k * k)),
        );
    }
}

/// 迷你窗运行时状态，挂在 `NeoApp` 上，每帧由 [`MiniWin::tick`] 驱动。
#[derive(Default)]
pub struct MiniWin {
    /// 上一帧是否已发 `Visible(true)`（显隐改由显式命令驱动，沿检测用）。
    shown: bool,
    /// 已见到的截屏时间戳（变了 = AI 又截了一次屏）。
    shot_seen: u64,
    /// 截屏隐藏的截止时刻。
    shot_hide_until: Option<Instant>,
    /// 截屏后重新露面的淡入起点。**与视口回调共享**：主窗藏托盘后主视口的
    /// repaint 被 eframe 节流到 10fps（`INVISIBLE_WINDOW_REPAINT_INTERVAL`），
    /// 250ms 的淡入若靠快照下发只剩两三帧 —— 改由回调里现取现算。
    refade_since: Arc<Mutex<Option<Instant>>>,
    /// 打断确认弹窗是否打开（画在小窗自己的视口里）。
    interrupt_open: bool,
    /// 打断确认结果（回调里写、tick 里读）：0 未决 / 1 打断 / 2 继续。
    interrupt_result: Arc<AtomicU8>,
    /// 上一帧全局左键状态（沿检测用）。
    lmb_was_down: bool,
    /// 本轮后台执行的起点（标题里的已用时长靠它）；闲下来清零。
    busy_since: Option<Instant>,
}

impl MiniWin {
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
            .with_visible(true)
            .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
    }

    /// 每帧驱动一次。放在 `tick()` 里而不是 `render()` 里：
    /// 主窗隐藏时渲染循环走 logic-only 路径，两者都经过 `tick()`。
    pub fn tick(&mut self, ctx: &Context, state: &mut AppState, theme: Theme, hidden_to_tray: bool) {
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

        // 2. 显隐目标：主窗已藏到托盘 且 AI 正在生成 / 执行工具。
        let busy = state.generating || state.tool_open || state.tool_round;
        if !busy {
            // 执行结束（或刚被打断）：确认框一并收掉。
            self.interrupt_open = false;
            self.interrupt_result.store(0, Ordering::Relaxed);
            self.busy_since = None;
        }
        let open = hidden_to_tray && busy && !shot_hiding;
        if open && self.busy_since.is_none() {
            self.busy_since = Some(Instant::now());
        }

        // 3. 全局输入：逻辑坐标光标 + 左键按下沿。
        let ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let (cursor, lmb_edge) = match global_cursor() {
            Some((physical, down)) => {
                let edge = down && !self.lmb_was_down;
                self.lmb_was_down = down;
                (Some(Pos2::new(physical.x / ppp, physical.y / ppp)), edge)
            }
            None => {
                self.lmb_was_down = false;
                (None, false)
            }
        };

        // 4. 几何：常态贴右上、避让时躲左上。窗口位置的动画挪到了视口回调里
        //    自驱（主视口托盘态被节流，跑不动动画）；这里只留两个静止端点，
        //    供打断判定「点在小窗自己身上不算」用 —— 两端都算上，覆盖移动途中。
        let m = theme.metrics;
        let size = if self.interrupt_open {
            Vec2::new(m.s(380.0), m.s(280.0))
        } else {
            Vec2::new(m.s(340.0), m.s(236.0))
        };
        let margin = m.s(16.0);
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
        let away = Pos2::new(margin, margin);

        // 5. 打断确认：后台执行中 + 本轮动过鼠标/键盘 + 左键按下沿。
        //    点在小窗自己身上不算（那是在点弹窗按钮）；有待确认的工具也不算 ——
        //    那一击多半是点在独立确认窗的按钮上，且等待权限时 AI 本就停着。
        if lmb_edge
            && open
            && !self.interrupt_open
            && state.awaiting_tool().is_none()
            && used_mouse_or_keyboard(state)
        {
            let on_miniwin = cursor.is_some_and(|c| {
                Rect::from_min_size(home, size).contains(c)
                    || Rect::from_min_size(away, size).contains(c)
            });
            if !on_miniwin {
                self.interrupt_open = true;
                self.interrupt_result.store(0, Ordering::Relaxed);
            }
        }
        match self.interrupt_result.swap(0, Ordering::Relaxed) {
            1 => {
                state.cancel();
                self.interrupt_open = false;
            }
            2 => self.interrupt_open = false,
            _ => {}
        }

        // 6. 显隐与内容。
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
            if open {
                // 先落位再恢复尺寸，免得在屏幕内从 1x1 长大被瞥见；
                // 每次露面都重新提到顶层，盖过同屏后到的其它 topmost 窗口。
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(home));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
            } else {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
            }
            self.shown = open;
        }
        let snapshot = if open {
            let busy_secs = self
                .busy_since
                .map(|t0| t0.elapsed().as_secs())
                .unwrap_or(0);
            Some(Snapshot::build(state, theme, size, self.interrupt_open, busy_secs))
        } else {
            // 隐藏中不供内容：回调空转，Visible(false) 生效前的缝隙帧
            // 不会画出一张空卡片。
            None
        };
        let result = Arc::clone(&self.interrupt_result);
        let refade = Arc::clone(&self.refade_since);
        ctx.show_viewport_deferred(
            viewport_id(),
            Self::builder(size, open),
            move |ui, _class| {
                let Some(snapshot) = &snapshot else { return };
                let ctx = ui.ctx().clone();
                // ---- 回调自驱的动画（主视口托盘态被 eframe 节流到 10fps，
                // 这些量若靠快照下发全会卡；本视口可见、不被节流）----
                // 淡入：起点由 tick 写进共享槽，插值在这里现算。
                let fade = {
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
                // 避让：只盯「静止位」判定（窗口移动本身不会让光标反复进出，
                // 否则会在两个角之间振荡）；弹窗打开时不躲（用户正要去点按钮）。
                let m = snapshot.theme.metrics;
                let size = snapshot.size;
                let margin = m.s(16.0);
                let monitor = ctx
                    .input(|i| i.viewport().monitor_size)
                    .unwrap_or(Vec2::new(1920.0, 1080.0));
                let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
                let away = Pos2::new(margin, margin);
                let ppp = ctx
                    .input(|i| i.viewport().native_pixels_per_point)
                    .unwrap_or(1.0);
                let cursor =
                    global_cursor().map(|(p, _)| Pos2::new(p.x / ppp, p.y / ppp));
                let dodge = !snapshot.interrupt_open
                    && cursor.is_some_and(|c| Rect::from_min_size(home, size).contains(c));
                let t = ctx.animate_value_with_time(
                    Id::new("neo-miniwin-avoid"),
                    if dodge { 1.0 } else { 0.0 },
                    AVOID_SECS,
                );
                let pos = home + (away - home) * ease_out_cubic(t);
                // 位置没变就不发：每条命令都是一次 SetWindowPos + DWM 重合成，
                // 静止时每帧白调是纯开销。
                let pos_id = Id::new("neo-miniwin-pos");
                let last = ctx.data_mut(|d| d.get_temp::<Pos2>(pos_id));
                if last != Some(pos) {
                    ctx.data_mut(|d| d.insert_temp(pos_id, pos));
                    ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(pos));
                }
                paint(ui, &snapshot, &result, fade);
                // 本视口自驱 60fps：流光 / 避让 / 淡入全靠它。
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

// ---------------------------------------------------------------------------
// 截屏闪光
// ---------------------------------------------------------------------------

/// 闪光排程：截屏信号置位后这么久才亮 —— `capture()` 内置 300ms 等待 +
/// 抓帧耗时，等闪光出现时画面早已抓完，AI 看到的截图里不会有这道白。
const FLASH_DELAY: Duration = Duration::from_millis(450);
/// 闪光总时长（前 13% 快速淡入，之后缓出）。
const FLASH_SECS: f32 = 0.45;

fn flash_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-shotflash")
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
}

impl ShotFlash {
    /// 每帧驱动一次，挂在 `NeoApp::tick` 里（与迷你窗同路）。
    pub fn tick(&mut self, ctx: &Context) {
        use neo_tools::tools::screen;

        // 1. 截屏信号：时间戳一变就锁定区域并排程闪光。
        let shot_at = screen::SCREENSHOT_AT.load(Ordering::Relaxed);
        if shot_at != self.shot_seen {
            self.shot_seen = shot_at;
            if shot_at != 0 {
                self.region = screen::SCREENSHOT_RECT.lock().ok().and_then(|slot| *slot);
                self.flash_at = Some(Instant::now() + FLASH_DELAY);
            }
        }
        if self.flash_at.is_some_and(|at| Instant::now() >= at) {
            self.flash_at = None;
            self.flashing_since = Some(Instant::now());
        }
        if self
            .flashing_since
            .is_some_and(|t0| t0.elapsed().as_secs_f32() >= FLASH_SECS)
        {
            self.flashing_since = None;
        }
        let on = self.flashing_since.is_some();

        // 2. 视口几何：盖满整个虚拟屏（多屏时原点可为负）。
        let ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let vs = screen::virtual_screen();
        let vs_size = Vec2::new(
            (vs.width as f32 / ppp).max(1.0),
            (vs.height as f32 / ppp).max(1.0),
        );
        let origin = Pos2::new(vs.x as f32 / ppp, vs.y as f32 / ppp);

        // 3. 显隐沿：不切 `Visible`（恒 true）——全屏透明视口的「隐藏→可见」
        //    首帧 surface 无内容，DWM 补一帧全屏黑，正是「截屏时屏幕闪黑」。
        //    起闪 = 恢复全屏尺寸 + 挪到虚拟屏原点；熄闪 = 缩 1x1 回 OFFSCREEN。
        if on != self.shown {
            if on {
                ctx.send_viewport_cmd_to(flash_viewport_id(), ViewportCommand::OuterPosition(origin));
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
                ctx.send_viewport_cmd_to(flash_viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
            }
            self.shown = on;
        }

        // 4. 每帧注册（防回收 + 刷新动画帧）。区域换算成视口本地逻辑坐标。
        let frame = if on {
            let region = self.region.unwrap_or(vs);
            Some((
                Rect::from_min_size(
                    Pos2::new(
                        (region.x - vs.x) as f32 / ppp,
                        (region.y - vs.y) as f32 / ppp,
                    ),
                    Vec2::new(
                        region.width as f32 / ppp,
                        region.height as f32 / ppp,
                    ),
                ),
                self.flashing_since.unwrap_or_else(Instant::now),
            ))
        } else {
            None
        };
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
                .with_visible(true)
                .with_inner_size(if on { vs_size } else { Vec2::new(1.0, 1.0) })
                .with_position(if on { origin } else { OFFSCREEN }),
            move |ui, _class| {
                if frame.is_none() {
                    return;
                }
                paint_flash(ui, frame);
                // 闪光动画由本视口自驱：主视口托盘态被 eframe 节流到 10fps，
                // 指望它拉帧率闪光会卡成慢动作。
                ui.ctx().request_repaint_after(POLL);
            },
        );

        // 5. 闪光与排程期间保持帧率（起燃时刻与动画都靠它）。
        if on || self.flash_at.is_some() {
            ctx.request_repaint_after(POLL);
        }
    }
}

/// 画一帧闪光：区域边缘一道白框 + 极淡的白色填充，快速淡入后缓出。
fn paint_flash(ui: &mut egui::Ui, frame: Option<(Rect, Instant)>) {
    let Some((rect, t0)) = frame else { return };
    let k = (t0.elapsed().as_secs_f32() / FLASH_SECS).clamp(0.0, 1.0);
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
