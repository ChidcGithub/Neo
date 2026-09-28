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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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
        // 0x8000 = 此刻按着；0x0001 = 自上次调用以来按过 —— 托盘态 ~10fps 的
        // 采样会漏掉比间隔还快的点击（打断确认不弹），两个位都要。
        let lmb = (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8001) != 0;
        Some((Pos2::new(point.x as f32, point.y as f32), lmb))
    }
}

/// 非 Windows 没有全局输入轮询（小窗退化为只读状态牌）。
#[cfg(not(windows))]
fn global_cursor() -> Option<(Pos2, bool)> {
    None
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 全局输入单点采样（只写于视口回调，读于 tick）。
///
/// `GetAsyncKeyState` 的「自上次调用以来按过」位（0x0001）**按线程锁存、读后
/// 即清**：回调（60fps）与 tick（托盘态 ~10fps）同线程抢读时，按下沿会先被
/// 回调清掉，tick 侧只剩「此刻仍按着」——一次 30ms 的快速点按约一半概率漏检。
/// 所以只有回调采样，结果经这三个槽位转发：光标每次采样刷新，按下沿记时刻。
static CURSOR: Mutex<Option<(Pos2, bool)>> = Mutex::new(None);
/// 最近一次左键按下沿的时刻（ms）；0 = 从未按过。
static LMB_EDGE_MS: AtomicU64 = AtomicU64::new(0);
/// 回调侧的上一帧按下态（沿检测用）。
static PREV_DOWN: AtomicBool = AtomicBool::new(false);

/// 视口回调里每帧调用一次：采样全局输入，刷新 [`CURSOR`] / [`LMB_EDGE_MS`]。
fn poll_global_input() {
    match global_cursor() {
        Some((pos, down)) => {
            let prev = PREV_DOWN.swap(down, Ordering::Relaxed);
            if down && !prev {
                LMB_EDGE_MS.store(now_ms(), Ordering::Relaxed);
            }
            if let Ok(mut g) = CURSOR.lock() {
                *g = Some((pos, down));
            }
        }
        None => {
            PREV_DOWN.store(false, Ordering::Relaxed);
            if let Ok(mut g) = CURSOR.lock() {
                *g = None;
            }
        }
    }
}

/// 最近一次 AI 合成输入（click/drag 工具的 SendInput 注入）是不是就发生在刚刚。
///
/// 合成点击与物理点击在系统里不可区分，只能靠工具层留的时间戳排除：
/// 注入与采样跨线程，留 1s 窗口盖住「注入完成 → 下一帧采样」的间隔。
/// 不排除的话，AI 操作鼠标时一次按下沿就会弹出「打断执行？」——
/// 弹窗又恰好钉在最大化窗口的关闭按钮区，AI 可能自己把自己打断。
fn synthetic_click_recent() -> bool {
    let last = neo_tools::tools::screen::SYNTHETIC_INPUT_AT.load(Ordering::Relaxed);
    if last == 0 {
        return false;
    }
    now_ms().saturating_sub(last) < 1000
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
    interrupt_open: bool,
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
        interrupt_open: bool,
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
            _ => ("正在处理…", true),
        };
        // 本轮工具流水（自最后一条用户消息起）：最近 MAX_STEPS 步，旧的在上。
        // 倒序收集到量即停，再翻回正序 —— 老消息成堆时不全扫。
        let m = theme.metrics;
        let inner_w = m.s(340.0 - 28.0);
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
            interrupt_open,
            steps,
            body,
            body_dim,
            streaming,
        }
    }
}

/// 画一帧小窗内容：工具流水 + 正文（markdown / LaTeX 渲染）。
///
/// 正文用组件布局（不再是纯 painter 排版），量到的内容高写进 `measure`
/// 槽 —— tick 下一帧据此算卡片目标高（自适应最后一段正文）。
fn paint(
    ui: &mut egui::Ui,
    snap: &Snapshot,
    result: &Arc<AtomicU8>,
    fade: f32,
    measure: &Arc<Mutex<f32>>,
) {
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

    // 打断确认：**内联画在卡片里**（不能用模态 —— 模态居中到整层屏幕，
    // 逃出卡片命中矩形，按钮永远点不到）。
    if snap.interrupt_open {
        let inner = rect.shrink(m.s(18.0));
        painter.text(
            inner.left_top(),
            Align2::LEFT_TOP,
            "打断执行？",
            d.font_bold(d.t().headline),
            tint(p.label_primary),
        );
        let body_y = inner.top() + d.t().headline * 1.6;
        let body = ui.painter().layout(
            "AI 本轮正在操作鼠标 / 键盘。打断会立即取消当前任务。".to_owned(),
            d.font(d.t().body),
            tint(p.label_secondary),
            inner.width(),
        );
        painter.galley(Pos2::new(inner.left(), body_y), body, tint(p.label_secondary));
        // 底部按钮行：右对齐 [继续][打断]。
        let btn_h = m.s(34.0);
        let footer =
            Rect::from_min_max(Pos2::new(inner.left(), inner.bottom() - btn_h), inner.max);
        let mut clicked: Option<bool> = None;
        super::at(ui, footer, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new("打断").danger().show(ui, &d).clicked() {
                    clicked = Some(true);
                }
                if Button::new("继续").ghost().show(ui, &d).clicked() {
                    clicked = Some(false);
                }
            });
        });
        if let Some(yes) = clicked {
            result.store(if yes { 1 } else { 2 }, Ordering::Relaxed);
        }
        return;
    }

    let inner = rect.shrink(m.s(14.0));
    let step_lh = d.t().caption * 1.55;
    super::at(ui, inner, |ui| {
        ui.set_opacity(fade);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.set_max_width(inner.width());

        // 工具流水：状态圆点 + 单行摘要，最近的在最下。
        for step in &snap.steps {
            let (row, _) = ui.allocate_exact_size(
                Vec2::new(inner.width(), step_lh),
                egui::Sense::hover(),
            );
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
            self.cumulative.push(self.cumulative[i]
                + self.points[i].distance(self.points[(i + 1) % self.points.len()]));
        }
        self.total = *self.cumulative.last().unwrap();
        self.key = Some(key);
        true
    }

    fn at(&self, s: f32) -> Pos2 {
        let s = s.rem_euclid(self.total);
        let idx = match self.cumulative.binary_search_by(|c|
            c.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Equal)) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }.min(self.points.len() - 1);
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
    /// 已消费的左键按下沿时刻（同一沿在 250ms 窗口内不重复触发）。
    lmb_edge_consumed: u64,
    /// 忙完后的驻留截止时刻（忙时持续刷新，闲下来那一刻起算 5s）。
    linger_until: Option<Instant>,
    /// 淡出起点（驻留结束 → 播完才收窗）；回调里读它算透明度与上飘，
    /// 所以走共享槽（与 refade 同理：主视口托盘态只有 10fps）。
    fadeout_since: Arc<Mutex<Option<Instant>>>,
    /// 内容高度测量槽：paint 写（回调里现量）、tick 读（算目标高）。
    content_h: Arc<Mutex<f32>>,
    /// 测试回退路径最近下发的视口尺寸（没变就不重发 InnerSize）。
    legacy_sent: Vec2,
    /// 渲染层卡片的共享矩形（主屏点）：tick 写静止位、层内绘制闭包写
    /// 避让动画位；层的 Area 定位与命中测试每帧重读它。打断判定的
    /// 「点在小窗自己身上」也读它（替代旧视口的 ctx.data 通道）。
    layer_rect: Arc<Mutex<[f32; 4]>>,
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
    ///
    /// `classwin_rect`：课堂总结弹窗开着时的目标矩形 —— 点它的「关闭」
    /// 不该被当成「用户想打断 AI」。
    pub fn tick(
        &mut self,
        ctx: &Context,
        state: &mut AppState,
        theme: Theme,
        hidden_to_tray: bool,
        classwin_rect: Option<Rect>,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
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
        //    忙完后驻留 LINGER（5s）播完成态，驻留结束再播一段淡出才收窗。
        let busy = state.generating || state.tool_open || state.tool_round;
        if busy {
            self.linger_until = Some(Instant::now() + LINGER);
            // 重新忙起来：进行中的淡出立即取消（接着播内容）。
            *self.fadeout_since.lock().unwrap() = None;
        }
        let lingering = !busy && self.linger_until.is_some_and(|t| Instant::now() < t);
        if !busy && !lingering {
            // 真正闲下来才收确认框 —— 只判 !busy 的话，流结束与下一轮工具
            // 启动之间的空档会把弹窗闪掉（用户正点着呢，框没了）。
            self.interrupt_open = false;
            self.interrupt_result.store(0, Ordering::Relaxed);
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
        let open = hidden_to_tray && (busy || lingering || (fading && !faded)) && !shot_hiding;

        // 3. 全局输入（由视口回调单点采样转发，见 LMB_EDGE_MS 注释）：
        //    逻辑坐标光标 + 左键按下沿。沿带 250ms 窗口并记消费，托盘态
        //    ~10fps 的 tick 不会漏掉两次 tick 之间的快速点按。
        let ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let cursor = CURSOR
            .lock()
            .ok()
            .and_then(|g| *g)
            .map(|(physical, _)| Pos2::new(physical.x / ppp, physical.y / ppp));
        let edge_ms = LMB_EDGE_MS.load(Ordering::Relaxed);
        let lmb_edge = edge_ms != 0
            && edge_ms != self.lmb_edge_consumed
            && now_ms().saturating_sub(edge_ms) < 250;
        if lmb_edge {
            self.lmb_edge_consumed = edge_ms;
        }

        // 4. 几何：常态贴右上、避让时躲左上。宽度固定；高度**自适应内容** ——
        //    paint 量到的内容高（上一帧）+ 边距，钳制后由回调朝它做缓动。
        //    打断弹窗打开时用固定大窗（模态需要稳定的落点）。
        let m = theme.metrics;
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let (width, target_h) = if self.interrupt_open {
            (m.s(380.0), m.s(280.0))
        } else {
            let content = *self.content_h.lock().unwrap();
            (
                m.s(340.0),
                (content + m.s(28.0)).clamp(m.s(96.0), (monitor.y - m.s(32.0)) * 0.62),
            )
        };
        let size = Vec2::new(width, target_h);
        let margin = m.s(16.0);
        let home = Pos2::new((monitor.x - width - margin).max(margin), margin);
        let away = Pos2::new(margin, margin);

        // 5. 打断确认：后台执行中 + 本轮动过鼠标/键盘 + 左键按下沿。
        //    点在小窗自己身上不算（那是在点弹窗按钮）；有待确认的工具也不算 ——
        //    那一击多半是点在独立确认窗的按钮上，且等待权限时 AI 本就停着；
        //    点在课堂总结弹窗上不算；AI 自己注入的点击（click/drag）更不算。
        if lmb_edge
            && open
            && busy // 驻留/淡出中不弹打断（已经做完了，没什么可打断的）
            && !self.interrupt_open
            && state.awaiting_tool().is_none()
            && used_mouse_or_keyboard(state)
            && !synthetic_click_recent()
        {
            let on_miniwin = cursor.is_some_and(|c| {
                // 实时位置优先（避让动画途中窗体不在任何一个静止端点上）：
                // 渲染层模式读共享矩形，旧视口模式读回调写进 ctx.data 的位置。
                let live = if overlay.is_some() {
                    let r = *self.layer_rect.lock().unwrap();
                    Some(Rect::from_min_size(Pos2::new(r[0], r[1]), Vec2::new(r[2], r[3])))
                } else {
                    ctx.data(|d| d.get_temp::<Pos2>(Id::new("neo-miniwin-pos")))
                        .map(|pos| Rect::from_min_size(pos, size))
                };
                live.is_some_and(|r| r.contains(c))
                    || Rect::from_min_size(home, size).contains(c)
                    || Rect::from_min_size(away, size).contains(c)
            });
            let on_classwin = cursor
                .zip(classwin_rect)
                .is_some_and(|(c, r)| r.contains(c));
            if !on_miniwin && !on_classwin {
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
        //    渲染层路径：每帧覆写卡片（与旧视口「每帧注册防回收」同构），
        //    避让/淡入动画由卡片的绘制闭包在层里以 vsync 自驱 —— 主视口
        //    托盘态 10fps 的节拍只负责内容快照，与旧架构的权责划分一致。
        let snapshot = if open {
            Some(Snapshot::build(
                state,
                theme,
                width,
                target_h,
                monitor,
                !busy && (lingering || fading),
                self.interrupt_open,
            ))
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
                let result = Arc::clone(&self.interrupt_result);
                let refade = Arc::clone(&self.refade_since);
                let rect_slot = Arc::clone(&self.layer_rect);
                let fadeout = Arc::clone(&self.fadeout_since);
                let measure = Arc::clone(&self.content_h);
                let card = neo_overlay::Card {
                    rect: Arc::clone(&self.layer_rect),
                    interactive: true,
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
                            .map(|t0| {
                                (t0.elapsed().as_secs_f32() / FADEOUT_SECS).clamp(0.0, 1.0)
                            })
                            .unwrap_or(0.0);
                        let fade = fade_in * (1.0 - ease_out_cubic(fade_out));
                        // 避让：只盯「静止位」判定（卡片移动本身不会让光标
                        // 反复进出，否则会在两个角之间振荡）；弹窗打开时不躲。
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
                        let home =
                            Pos2::new((monitor.x - size.x - margin).max(margin), margin);
                        let away = Pos2::new(margin, margin);
                        let ppp = ctx.pixels_per_point();
                        let cursor = {
                            // 采样顺带刷新槽位（层内 vsync 单点采样，tick 的
                            // 沿检测靠它喂 —— 见 LMB_EDGE_MS 注释）。
                            poll_global_input();
                            CURSOR
                                .lock()
                                .ok()
                                .and_then(|g| *g)
                                .map(|(p, _)| Pos2::new(p.x / ppp, p.y / ppp))
                        };
                        let dodge = !snap.interrupt_open
                            && cursor
                                .is_some_and(|c| Rect::from_min_size(home, size).contains(c));
                        let t = ctx.animate_value_with_time(
                            Id::new("neo-miniwin-avoid"),
                            if dodge { 1.0 } else { 0.0 },
                            AVOID_SECS,
                        );
                        let mut pos = home + (away - home) * ease_out_cubic(t);
                        pos.y -= m.s(10.0) * ease_out_cubic(fade_out);
                        // 写回共享矩形：层的 Area 定位 + 命中测试 + tick 的
                        // 打断排除区下一帧都用它（一帧延迟无感）。
                        *rect_slot.lock().unwrap() = [pos.x, pos.y, size.x, size.y];
                        paint(ui, &snap, &result, fade, &measure);
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
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)));
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
        let result = Arc::clone(&self.interrupt_result);
        let refade = Arc::clone(&self.refade_since);
        let fadeout = Arc::clone(&self.fadeout_since);
        let measure = Arc::clone(&self.content_h);
        ctx.show_viewport_deferred(
            viewport_id(),
            Self::builder(size, open),
            move |ui, _class| {
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
                // 否则会在两个角之间振荡）；弹窗打开时不躲（用户正要去点按钮）。
                let m = snapshot.theme.metrics;
                let size = Vec2::new(snapshot.width, snapshot.target_h);
                let margin = m.s(16.0);
                let monitor = ctx
                    .input(|i| i.viewport().monitor_size)
                    .unwrap_or(Vec2::new(1920.0, 1080.0));
                let home = Pos2::new((monitor.x - size.x - margin).max(margin), margin);
                let away = Pos2::new(margin, margin);
                let ppp = ctx
                    .input(|i| i.viewport().native_pixels_per_point)
                    .unwrap_or(1.0);
                let cursor = {
                    // 采样顺带刷新槽位（60fps 单点，tick 的沿检测靠它喂）。
                    poll_global_input();
                    CURSOR
                        .lock()
                        .ok()
                        .and_then(|g| *g)
                        .map(|(p, _)| Pos2::new(p.x / ppp, p.y / ppp))
                };
                let dodge = !snapshot.interrupt_open
                    && cursor.is_some_and(|c| Rect::from_min_size(home, size).contains(c));
                let t = ctx.animate_value_with_time(
                    Id::new("neo-miniwin-avoid"),
                    if dodge { 1.0 } else { 0.0 },
                    AVOID_SECS,
                );
                let mut pos = home + (away - home) * ease_out_cubic(t);
                pos.y -= m.s(10.0) * ease_out_cubic(fade_out);
                // 位置没变就不发：每条命令都是一次 SetWindowPos + DWM 重合成，
                // 静止时每帧白调是纯开销。
                let pos_id = Id::new("neo-miniwin-pos");
                let last = ctx.data_mut(|d| d.get_temp::<Pos2>(pos_id));
                if last != Some(pos) {
                    ctx.data_mut(|d| d.insert_temp(pos_id, pos));
                    ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(pos));
                }
                paint(ui, &snapshot, &result, fade, &measure);
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
mod beam_tests {
    use super::*;

    #[test]
    fn beam_cache_invalidates_only_geometry_inputs() {
        let mut cache = BeamGeometry::default();
        let mut key = BeamKey::new(Vec2::new(340.0, 96.0), 18.0);
        assert!(cache.ensure(key));
        assert!(!cache.ensure(key));
        key.size.y += 1.0;
        assert!(cache.ensure(key));
        key.size.x += 1.0;
        assert!(cache.ensure(key));
        key.radius += 1.0;
        assert!(cache.ensure(key));
        key.segments += 1;
        assert!(cache.ensure(key));
        key.superellipse += 0.1;
        assert!(cache.ensure(key));
        assert!(!cache.ensure(key));
    }

    #[test]
    fn beam_local_geometry_matches_absolute_reference_and_wraps() {
        for size in [Vec2::new(340.0, 96.0), Vec2::new(340.0, 273.5), Vec2::new(510.0, 144.0)] {
            for radius in [0.0, 18.0, 27.0] {
                let mut cache = BeamGeometry::default();
                cache.ensure(BeamKey::new(size, radius));
                for origin in [Pos2::ZERO, Pos2::new(-1280.0, 16.0), Pos2::new(1523.25, 8.75)] {
                    let pts = neo_theme::squircle::squircle_points(
                        Rect::from_min_size(origin, size).shrink(0.5), radius,
                        neo_theme::HARNESS_SUPERELLIPSE, neo_theme::squircle::DEFAULT_SEGMENTS);
                    let mut cum = vec![0.0];
                    for i in 0..pts.len() {
                        cum.push(cum[i] + pts[i].distance(pts[(i + 1) % pts.len()]));
                    }
                    let total = *cum.last().unwrap();
                    for now in [0.0, 0.3, 1.7, 2.39999, 2.4, 23.0] {
                        let head = ((now / 2.4_f64).fract() as f32) * total;
                        let tail = total * 0.30;
                        for (i, p) in cache.tail_points(now).iter().enumerate() {
                            let s = (head - tail + tail * (i as f32 / 28.0)).rem_euclid(total);
                            let idx = match cum.binary_search_by(|c| c.partial_cmp(&s).unwrap()) {
                                Ok(i) => i,
                                Err(i) => i.saturating_sub(1),
                            }.min(pts.len() - 1);
                            let t = ((s - cum[idx]) / (cum[idx + 1] - cum[idx]).max(f32::EPSILON)).clamp(0.0, 1.0);
                            let reference = pts[idx] + (pts[(idx + 1) % pts.len()] - pts[idx]) * t;
                            assert!((*p + origin.to_vec2()).distance(reference) < 0.002);
                        }
                    }
                }
                assert!(cache.at(-cache.total * 0.25).distance(cache.at(cache.total * 0.75)) < 0.001);
            }
        }
    }

    #[test]
    fn beam_synthetic_frames_reuse_buffers_and_reduce_rebuilds() {
        const FRAMES: usize = 6000;
        let key = BeamKey::new(Vec2::new(340.0, 180.0), 18.0);
        let start = Instant::now();
        for frame in 0..FRAMES {
            let mut geometry = BeamGeometry::default();
            geometry.ensure(std::hint::black_box(key));
            let head = ((frame as f64 / 60.0 / 2.4).fract() as f32) * geometry.total;
            let tail = geometry.total * 0.30;
            for i in 0..28 {
                std::hint::black_box(geometry.at(head - tail + tail * (i as f32 / 28.0)));
                std::hint::black_box(geometry.at(head - tail + tail * ((i + 1) as f32 / 28.0)));
            }
        }
        let uncached = start.elapsed();
        let start = Instant::now();
        let mut cache = BeamGeometry::default();
        let mut rebuilds = usize::from(cache.ensure(key));
        let buffers = (cache.points.as_ptr(), cache.cumulative.as_ptr());
        let capacities = (cache.points.capacity(), cache.cumulative.capacity());
        for frame in 0..FRAMES {
            // 模拟避让平移，局部尺寸不变。
            let rect = Rect::from_min_size(Pos2::new(frame as f32 * 0.25, 16.0), key.size);
            rebuilds += usize::from(cache.ensure(BeamKey::new(rect.size(), key.radius)));
            let points = cache.tail_points(std::hint::black_box(frame as f64 / 60.0));
            std::hint::black_box(points.map(|p| p + rect.min.to_vec2()));
            assert_eq!((cache.points.as_ptr(), cache.cumulative.as_ptr()), buffers);
            assert_eq!((cache.points.capacity(), cache.cumulative.capacity()), capacities);
        }
        assert_eq!(rebuilds, 1);
        eprintln!("{FRAMES} 合成帧：几何重建 {FRAMES} -> {rebuilds}，端点查找 {} -> {}；缓存命中不分配几何 Vec；未缓存 {:?}，缓存 {:?}（仅本机合成 CPU 路径）",
            FRAMES * 56, FRAMES * 29, uncached, start.elapsed());
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
    pub fn tick(&mut self, ctx: &Context, overlay: Option<&neo_overlay::OverlayHandle>) {
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

        // 2. 几何：虚拟屏物理像素 → 点（多屏时原点可为负）。
        let ppp = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0);
        let vs = screen::virtual_screen();
        let vs_size = Vec2::new(
            (vs.width as f32 / ppp).max(1.0),
            (vs.height as f32 / ppp).max(1.0),
        );
        let origin = Pos2::new(vs.x as f32 / ppp, vs.y as f32 / ppp);

        // 3a. 渲染层：闪光是一张 passive 卡（只画不收点击），区域即卡矩形。
        if let Some(layer) = overlay {
            if on {
                let region = self.region.unwrap_or(vs);
                let rect = [
                    region.x as f32 / ppp,
                    region.y as f32 / ppp,
                    region.width as f32 / ppp,
                    region.height as f32 / ppp,
                ];
                let t0 = self.flashing_since.unwrap_or_else(Instant::now);
                let card = neo_overlay::Card::passive(rect, move |ui| {
                    let r = ui.max_rect();
                    paint_flash(ui, Some((r, t0)));
                });
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
