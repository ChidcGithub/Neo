//! 课堂总结弹窗：打磨完成后从屏幕上方滑入的横条卡片。
//!
//! 机制与确认窗一致（见 miniwin.rs 的 `OFFSCREEN` 注释）：每帧注册防回收，
//! 恒可见、休眠时缩 1x1 挪屏幕外，显隐走尺寸/位置命令。
//! 滑入动画在视口自己的回调里自驱（主窗托盘后主视口被节流到 ~10fps，
//! 动画帧不能指望它）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{
    Color32, Context, Pos2, Rect, Vec2, ViewportBuilder, ViewportCommand, ViewportId,
    WindowLevel,
};
use neo_theme::{SquirclePaint, Theme};
use neo_ui::{Icon, IconButton};

use crate::brand::WhaleMark;
use crate::class::{ClassMonitor, SaveState};
use crate::ui::{Skin, markdown, text_left};

use super::miniwin::OFFSCREEN;

/// 滑入时长（秒）。
const SLIDE_SECS: f32 = 0.35;
/// 动画帧节拍（回调自驱 60fps）。
const POLL: Duration = Duration::from_millis(16);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-classwin")
}

fn save_status(generated: bool, state: SaveState, close_blocked: bool) -> String {
    let generation = if generated { "总结已生成" } else { "未完成自动整理，已保留素材节选" };
    let saved = match state {
        SaveState::Unsaved => "尚未保存，仅在内存中；请重试保存，退出前会再次检查",
        SaveState::IndexFailed => "总结已保存；记忆索引失败，请重试",
        SaveState::Saved => "总结及记忆索引已保存",
    };
    let retained = if close_blocked { "；已保留卡片，未关闭" } else { "" };
    format!("{generation} · {saved}{retained}")
}

/// 课堂总结弹窗的运行时状态，挂在 `NeoApp` 上，每帧由 [`ClassWin::tick`] 驱动。
#[derive(Default)]
pub struct ClassWin {
    /// 上一帧是否开着（沿检测用）。
    open: bool,
    /// 回调里点「关闭」置位，tick 里落到 `ClassMonitor::dismiss`。
    close_wanted: Arc<AtomicBool>,
    retry_wanted: Arc<AtomicBool>,
    /// 滑入起点（打开沿置位，回调读完动画进度）。
    open_since: Arc<Mutex<Option<Instant>>>,
}

impl ClassWin {
    /// 最近一次原生视口采样的可见客户区，使用全局物理像素。
    /// 不用目标位置推测窗口是否已创建/移到位，也不借用 ROOT 的 DPI。
    /// 这不是 hook 点击瞬间的窗口归属；移动/显隐与采样之间仍有延迟。
    pub fn visible_rect_physical(&self, ctx: &Context) -> Option<Rect> {
        if !self.open || crate::app::desktop_suspended(ctx) {
            return None;
        }
        let zoom = ctx.zoom_factor();
        ctx.input(|input| {
            let viewport = input.raw.viewports.get(&viewport_id())?;
            if viewport.visible() == Some(false) {
                return None;
            }
            let rect = viewport.inner_rect?;
            let ppp = viewport.native_pixels_per_point? * zoom;
            if !ppp.is_finite() || ppp <= 0.0 || !rect.is_finite() {
                return None;
            }
            let physical = Rect::from_min_max(rect.min * ppp, rect.max * ppp);
            // 常驻休眠 HWND 仍会报告 1x1 客户区；打开命令尚未落地时也不能排除。
            (physical.is_finite() && physical.width() > 1.0 && physical.height() > 1.0
                && rect.width() > 1.0 && rect.height() > 1.0).then_some(physical)
        })
    }

    /// 打开时的布局目标矩形（屏幕上方居中），不能用于实际点击排除。
    pub fn target_rect(theme: Theme, monitor_size: Vec2) -> Rect {
        let m = theme.metrics;
        // 尺寸随显示器收窄（钳左上不钳右下，窗口比屏大时关闭钮会画出屏外）。
        let size = Vec2::new(m.s(640.0), m.s(460.0))
            .min(monitor_size - egui::vec2(m.s(32.0), m.s(32.0)))
            .max(egui::vec2(m.s(280.0), m.s(160.0)));
        Rect::from_min_size(
            Pos2::new(((monitor_size.x - size.x) * 0.5).max(m.s(16.0)), m.s(20.0)),
            size,
        )
    }

    /// 每帧驱动一次，挂在 `NeoApp::tick`（与确认窗同路，logic-only 也经过）。
    pub fn tick(
        &mut self,
        ctx: &Context,
        monitor: &mut ClassMonitor,
        theme: Theme,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        if let Some(layer) = overlay { layer.set_card(neo_overlay::card_id::CLASS, None); }
        // 1. 关闭回传先行：dismiss 后 presenting 转 None，本轮即走关闭沿。
        if self.retry_wanted.swap(false, Ordering::Relaxed) {
            monitor.retry_save();
        }
        if self.close_wanted.swap(false, Ordering::Relaxed) {
            monitor.dismiss();
        }
        let open = monitor.presenting().is_some();

        // 2. 位置：屏幕上方居中。
        let rect = Self::target_rect(
            theme,
            super::miniwin::screen_geometry(ctx, false).monitor,
        );
        let size = rect.size();
        let target = rect.min;

        // 3. 内容快照（两条路径共用）。
        let snapshot = monitor.presenting().map(|r| {
            (r.subject.clone(), r.summary.clone(), r.over_limit, r.date.clone(),
                save_status(r.generated, r.save_state, r.close_blocked), r.save_state != SaveState::Saved)
        });

        // 正文滚动、关闭和重试始终由独立交互视口处理。
        // 4b. 显隐沿：露面时提顶 + 记滑入起点；休眠缩 1x1 回 OFFSCREEN。
        if open != self.open {
            if open {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(target));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
                *self.open_since.lock().unwrap() = Some(Instant::now());
            } else {
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)),
                );
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
            }
            self.open = open;
        }

        // 5. 每帧注册（防回收 + 内容增量）。
        let close_wanted = Arc::clone(&self.close_wanted);
        let retry_wanted = Arc::clone(&self.retry_wanted);
        let open_since = Arc::clone(&self.open_since);
        let suspended = crate::app::desktop_viewport(ctx, viewport_id());
        ctx.show_viewport_deferred(
            viewport_id(),
            ViewportBuilder::default()
                .with_title("课堂总结")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                // 不抢焦点：老师可能正在操作电脑；关闭靠鼠标点。
                .with_active(false)
                // 正文、留白和圆角都属于不可穿透的交互客户区。
                .with_transparent(false)
                .with_mouse_passthrough(false)
                .with_visible(!suspended)
                .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
                .with_position(if open { target } else { OFFSCREEN }),
            move |ui, _class| {
                if crate::app::desktop_suspended(ui.ctx()) { return; }
                let Some((subject, summary, over_limit, date, status, retry)) = &snapshot else {
                    return;
                };
                // 滑入：从顶外到目标位，ease-out；动画期间自驱 60fps。
                let mut since = open_since.lock().unwrap();
                if let Some(t0) = *since {
                    let t = (t0.elapsed().as_secs_f32() / SLIDE_SECS).clamp(0.0, 1.0);
                    let eased = 1.0 - (1.0 - t).powi(3);
                    let y = target.y * eased + (-size.y) * (1.0 - eased);
                    // 即使两帧间停顿跨过终点，也要显式落到最终位置。
                    ui.ctx().send_viewport_cmd_to(
                        viewport_id(),
                        ViewportCommand::OuterPosition(if t < 1.0 { Pos2::new(target.x, y) } else { target }),
                    );
                    if t < 1.0 {
                        ui.ctx().request_repaint_after(POLL);
                    } else {
                        *since = None;
                    }
                }
                drop(since);

                let whale = WhaleMark::cached(ui.ctx());
                let skin = Skin::new(theme, &whale);
                if paint(ui, &skin, subject, summary, *over_limit, date, status, *retry, &retry_wanted) {
                    close_wanted.store(true, Ordering::Relaxed);
                    ui.ctx().request_repaint_of(ViewportId::ROOT);
                }
            },
        );
    }
}

/// 画卡片；点了关闭返回 true。
fn paint(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    subject: &str,
    summary: &str,
    over_limit: bool,
    date: &str,
    status: &str,
    retry: bool,
    retry_wanted: &AtomicBool,
) -> bool {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    // 卡片矩形：旧视口里 = 视口内容区；渲染层里 = Area 钉住的卡矩形。
    let rect = ui.max_rect();

    // 假阴影 + 卡片底（参数与迷你窗一致：偏移 2pt、26α 黑、圆角 18）。
    let shadow = rect.translate(egui::vec2(0.0, m.s(2.0)));
    ui.painter()
        .squircle_filled(shadow, m.s(18.0), Color32::from_black_alpha(26));
    ui.painter().squircle_filled(rect, m.s(18.0), p.bg_layer_1);
    ui.painter().squircle_stroked(
        rect,
        m.s(18.0),
        egui::Stroke::new(1.0, p.border_l1),
    );

    let pad = m.s(18.0);
    let inner = rect.shrink(pad);

    // 标题行：「课堂总结 · 数学」 + 右侧日期与关闭钮。
    let title_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(26.0)));
    let close_d = m.s(26.0);
    let close_center = Pos2::new(title_rect.right() - close_d * 0.5, title_rect.center().y);
    let close = IconButton::new(Icon::Close)
        .ghost()
        .id_salt("neo-classwin-close")
        .show_at(ui, &d, close_center);
    #[cfg(test)]
    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-classwin-close-probe"), close.rect));
    let close = close.clicked();

    let title = if date.is_empty() {
        format!("课堂总结 · {subject}")
    } else {
        format!("课堂总结 · {subject} · {date}")
    };
    // 科目名来自视觉模型输出，不守规矩时会画穿关闭钮 —— 超宽 elide。
    let title_max_w = close_center.x - close_d - title_rect.left();
    let title_font = d.font_bold(d.t().label + m.s(4.0));
    let title = super::elide(ui.painter(), &title, &title_font, title_max_w);
    text_left(
        ui.painter(),
        Rect::from_min_max(title_rect.min, Pos2::new(close_center.x - close_d, title_rect.bottom())),
        &title,
        title_font,
        p.label_primary,
    );
    let mut y = title_rect.bottom() + m.s(4.0);

    // 超限提醒（提醒而不强制：总结太长只标出来）。
    if over_limit {
        let warn_rect = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), m.s(16.0)));
        text_left(
            ui.painter(),
            warn_rect,
            "总结超过建议的 1500 字",
            skin.prop(skin.t().caption),
            p.label_tertiary,
        );
        y = warn_rect.bottom() + m.s(4.0);
    }

    // 分隔线。
    ui.painter().hline(
        inner.left()..=inner.right(),
        y + m.s(4.0),
        egui::Stroke::new(1.0, p.border_l1),
    );
    y += m.s(12.0);

    // 正文：Markdown 滚动区。
    let body_rect = Rect::from_min_max(Pos2::new(inner.left(), y), inner.max);
    let mut body_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    egui::ScrollArea::vertical()
        .id_salt("neo-classwin-body")
        .auto_shrink([false, false])
        .show(&mut body_ui, |ui| {
            ui.label(egui::RichText::new(status).color(if retry { p.error } else { p.label_tertiary }));
            if retry && ui.button("重试保存 / 索引").clicked() {
                retry_wanted.store(true, Ordering::Relaxed);
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
            ui.separator();
            markdown::render(ui, skin, summary, false);
        });

    close
}

// ---------------------------------------------------------------------------
// 总结起止红点
// ---------------------------------------------------------------------------

/// 红点亮起总时长（开始/结束总结各亮一次）：前 0.15s 淡入，末 0.4s 淡出。
const DOT_SECS: Duration = Duration::from_secs(5);

/// 左上角小红点：课堂总结「开始整理 / 整理完成」时亮 5s。
///
/// 只是个小小的状态信号 —— 老师不用盯屏幕也知道「后台开始/完成总结了」。
/// 画在渲染层上的 passive 卡（不吃输入、不挡点击）；没有渲染层
/// （离屏测试）就不画：纯装饰，不值得为它再撑一条测试视口路径。
#[derive(Default)]
pub struct ClassDot {
    /// 本批红点的亮起起点；新的起止事件重置计时（连着来就连着亮）。
    since: Option<Instant>,
}

impl ClassDot {
    /// 标记一次「开始/结束总结」。
    pub fn ping(&mut self) {
        self.since = Some(Instant::now());
    }

    /// 每帧驱动（挂在 `NeoApp::tick`；有渲染层才画）。
    pub fn tick(&mut self, overlay: Option<&neo_overlay::OverlayHandle>, theme: Theme) {
        if self.since.is_some_and(|t| t.elapsed() >= DOT_SECS) {
            self.since = None;
        }
        let Some(layer) = overlay else { return };
        let card = self.since.map(|t0| {
            let m = theme.metrics;
            // 直径随大屏度量缩放：这就是「自适应大小，比较小」。
            let d = m.s(12.0);
            let margin = m.s(14.0);
            neo_overlay::Card::passive([margin, margin, d, d], move |ui| {
                let k = (t0.elapsed().as_secs_f32() / DOT_SECS.as_secs_f32()).clamp(0.0, 1.0);
                // 淡入（前 3%）→ 保持 → 淡出（末 8%）。
                let a = (k / 0.03).min(1.0) * (1.0 - ((k - 0.92) / 0.08).clamp(0.0, 1.0));
                let p = neo_ui::Design::new(theme).p();
                let c = ui.max_rect().center();
                let r = ui.max_rect().width() * 0.5;
                // 外圈微光晕 + 实心红点。
                ui.painter().circle_filled(c, r, p.error.gamma_multiply(0.16 * a));
                ui.painter().circle_filled(c, r * 0.62, p.error.gamma_multiply(a));
            })
        });
        layer.set_card(neo_overlay::card_id::DOT, card);
    }
}

#[cfg(test)]
#[path = "classwin_tests.rs"]
mod tests;
