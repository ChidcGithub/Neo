//! 屏幕中央的结果卡：AI 调用 `present_result` 后一直显示，直到用户关闭。
//!
//! 机制与确认窗一致（见 miniwin.rs 的 `OFFSCREEN` 注释）：每帧注册防回收，
//! 恒可见、休眠时缩 1x1 挪屏幕外，显隐走尺寸/位置命令。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Context, Pos2, Rect, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel};
use neo_theme::{SquirclePaint, Theme};
use neo_ui::{Icon, IconButton};

use crate::brand::WhaleMark;
use crate::state::AppState;
use crate::ui::{Skin, markdown, text_left};

use super::miniwin::OFFSCREEN;

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-resultwin")
}

/// 结果卡视口的运行时状态，挂在 `NeoApp` 上，每帧由 [`ResultWin::tick`] 驱动。
#[derive(Default)]
pub struct ResultWin {
    /// 上一帧是否开着（沿检测用）。
    open: bool,
    /// 回调里点「关闭」置位，tick 里清掉 `AppState::presented`。
    close_wanted: Arc<AtomicBool>,
}

impl ResultWin {
    /// 最近一次原生视口采样的可见客户区，使用全局物理像素。
    /// 与课堂总结窗同路：只认已落地的几何，不用目标位置推测。
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
            (physical.is_finite()
                && physical.width() > 1.0
                && physical.height() > 1.0
                && rect.width() > 1.0
                && rect.height() > 1.0)
                .then_some(physical)
        })
    }

    /// 打开时的布局目标矩形（屏幕正中）。
    pub fn target_rect(theme: Theme, monitor_size: Vec2) -> Rect {
        let m = theme.metrics;
        let size = Vec2::new(m.s(760.0), m.s(600.0))
            .min(monitor_size - egui::vec2(m.s(48.0), m.s(48.0)))
            .max(egui::vec2(m.s(360.0), m.s(240.0)));
        Rect::from_min_size(
            Pos2::new(
                ((monitor_size.x - size.x) * 0.5).max(m.s(16.0)),
                ((monitor_size.y - size.y) * 0.5).max(m.s(16.0)),
            ),
            size,
        )
    }

    /// 每帧驱动一次，挂在 `NeoApp::tick`（与确认窗同路，logic-only 也经过）。
    pub fn tick(
        &mut self,
        ctx: &Context,
        state: &mut AppState,
        theme: Theme,
        _overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        if self.close_wanted.swap(false, Ordering::AcqRel) {
            state.presented = None;
            ctx.request_repaint_of(ViewportId::ROOT);
        }
        let card = state.presented.clone();
        let open = card.is_some();
        let rect = Self::target_rect(theme, super::miniwin::screen_geometry(ctx, false).monitor);
        let size = rect.size();
        let target = rect.min;

        if open != self.open {
            if open {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(target));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
            } else {
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)),
                );
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
            }
            self.open = open;
        }

        let close_wanted = Arc::clone(&self.close_wanted);
        let suspended = crate::app::desktop_viewport(ctx, viewport_id());
        ctx.show_viewport_deferred(
            viewport_id(),
            ViewportBuilder::default()
                .with_title(crate::i18n::tr("Neo · 结果"))
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                // 不抢焦点：结果卡只是展示，用户可能正在别处操作。
                .with_active(false)
                .with_transparent(false)
                .with_mouse_passthrough(false)
                .with_visible(!suspended)
                .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
                .with_position(if open { target } else { OFFSCREEN }),
            move |ui, _class| {
                if crate::app::desktop_suspended(ui.ctx()) {
                    return;
                }
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    close_wanted.store(true, Ordering::Release);
                    ui.ctx().request_repaint_of(ViewportId::ROOT);
                }
                let Some(card) = &card else { return };
                let whale = WhaleMark::cached(ui.ctx());
                let skin = Skin::new(theme, &whale);
                if paint(ui, &skin, card) {
                    close_wanted.store(true, Ordering::Release);
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
    card: &neo_tools::tools::present_result::Card,
) -> bool {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let rect = ui.max_rect();

    // 假阴影 + 卡片底（与迷你窗 / 课堂总结同一套参数）。
    let shadow = rect.translate(egui::vec2(0.0, m.s(2.0)));
    ui.painter()
        .squircle_filled(shadow, m.s(18.0), egui::Color32::from_black_alpha(26));
    ui.painter().squircle_filled(rect, m.s(18.0), p.bg_layer_1);
    ui.painter()
        .squircle_stroked(rect, m.s(18.0), egui::Stroke::new(1.0, p.border_l1));

    let pad = m.s(22.0);
    let inner = rect.shrink(pad);
    let title_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(30.0)));
    let close_d = m.s(28.0);
    let close_center = Pos2::new(title_rect.right() - close_d * 0.5, title_rect.center().y);
    let close = IconButton::new(Icon::Close)
        .ghost()
        .id_salt("neo-resultwin-close")
        .show_at(ui, &d, close_center)
        .on_hover_text(crate::i18n::tr("关闭"));
    let close = close.clicked();

    let title_font = d.font_bold(d.t().label + m.s(4.0));
    let title_max_w = close_center.x - close_d - title_rect.left();
    let title = super::elide(ui.painter(), &card.title, &title_font, title_max_w);
    text_left(
        ui.painter(),
        Rect::from_min_max(
            title_rect.min,
            Pos2::new(close_center.x - close_d, title_rect.bottom()),
        ),
        &title,
        title_font,
        p.label_primary,
    );

    let y = title_rect.bottom() + m.s(8.0);
    ui.painter().hline(
        inner.left()..=inner.right(),
        y,
        egui::Stroke::new(1.0, p.border_l1),
    );

    let body_rect = Rect::from_min_max(Pos2::new(inner.left(), y + m.s(12.0)), inner.max);
    let mut body_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    // 新结果换一个滚动 id，避免沿用上一张卡的位置。
    let body_id = card as *const _ as usize;
    egui::ScrollArea::vertical()
        .id_salt(("neo-resultwin-body", body_id))
        .auto_shrink([false, false])
        .show(&mut body_ui, |ui| {
            markdown::render(ui, skin, &card.content, false);
        });

    close
}

#[cfg(test)]
#[path = "resultwin_tests.rs"]
mod tests;
