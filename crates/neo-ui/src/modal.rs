//! 模态对话框。
//!
//! 上游的遮罩分四级（`bg-mask-1/2/3`、`bg-drop`），模态用最重的
//! `bg-mask-1`。这里把"遮罩 + 阻塞点击 + 居中卡片 + 标题栏 + Esc 关闭"
//! 一次封好，调用方只写：

use egui::{Key, Rect, Ui, Vec2};

use crate::base::{at, text_left};
use crate::button::IconButton;
use crate::container::Panel;
use crate::design::Size;
use crate::icons::Icon;
use crate::Design;

/// 模态的尺寸档。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ModalSize {
    /// 确认框（约 420pt 宽）。
    Sm,
    /// 表单 / 设置（约 560pt 宽）。
    #[default]
    Md,
    /// 宽面板（约 760pt 宽）。
    Lg,
}

impl ModalSize {
    fn base_width(self) -> f32 {
        match self {
            ModalSize::Sm => 420.0,
            ModalSize::Md => 560.0,
            ModalSize::Lg => 760.0,
        }
    }

    /// 实际宽度（按当前缩放放大，并限制在视口的 92% 内）。
    pub fn width(self, d: &Design, viewport_w: f32) -> f32 {
        d.m().s(self.base_width()).min(viewport_w * 0.92)
    }
}

/// 一个模态对话框。
///
/// 用法（一帧内）：
///
/// ```no_run
/// # use neo_ui::{Modal, ModalSize, Design};
/// # fn demo(ui: &mut egui::Ui, d: &Design, open: &mut bool, content_h: f32) {
/// // 1. 先问高度（用于居中定位，也用于自校验）
/// let h = Modal::height(d, ModalSize::Md, content_h);
/// // 2. 画底（遮罩 + 卡片），拿到内容区
/// let modal = Modal::new("设置", ModalSize::Md);
/// if let Some(body) = modal.begin(ui, d, h) {
///     // 3. 在 body 里画内容；返回 true 表示请求关闭
///     if modal.end(ui, d, body) { *open = false; }
/// }
/// # }
/// ```
pub struct Modal<'a> {
    title: &'a str,
    size: ModalSize,
    /// 显示右上角关闭钮。
    closable: bool,
    /// 遮罩不透明度系数（1.0 = 用 token 原值）。
    dim: f32,
}

/// 模态的布局节奏。
#[derive(Clone, Copy)]
pub struct ModalRhythm {
    pub pad: f32,
    pub title_h: f32,
    pub gap_after_title: f32,
    pub bottom_pad: f32,
}

impl ModalRhythm {
    pub fn new(d: &Design) -> Self {
        let m = d.m();
        Self {
            pad: m.s(20.0),
            title_h: m.s(26.0),
            gap_after_title: m.s(14.0),
            bottom_pad: m.s(20.0),
        }
    }
    /// 除内容外的固定高度。
    pub fn chrome(&self) -> f32 {
        self.pad + self.title_h + self.gap_after_title + self.bottom_pad
    }
}

impl<'a> Modal<'a> {
    pub fn new(title: &'a str, size: ModalSize) -> Self {
        Self {
            title,
            size,
            closable: true,
            dim: 1.0,
        }
    }
    pub fn closable(mut self, on: bool) -> Self {
        self.closable = on;
        self
    }
    pub fn dim(mut self, k: f32) -> Self {
        self.dim = k;
        self
    }

    /// 给定内容高度，算出面板总高（须在 [`Modal::begin`] 之前调用）。
    pub fn height(d: &Design, size: ModalSize, content_h: f32) -> f32 {
        let _ = size;
        ModalRhythm::new(d).chrome() + content_h
    }

    /// 画遮罩与卡片，返回内容区。
    ///
    /// 返回 `None` 表示本帧不显示（调用方自行决定是否提前 return）。
    pub fn begin(&self, ui: &mut Ui, d: &Design, height: f32) -> Option<Rect> {
        let screen = ui.ctx().content_rect();
        let m = d.m();

        // 阻塞下层交互：先占住整屏。
        ui.interact(
            screen,
            egui::Id::new(("neo-modal-blocker", self.title)),
            egui::Sense::click(),
        );

        // 遮罩。
        let c = d.c();
        let mask = crate::base::translucent(c.mask_modal, self.dim);
        ui.painter().rect_filled(screen, 0.0, mask);

        let w = self.size.width(d, screen.width());
        let rect = Rect::from_center_size(screen.center(), Vec2::new(w, height));
        let panel = Panel::new();
        let body = panel.paint(ui, d, rect, ModalRhythm::new(d).pad);

        let r = ModalRhythm::new(d);
        // 标题栏。
        let (title_rect, _) =
            ui.allocate_exact_size(Vec2::new(body.width(), r.title_h), egui::Sense::hover());
        // 标题区可能落在 panel 之外（alloc 走的是父级游标）—— 用绝对坐标重画。
        let title_abs = Rect::from_min_size(body.min, Vec2::new(body.width(), r.title_h));
        ui.painter()
            .rect_filled(title_rect, 0.0, egui::Color32::TRANSPARENT);
        text_left(
            ui.painter(),
            title_abs,
            self.title,
            d.font_bold(d.t().label + m.s(2.0)),
            d.p().label_primary,
        );

        if self.closable {
            let close_d = m.s(24.0);
            let center = egui::pos2(title_abs.right() - close_d * 0.5, title_abs.center().y);
            let resp = IconButton::new(Icon::Close)
                .ghost()
                .size(Size::Md)
                .id_salt(("neo-modal-close", self.title))
                .show_at(ui, d, center);
            if resp.clicked() {
                ui.memory_mut(|mem| mem.data.insert_temp(close_flag(self.title), true));
            }
        }

        // 内容区（标题栏之下）。
        let content = Rect::from_min_max(
            egui::pos2(body.left(), title_abs.bottom() + r.gap_after_title),
            body.max,
        );
        Some(content)
    }

    /// 收尾：处理 Esc / 关闭钮，返回是否请求关闭。
    pub fn end(&self, ui: &mut Ui, d: &Design, body: Rect) -> bool {
        let _ = d;
        let mut close = false;
        // Esc
        if ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            close = true;
        }
        // 关闭钮（在 begin 里置的标志）
        if ui
            .memory(|m| m.data.get_temp::<bool>(close_flag(self.title)))
            .unwrap_or(false)
        {
            ui.memory_mut(|m| m.data.remove::<bool>(close_flag(self.title)));
            close = true;
        }
        // 自校验：内容不得溢出预留高度。
        debug_assert!(
            ui.min_rect().height() <= body.height() + 1.0,
            "模态内容溢出：实际 {:.1}pt，预留 {:.1}pt",
            ui.min_rect().height(),
            body.height()
        );
        close
    }
}

fn close_flag(title: &str) -> egui::Id {
    egui::Id::new(("neo-modal-close-flag", title))
}

/// 简单确认框：标题 + 正文 + 取消/确认。
///
/// 覆盖"删除会话""放弃修改"这类最高频的确认场景，不必每次手搓。
pub struct Confirm<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub confirm: &'a str,
    pub cancel: &'a str,
    /// 确认键是否用危险色。
    pub danger: bool,
}

impl<'a> Confirm<'a> {
    pub fn new(title: &'a str, body: &'a str) -> Self {
        Self {
            title,
            body,
            confirm: "确定",
            cancel: "取消",
            danger: false,
        }
    }
    pub fn danger(mut self, on: bool) -> Self {
        self.danger = on;
        self
    }
    pub fn labels(mut self, confirm: &'a str, cancel: &'a str) -> Self {
        self.confirm = confirm;
        self.cancel = cancel;
        self
    }

    /// 对话框高度（正文按宽度自动换行，需先算行数）。
    pub fn height(d: &Design, body: &str, width: f32) -> f32 {
        let m = d.m();
        let lines = estimate_lines(body, width - m.s(40.0), d.t().body);
        Modal::height(
            d,
            ModalSize::Sm,
            lines * d.t().body * 1.6 + m.s(12.0) + m.s(36.0),
        )
    }

    /// 绘制一整个确认框。返回 `Some(true)` 确认、`Some(false)` 取消、`None` 未决。
    pub fn show(&self, ui: &mut Ui, d: &Design, width: f32) -> Option<bool> {
        let m = d.m();
        let h = Self::height(d, self.body, width);
        let modal = Modal::new(self.title, ModalSize::Sm).closable(false);
        let body_rect = modal.begin(ui, d, h)?;

        let lines = estimate_lines(self.body, body_rect.width(), d.t().body);
        let text_h = lines * d.t().body * 1.6;
        let text_rect = Rect::from_min_size(body_rect.min, Vec2::new(body_rect.width(), text_h));
        let galley = ui.painter().layout(
            self.body.to_owned(),
            d.font(d.t().body),
            d.p().label_secondary,
            text_rect.width(),
        );
        ui.painter()
            .galley(text_rect.min, galley, d.p().label_secondary);

        // 底部按钮：右对齐 [取消][确认]。
        let btn_h = m.s(36.0);
        let btn_y = body_rect.bottom() - btn_h;
        let confirm_font = d.font_bold(d.t().label);
        let cw_confirm = ui
            .painter()
            .layout_no_wrap(self.confirm.to_owned(), confirm_font, egui::Color32::WHITE)
            .size()
            .x
            + m.s(32.0);
        let cw_cancel = ui
            .painter()
            .layout_no_wrap(
                self.cancel.to_owned(),
                d.font_bold(d.t().label),
                egui::Color32::WHITE,
            )
            .size()
            .x
            + m.s(32.0);

        let mut result = None;
        let confirm_rect = Rect::from_min_size(
            egui::pos2(body_rect.right() - cw_confirm, btn_y),
            Vec2::new(cw_confirm, btn_h),
        );
        let cancel_rect = Rect::from_min_size(
            egui::pos2(confirm_rect.left() - m.s(8.0) - cw_cancel, btn_y),
            Vec2::new(cw_cancel, btn_h),
        );
        at(ui, cancel_rect, |ui| {
            let b = crate::button::Button::new(self.cancel).ghost().full_width();
            if b.show(ui, d).clicked() {
                result = Some(false);
            }
        });
        at(ui, confirm_rect, |ui| {
            let b = if self.danger {
                crate::button::Button::new(self.confirm)
                    .danger()
                    .full_width()
            } else {
                crate::button::Button::new(self.confirm)
                    .primary()
                    .full_width()
            };
            if b.show(ui, d).clicked() {
                result = Some(true);
            }
        });

        if modal.end(ui, d, body_rect) && result.is_none() {
            result = Some(false);
        }
        result
    }
}

/// 估算换行后的行数（中英混排按 1 字 ≈ 1 字号宽）。
fn estimate_lines(text: &str, width: f32, font_size: f32) -> f32 {
    if width <= 0.0 || font_size <= 0.0 {
        return 1.0;
    }
    let per_line = (width / font_size).floor().max(1.0);
    // 按显式换行切，再按宽度折算。
    text.split('\n')
        .map(|seg| ((seg.chars().count() as f32 / per_line).ceil()).max(1.0))
        .sum::<f32>()
        .max(1.0)
}
