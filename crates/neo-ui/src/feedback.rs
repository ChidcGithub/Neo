//! 反馈类：加载、空态、浮动提示、悬浮提示。
//!
//! 状态语言的统一出处："生成中"是转圈，"没有内容"是空态，"出错了"
//! 是错误色的 toast —— 不再每次手搓一组脉冲点或一段红字。

use egui::{Color32, Painter, Rect, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{inset, text_center, text_left};
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

/// 空态：图标 + 标题 + 说明。
///
/// 消息流为空、会话列表为空、搜索结果为空 —— 都走它，
/// 不再各自拼一个居中的灰字。
pub struct EmptyState<'a> {
    icon: Icon,
    title: &'a str,
    hint: Option<&'a str>,
}

impl<'a> EmptyState<'a> {
    pub fn new(icon: Icon, title: &'a str) -> Self {
        Self {
            icon,
            title,
            hint: None,
        }
    }
    pub fn hint(mut self, h: &'a str) -> Self {
        self.hint = Some(h);
        self
    }

    pub fn show(&self, ui: &mut Ui, d: &Design, rect: Rect) {
        let p = d.p();
        let m = d.m();
        let painter = ui.painter();

        let icon_d = m.s(36.0);
        let title_h = d.t().label_lh;
        let hint_h = if self.hint.is_some() { m.s(18.0) } else { 0.0 };
        let total = icon_d
            + m.s(10.0)
            + title_h
            + if self.hint.is_some() {
                m.s(4.0) + hint_h
            } else {
                0.0
            };
        let top = rect.center().y - total * 0.5;

        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.center().x, top + icon_d * 0.5),
            Vec2::splat(icon_d),
        );
        self.icon.paint(
            painter,
            icon_rect,
            crate::base::translucent(p.label_caption, 0.7),
            1.6,
        );

        let title_rect = Rect::from_min_size(
            egui::pos2(rect.left(), icon_rect.bottom() + m.s(10.0)),
            Vec2::new(rect.width(), title_h),
        );
        text_center(
            painter,
            title_rect,
            self.title,
            d.font_bold(d.t().label),
            p.label_secondary,
        );

        if let Some(hint) = self.hint {
            let hint_rect = Rect::from_min_size(
                egui::pos2(rect.left(), title_rect.bottom() + m.s(4.0)),
                Vec2::new(rect.width(), hint_h),
            );
            text_center(
                painter,
                hint_rect,
                hint,
                d.font(d.t().caption),
                p.label_caption,
            );
        }
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
        icon.paint(ui.painter(), icon_rect, self.accent(d), 1.7);
        text_left(
            ui.painter(),
            inset(rect, m.s(38.0), 0.0, m.s(14.0), 0.0),
            self.text,
            font,
            d.p().label_primary,
        );
    }
}

/// 悬浮提示（hover 某控件时弹出的小气泡）。
///
/// 教室触控场景用不上 hover，但为了"老师拿着鼠标调试"这条路径
/// 仍保留；它只渲染一次内容，不管理出现/消失时机。
pub struct Tooltip<'a> {
    text: &'a str,
}

impl<'a> Tooltip<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// 在 `pos`（期望的左上角）绘制。
    pub fn show_at(&self, ui: &Ui, d: &Design, pos: egui::Pos2) {
        let m = d.m();
        let font = d.font(d.t().caption);
        let w = ui
            .painter()
            .layout_no_wrap(self.text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
            + m.s(16.0);
        let h = m.s(24.0);
        let rect = Rect::from_min_size(pos, Vec2::new(w, h));
        ui.painter().squircle(
            rect,
            m.s(6.0),
            d.c().tooltip,
            egui::Stroke::new(1.0, crate::base::translucent(d.p().border_l2, 0.4)),
        );
        text_center(ui.painter(), rect, self.text, font, d.p().label_primary);
    }
}
