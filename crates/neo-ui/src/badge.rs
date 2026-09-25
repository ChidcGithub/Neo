//! 标签 / 徽标。
//!
//! 上游的 `markdown-tag` 与状态药丸：小圆角、浅底、caption 字号。
//! 统一在这里，避免"版本徽标""模型名""状态标"各画一套。

use egui::{Rect, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::text_center;
use crate::Design;

/// 徽标的语义色调。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeTone {
    /// 中性（版本号、数量）。
    #[default]
    Neutral,
    /// 业务蓝（强调 / 当前项）。
    Accent,
    /// 成功。
    Success,
    /// 警告。
    Warn,
    /// 错误。
    Danger,
}

/// 一枚徽标。
pub struct Badge<'a> {
    text: &'a str,
    tone: BadgeTone,
    mono: bool,
}

impl<'a> Badge<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            tone: BadgeTone::default(),
            mono: true,
        }
    }
    pub fn tone(mut self, t: BadgeTone) -> Self {
        self.tone = t;
        self
    }
    pub fn prop(mut self) -> Self {
        self.mono = false;
        self
    }

    fn colors(&self, d: &Design) -> (egui::Color32, egui::Color32) {
        let p = d.p();
        let c = d.c();
        match self.tone {
            BadgeTone::Neutral => (p.bg_layer_3, p.label_tertiary),
            BadgeTone::Accent => (p.accent_soft, p.label_primary),
            BadgeTone::Success => (c.success_soft, c.success),
            BadgeTone::Warn => (c.warn_soft, c.warn_label),
            BadgeTone::Danger => (c.error_soft, c.error),
        }
    }

    /// 所需宽度。
    pub fn width(&self, ui: &Ui, d: &Design) -> f32 {
        let font = if self.mono {
            d.font_mono(d.t().caption - d.m().s(1.0))
        } else {
            d.font_bold(d.t().caption - d.m().s(1.0))
        };
        ui.painter()
            .layout_no_wrap(self.text.to_owned(), font, egui::Color32::WHITE)
            .size()
            .x
            + d.m().s(14.0)
    }

    /// 在给定矩形内绘制。
    pub fn show_at(&self, ui: &Ui, d: &Design, rect: Rect) {
        let (fill, fg) = self.colors(d);
        ui.painter()
            .squircle(rect, d.m().s(9.0), fill, egui::Stroke::NONE);
        let font = if self.mono {
            d.font_mono(d.t().caption - d.m().s(1.0))
        } else {
            d.font_bold(d.t().caption - d.m().s(1.0))
        };
        text_center(ui.painter(), rect, self.text, font, fg);
    }

    /// 走布局流。
    pub fn show(self, ui: &mut Ui, d: &Design) -> Rect {
        let h = d.m().s(18.0);
        let w = self.width(ui, d);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), egui::Sense::hover());
        self.show_at(ui, d, rect);
        rect
    }
}
