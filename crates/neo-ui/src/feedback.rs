//! 反馈类：加载与浮动提示。
//!
//! 状态语言的统一出处："生成中"是转圈，"出错了"是错误色的 toast ——
//! 不再每次手搓一组脉冲点或一段红字。

use egui::{Color32, Painter, Rect, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{inset, text_left};
use crate::icons::Icon;
use crate::Design;

/// 加载转圈。
///
/// 转圈本身不持有"进度"，靠一帧一帧旋转来表达"在做"，
/// 与上游的 `.pending` 呼吸点同一份语言。
pub struct Spinner {
    /// 角速度（弧度/秒）。
    speed: f32,
    stroke_w: f32,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            speed: std::f32::consts::TAU,
            stroke_w: 1.8,
        }
    }
    pub fn slow(mut self) -> Self {
        self.speed *= 0.6;
        self
    }

    /// 直接画在给定的矩形里（不分配布局）。
    pub fn show_painter(&self, painter: &Painter, d: &Design, rect: Rect, color: Color32) {
        let t = painter.ctx().time();
        let phase = (t as f32 * self.speed) % std::f32::consts::TAU;
        let r = rect.width().min(rect.height()) * 0.5 - self.stroke_w;
        // 一段 270° 的弧，随时间旋转。
        let mut pts = Vec::with_capacity(24);
        for i in 0..=24 {
            let a = phase + (i as f32 / 24.0) * std::f32::consts::TAU * 0.75;
            pts.push(egui::pos2(
                rect.center().x + a.cos() * r,
                rect.center().y + a.sin() * r,
            ));
        }
        painter.add(egui::Shape::line(
            pts,
            egui::Stroke::new(self.stroke_w, color),
        ));
        let _ = d;
    }

    /// 走布局流。
    pub fn show(&self, ui: &mut Ui, d: &Design, size: f32) {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        self.show_painter(ui.painter(), d, rect, d.p().label_tertiary);
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

/// 浮动提示（屏幕下方中央，短暂出现）。
///
/// "已复制""已保存"这类一闪而过的确认。由一个外部状态
/// （`state.toast = Some((kind, msg, 截止时间))`）驱动。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

/// 单条 toast。
pub struct Toast<'a> {
    kind: ToastKind,
    text: &'a str,
}

impl<'a> Toast<'a> {
    pub fn new(kind: ToastKind, text: &'a str) -> Self {
        Self { kind, text }
    }

    fn accent(&self, d: &Design) -> Color32 {
        let c = d.c();
        match self.kind {
            ToastKind::Info => d.p().label_secondary,
            ToastKind::Success => c.success,
            ToastKind::Warn => c.warn,
            ToastKind::Error => c.error,
        }
    }

    /// 在 `anchor`（屏幕中央偏下）绘制。
    pub fn show_at(&self, ui: &mut Ui, d: &Design, anchor: egui::Pos2) {
        let m = d.m();
        let font = d.font(d.t().label);
        let text_w = ui
            .painter()
            .layout_no_wrap(self.text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x;
        let icon_d = m.s(16.0);
        let w = text_w + icon_d + m.s(8.0) + m.s(28.0);
        let h = m.s(40.0);
        let rect = Rect::from_center_size(anchor, Vec2::new(w, h));

        let c = d.c();
        ui.painter()
            .add(crate::base::elevation_soft(d).as_shape(rect, m.s(20.0)));
        ui.painter().squircle(
            rect,
            m.s(20.0),
            c.toast,
            egui::Stroke::new(1.0, crate::base::translucent(d.p().border_l2, 0.4)),
        );

        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.left() + m.s(14.0) + icon_d * 0.5, rect.center().y),
            Vec2::splat(icon_d),
        );
        let icon = match self.kind {
            ToastKind::Info => Icon::Info,
            ToastKind::Success => Icon::Check,
            ToastKind::Warn => Icon::Warn,
            ToastKind::Error => Icon::Close,
        };
        icon.paint(ui.painter(), icon_rect, self.accent(d));
        text_left(
            ui.painter(),
            inset(rect, m.s(38.0), 0.0, m.s(14.0), 0.0),
            self.text,
            font,
            d.p().label_primary,
        );
    }
}
