//! 迷你窗：AI 后台执行期间的屏幕角落小窗。
//!
//! - 主窗藏到托盘且 AI 正在生成 / 执行工具时，贴在屏幕右上角；
//! - AI 一截屏就暂时消失（`neo_tools::tools::screen::SCREENSHOT_AT` 置位），
//!   截完带着一个轻微的淡入回来 —— 截图里永远不会有 Neo 自己的窗；
//! - AI 的鼠标移进小窗区域时，小窗非线性躲到左上角，鼠标离开后弹回；
//! - 本轮 AI 用过鼠标/键盘工具时，用户点击屏幕任意处弹出「打断确认」，
//!   确认即 [`AppState::cancel`]。
//!
//! 实现要点：egui deferred 视口每帧重注册，`with_visible` / `with_position`
//! 的变化会被 `ViewportBuilder::patch` 收成增量命令，所以显隐与移动都只需要
//! 「每帧给出目标值」，不用手动发 viewport 命令。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
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
const SHOT_HIDE: Duration = Duration::from_millis(500);
/// 小窗开着时的轮询节拍（避让 / 截屏信号 / 打断点击都靠它）。
/// 16ms ≈ 60fps：避让、淡入、呼吸点都是动画，再低肉眼能看出顿。
const POLL: Duration = Duration::from_millis(16);
/// 工具流水最多保留的步数（旧的滚出，最近的排最下）。
const MAX_STEPS: usize = 4;

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
struct Snapshot {
    theme: Theme,
    fade: f32,
    interrupt_open: bool,
    title: String,
    steps: Vec<Step>,
    body: String,
    streaming: bool,
}

impl Snapshot {
    fn build(
        state: &AppState,
        theme: Theme,
        fade: f32,
        interrupt_open: bool,
        busy_secs: u64,
    ) -> Self {
        let last_assistant = state
            .messages
            .iter()
            .rev()
            .find(|msg| msg.role == Role::Assistant);
        let streaming = last_assistant.is_some_and(|msg| msg.streaming);
        let body_src = last_assistant
            .map(|msg| msg.content.trim())
            .filter(|text| !text.is_empty())
            .unwrap_or("正在处理…");
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
            fade,
            interrupt_open,
            title: format!("Neo 执行中 · {elapsed}"),
            steps,
            body: tail(body_src, per_line * 3),
            streaming,
        }
    }

    /// 创建视口时的占位快照：`fade = 0` 全透明，即便意外露面也只是一张
    /// 看不见的卡片；真正的内容要等打开后由 [`Snapshot::build`] 逐帧刷新。
    fn placeholder(theme: Theme) -> Self {
        Self {
            theme,
            fade: 0.0,
            interrupt_open: false,
            title: String::new(),
            steps: Vec::new(),
            body: String::new(),
            streaming: false,
        }
    }
}

/// 画一帧小窗内容（纯 painter 自绘：这里拿不到 `&WhaleMark`，也用不上）。
fn paint(ui: &mut egui::Ui, snap: &Snapshot, result: &Arc<AtomicU8>) {
    let d = Design::new(snap.theme);
    let p = d.p();
    let m = d.m();
    let rect = ui.ctx().content_rect();
    let painter = ui.painter().clone();
    let fade = snap.fade;
    let tint = |c: Color32| c.gamma_multiply(fade);

    // 卡片：视口本身是主题底色矩形，squircle 卡片盖在上面。
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
    painter.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        &snap.title,
        d.font_bold(d.t().caption),
        tint(p.label_tertiary),
    );
    y += d.t().caption * 1.6 + m.s(4.0);

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
    let galley = painter.layout(
        body,
        d.font(d.t().body),
        tint(p.label_primary),
        inner.width(),
    );
    painter.galley(Pos2::new(inner.left(), y), galley, tint(p.label_primary));

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
    /// 小窗视口是否已趁主窗可见时建出来（进 viewports 表）。
    /// deferred 视口的创建命令只在真渲染 pass 里派发，主窗藏到托盘后走
    /// logic-only 路径、不再建窗，所以必须在还能跑 pass 时先建好。
    created: bool,
    /// 上一帧是否已发 `Visible(true)`（显隐改由显式命令驱动，沿检测用）。
    shown: bool,
    /// 已见到的截屏时间戳（变了 = AI 又截了一次屏）。
    shot_seen: u64,
    /// 截屏隐藏的截止时刻。
    shot_hide_until: Option<Instant>,
    /// 截屏后重新露面的淡入起点。
    refade_since: Option<Instant>,
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
    /// 视口 builder：显隐、尺寸、位置都给目标值，patch 自己收成增量命令。
    fn builder(size: Vec2, pos: Pos2, visible: bool) -> ViewportBuilder {
        ViewportBuilder::default()
            .with_title("Neo")
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_active(false)
            .with_visible(visible)
            .with_inner_size(size)
            .with_position(pos)
    }

    /// 趁主窗可见（跑着真渲染 pass）把小窗视口先建出来（保持隐藏）。
    ///
    /// 为什么必须提前建：主窗藏到托盘后 eframe 走 logic-only 路径，
    /// `show_viewport_deferred` 只写上下文状态、不产生创建命令，新视口永远
    /// 建不出来；只有已存在于 viewports 表里的视口才收得到显式命令。首帧
    /// （哪怕随后立即进托盘）一定是真 pass，趁这里建好，之后后台显隐才有着落。
    pub fn ensure_created(&mut self, ctx: &Context, theme: Theme) {
        if self.created {
            return;
        }
        let m = theme.metrics;
        let size = Vec2::new(m.s(340.0), m.s(220.0));
        let margin = m.s(16.0);
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
        let snapshot = Snapshot::placeholder(theme);
        let result = Arc::clone(&self.interrupt_result);
        ctx.show_viewport_deferred(
            viewport_id(),
            Self::builder(size, home, false),
            move |ui, _| {
                paint(ui, &snapshot, &result);
            },
        );
        self.created = true;
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
            self.refade_since = Some(Instant::now());
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

        // 4. 位置：常态贴右上；光标进入小窗区域就非线性躲到左上。
        //    只盯「静止位」判定 —— 窗口移动本身不会让光标反复进出，
        //    否则窗口会在两个角之间来回振荡。
        let m = theme.metrics;
        let size = if self.interrupt_open {
            Vec2::new(m.s(380.0), m.s(280.0))
        } else {
            Vec2::new(m.s(340.0), m.s(220.0))
        };
        let margin = m.s(16.0);
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
        let away = Pos2::new(margin, margin);
        let dodge = cursor.is_some_and(|c| Rect::from_min_size(home, size).contains(c));
        let t = ctx.animate_value_with_time(
            Id::new("neo-miniwin-avoid"),
            if dodge { 1.0 } else { 0.0 },
            AVOID_SECS,
        );
        let pos = home + (away - home) * ease_out_cubic(t);

        // 5. 打断确认：后台执行中 + 本轮动过鼠标/键盘 + 左键按下沿。
        //    点在小窗自己身上不算（那是在点弹窗按钮）。
        if lmb_edge && open && !self.interrupt_open && used_mouse_or_keyboard(state) {
            let on_miniwin =
                cursor.is_some_and(|c| Rect::from_min_size(pos, size).contains(c));
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

        // 6. 截屏隐藏结束后的轻微淡入。
        let mut refading = false;
        let fade = match self.refade_since {
            Some(t0) => {
                let k = t0.elapsed().as_secs_f32() / REFADE_SECS;
                if k >= 1.0 {
                    self.refade_since = None;
                    1.0
                } else {
                    refading = true;
                    ease_out_cubic(k)
                }
            }
            None => 1.0,
        };

        // 7. 显隐与内容。
        //
        //    关键约束：deferred 视口的「创建」与 builder patch 只在真渲染 pass
        //    里发生，主窗藏到托盘后的 logic-only 路径不产生这些命令。所以——
        //    视口必须先趁主窗可见时建好（`ensure_created`，install 与此处兜底）；
        //    显隐改用显式 `ViewportCommand::Visible`，它走另一条派发路径，
        //    logic-only 下照送不误。小窗一旦可见，`is_viewport_or_descendant_visible`
        //    又反向强制主窗跑真 pass，位置 / 避让 / 内容的增量更新随之恢复。
        if !self.created {
            if !hidden_to_tray {
                // 主窗还可见：趁这帧真 pass 把视口建出来（保持隐藏）。
                self.ensure_created(ctx, theme);
            }
        } else {
            if open != self.shown {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::Visible(open));
                if open {
                    // 每次露面都重新提到顶层，盖过同屏后到的其它 topmost 窗口。
                    ctx.send_viewport_cmd_to(
                        viewport_id(),
                        ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                    );
                }
                self.shown = open;
            }
            if open {
                let busy_secs = self
                    .busy_since
                    .map(|t0| t0.elapsed().as_secs())
                    .unwrap_or(0);
                let snapshot = Snapshot::build(state, theme, fade, self.interrupt_open, busy_secs);
                let result = Arc::clone(&self.interrupt_result);
                ctx.show_viewport_deferred(
                    viewport_id(),
                    Self::builder(size, pos, true),
                    move |ui, _class| {
                        paint(ui, &snapshot, &result);
                    },
                );
            }
        }

        // 8. 小窗开着时把帧率拉上来（隐藏态默认 200ms 一拍，动画会卡）。
        if open || refading {
            ctx.request_repaint_after(POLL);
        }
    }
}
