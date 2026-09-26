//! 反馈类：加载与浮动提示。
//!
//! 状态语言的统一出处："生成中"是转圈，"出错了"是错误色的 toast ——
//! 不再每次手搓一组脉冲点或一段红字。
//!
//! toast 的**队列与生命周期**由 [`crate::toasts`] 承担（以 egui-toast 为底的
//! vendor 版，锚定 / 堆叠 / 到期回收都是它的），组件库只管两件事：Neo 外观的
//! 自绘内容（[`toast`]），和一只配好锚点与外观的队列工厂（[`toasts`]）。

use egui::{Color32, Painter, Rect, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{inset, text_left, translucent};
use crate::icons::Icon;
use crate::toasts::{Toast, ToastKind, Toasts};
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
/// "已复制""已保存"这类一闪而过的确认。队列、堆叠、到期回收由
/// egui-toast 承担；这里的三个函数是组件库侧的接口：
///
/// - [`toast`]：布局流里画一条（队列的 custom_contents 走这里）；
/// - [`toast_at`]：指定中心点画一条（陈列室摆位用）；
/// - [`toasts`]：拿一只配好 Neo 外观与锚点的队列，每帧重建即可
///   （队列本体存在 egui memory，不丢）。
fn toast_accent(d: &Design, kind: ToastKind) -> Color32 {
    let c = d.c();
    match kind {
        ToastKind::Success => c.success,
        ToastKind::Warning => c.warn,
        ToastKind::Error => c.error,
        ToastKind::Info | ToastKind::Custom(_) => d.p().label_secondary,
    }
}

fn toast_icon(kind: ToastKind) -> Icon {
    match kind {
        ToastKind::Success => Icon::Check,
        ToastKind::Warning => Icon::Warn,
        ToastKind::Error => Icon::Close,
        ToastKind::Info | ToastKind::Custom(_) => Icon::Info,
    }
}

/// 一条 toast 的自绘尺寸：图标 + 间距 + 文字 + 左右内边距。
fn toast_size(ui: &Ui, d: &Design, text: &str) -> Vec2 {
    let m = d.m();
    let font = d.font(d.t().label);
    let text_w = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x;
    Vec2::new(text_w + m.s(16.0) + m.s(8.0) + m.s(28.0), m.s(40.0))
}

/// 在 rect 内画一条 toast（elevation + squircle 底 + 图标 + 文字）。
fn paint_toast(ui: &Ui, d: &Design, kind: ToastKind, text: &str, rect: Rect) {
    let m = d.m();
    let c = d.c();
    ui.painter()
        .add(crate::base::elevation_soft(d).as_shape(rect, m.s(20.0)));
    ui.painter().squircle(
        rect,
        m.s(20.0),
        c.toast,
        egui::Stroke::new(1.0, translucent(d.p().border_l2, 0.4)),
    );

    let icon_d = m.s(16.0);
    let icon_rect = Rect::from_center_size(
        egui::pos2(rect.left() + m.s(14.0) + icon_d * 0.5, rect.center().y),
        Vec2::splat(icon_d),
    );
    toast_icon(kind).paint(ui.painter(), icon_rect, toast_accent(d, kind));
    text_left(
        ui.painter(),
        inset(rect, m.s(38.0), 0.0, m.s(14.0), 0.0),
        text,
        d.font(d.t().label),
        d.p().label_primary,
    );
}

/// 单条 toast（布局流）。toast 是纯提示：只感知悬停，不拦截点击。
pub fn toast(ui: &mut Ui, d: &Design, kind: ToastKind, text: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(toast_size(ui, d, text), egui::Sense::hover());
    paint_toast(ui, d, kind, text, rect);
    resp
}

/// 单条 toast（指定中心点，陈列室摆位用）。
pub fn toast_at(ui: &Ui, d: &Design, kind: ToastKind, text: &str, center: egui::Pos2) {
    let rect = Rect::from_center_size(center, toast_size(ui, d, text));
    paint_toast(ui, d, kind, text, rect);
}

/// 一只配好 Neo 外观的 toast 队列：屏幕下方中央、向上堆叠、最顶层。
/// 每帧重建实例即可 —— 队列本体在 egui memory 里。
///
/// 提示层不拦截点击（见 [`crate::toasts`] 的模块说明）。
pub fn toasts(d: &Design) -> Toasts {
    let offset_y = -d.m().s(150.0);
    let d = *d;
    Toasts::new(move |ui: &mut Ui, t: &Toast| {
        toast(ui, &d, t.kind, &t.text);
    })
    .anchor(egui::Align2::CENTER_BOTTOM, egui::pos2(0.0, offset_y))
    .direction(egui::Direction::BottomUp)
}
