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
use crate::class::ClassMonitor;
use crate::ui::{Skin, markdown, text_left};

use super::miniwin::OFFSCREEN;

/// 滑入时长（秒）：从屏幕顶外滑到目标位。
const SLIDE_SECS: f32 = 0.35;
/// 动画帧节拍（回调自驱 60fps）。
const POLL: Duration = Duration::from_millis(16);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-classwin")
}

/// 课堂总结弹窗的运行时状态，挂在 `NeoApp` 上，每帧由 [`ClassWin::tick`] 驱动。
#[derive(Default)]
pub struct ClassWin {
    /// 上一帧是否开着（沿检测用）。
    open: bool,
    /// 回调里点「关闭」置位，tick 里落到 `ClassMonitor::dismiss`。
    close_wanted: Arc<AtomicBool>,
    /// 滑入起点（打开沿置位，回调读完动画进度）。
    open_since: Arc<Mutex<Option<Instant>>>,
}

impl ClassWin {
    /// 打开时的目标矩形（屏幕上方居中）。打断判定（miniwin）也用它排除
    /// 「点在课堂总结窗上」的点击。
    pub fn target_rect(theme: Theme, monitor_size: Vec2) -> Rect {
        let m = theme.metrics;
        let size = Vec2::new(m.s(640.0), m.s(460.0));
        Rect::from_min_size(
            Pos2::new(((monitor_size.x - size.x) * 0.5).max(m.s(16.0)), m.s(20.0)),
            size,
        )
    }

    /// 每帧驱动一次，挂在 `NeoApp::tick`（与确认窗同路，logic-only 也经过）。
    pub fn tick(&mut self, ctx: &Context, monitor: &mut ClassMonitor, theme: Theme) {
        // 1. 关闭回传先行：dismiss 后 presenting 转 None，本轮即走关闭沿。
        if self.close_wanted.swap(false, Ordering::Relaxed) {
            monitor.dismiss();
        }
        let open = monitor.presenting().is_some();

        // 2. 位置：屏幕上方居中。
        let rect = Self::target_rect(
            theme,
            ctx.input(|i| i.viewport().monitor_size)
                .unwrap_or(Vec2::new(1920.0, 1080.0)),
        );
        let size = rect.size();
        let target = rect.min;

        // 3. 显隐沿：露面时提顶 + 记滑入起点；休眠缩 1x1 回 OFFSCREEN。
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

        // 4. 每帧注册（防回收 + 内容增量）。
        let snapshot: Option<(String, String, bool, String)> = monitor.presenting().map(|r| {
            (r.subject.clone(), r.summary.clone(), r.over_limit, r.date.clone())
        });
        let close_wanted = Arc::clone(&self.close_wanted);
        let open_since = Arc::clone(&self.open_since);
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
                .with_transparent(true)
                .with_visible(true)
                .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
                .with_position(if open { target } else { OFFSCREEN }),
            move |ui, _class| {
                let Some((subject, summary, over_limit, date)) = &snapshot else {
                    return;
                };
                // 滑入：从顶外到目标位，ease-out；动画期间自驱 60fps。
                let since = open_since.lock().unwrap();
                if let Some(t0) = *since {
                    let t = (t0.elapsed().as_secs_f32() / SLIDE_SECS).clamp(0.0, 1.0);
                    let eased = 1.0 - (1.0 - t).powi(3);
                    let y = target.y * eased + (-size.y) * (1.0 - eased);
                    if t < 1.0 {
                        ui.ctx().send_viewport_cmd_to(
                            viewport_id(),
                            ViewportCommand::OuterPosition(Pos2::new(target.x, y)),
                        );
                        ui.ctx().request_repaint_after(POLL);
                    }
                }
                drop(since);

                let whale = WhaleMark::load(ui.ctx());
                let skin = Skin::new(theme, &whale);
                if paint(ui, &skin, subject, summary, *over_limit, date) {
                    close_wanted.store(true, Ordering::Relaxed);
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
) -> bool {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let rect = Rect::from_min_size(Pos2::ZERO, ui.available_size());

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
        .show_at(ui, &d, close_center)
        .clicked();

    let title = if date.is_empty() {
        format!("课堂总结 · {subject}")
    } else {
        format!("课堂总结 · {subject} · {date}")
    };
    text_left(
        ui.painter(),
        Rect::from_min_max(title_rect.min, Pos2::new(close_center.x - close_d, title_rect.bottom())),
        &title,
        d.font_bold(d.t().label + m.s(4.0)),
        p.label_primary,
    );
    let mut y = title_rect.bottom() + m.s(4.0);

    // 超限提醒（提醒而不强制：总结太长只标出来）。
    if over_limit {
        let warn_rect = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), m.s(16.0)));
        text_left(
            ui.painter(),
            warn_rect,
            "总结超过 1500 字，未强制截断",
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
            markdown::render(ui, skin, summary, false);
        });

    close
}
