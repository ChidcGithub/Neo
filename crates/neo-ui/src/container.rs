//! 容器族：卡片、面板、区块。
//!
//! 三者是同一套"表面"语言的三个层级：
//!
//! | 组件 | 用途 | 上游表面 |
//! |---|---|---|
//! | [`Card`] | 输入卡、消息气泡、场景卡 | `specific-input-major` / `specific-bubble` |
//! | [`Panel`] | 浮层面板（设置、菜单） | `bg-layer-*` + `elevation-soft` |
//! | [`Section`] | 面板内的一组设置项 | 无表面，只有分组标题与间距 |
//!
//! 圆角一律走 [`SquirclePaint`] 的超椭圆，与上游 `corner-shape: superellipse(1.5)` 对齐。

use egui::{Rect, Sense, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{inset_all, section_label};
use crate::design::Size;
use crate::Design;

/// 卡片表面类型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CardSurface {
    /// 输入卡表面（`specific-input-major`）。
    #[default]
    Input,
    /// 消息气泡（`specific-bubble`）。
    Bubble,
    /// 场景卡 / 次级区块（`alias-bg-layer-2`）。
    Raised,
    /// 弱底（`specific-tip`）。
    Tip,
}

/// 一张卡片。
///
/// 关键差别在悬停：`interactive()` 打开时会抬升 2pt + 浮出投影 ——
/// 这是上游卡片 hover 的语言，大屏上"这张能点"一眼可辨。
pub struct Card {
    surface: CardSurface,
    radius: Option<f32>,
    interactive: bool,
    selected: bool,
    /// 自定义描边；`None` 则按 surface 取默认。
    stroke_override: Option<egui::Stroke>,
    id_salt: Option<egui::Id>,
}

impl Card {
    pub fn new(surface: CardSurface) -> Self {
        Self {
            surface,
            radius: None,
            interactive: false,
            selected: false,
            stroke_override: None,
            id_salt: None,
        }
    }
    pub fn input() -> Self {
        Self::new(CardSurface::Input)
    }
    pub fn bubble() -> Self {
        Self::new(CardSurface::Bubble)
    }
    pub fn raised() -> Self {
        Self::new(CardSurface::Raised)
    }

    pub fn radius(mut self, r: f32) -> Self {
        self.radius = Some(r);
        self
    }
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }
    pub fn stroke(mut self, s: egui::Stroke) -> Self {
        self.stroke_override = Some(s);
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    /// 只绘制表面（调用方自己安排内容）。
    ///
    /// 返回加工后的矩形（悬停抬升时它会上移）与交互结果。
    pub fn paint(self, ui: &Ui, d: &Design, rect: Rect) -> (Rect, Option<egui::Response>) {
        let p = d.p();
        let m = d.m();
        let painter = ui.painter();

        let mut resp = None;
        let mut lift = 0.0;
        if self.interactive {
            let id = self.id_salt.unwrap_or_else(|| {
                ui.id()
                    .with(("card", rect.left() as i32, rect.top() as i32))
            });
            let r = crate::base::tap(ui, rect, id);
            lift = crate::base::ease(ui, id.with("lift"), if r.hovered() { 1.0 } else { 0.0 });
            if lift > 0.001 {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(16));
            }
            resp = Some(r);
        }

        let draw_rect = rect.translate(egui::vec2(0.0, -m.s(2.0) * lift));
        if lift > 0.01 {
            let mut sh = crate::base::elevation_soft(d);
            sh.color = crate::base::translucent(sh.color, lift);
            painter.add(sh.as_shape(draw_rect, self.radius.unwrap_or(m.s(16.0))));
        }

        let radius = self.radius.unwrap_or_else(|| match self.surface {
            CardSurface::Input => m.radius_card(),
            _ => m.s(16.0),
        });
        let fill = if self.selected {
            p.accent_soft
        } else {
            match self.surface {
                CardSurface::Input => p.input_surface,
                CardSurface::Bubble => p.bubble,
                CardSurface::Raised => p.bg_layer_2,
                CardSurface::Tip => p.tip,
            }
        };
        let stroke = self.stroke_override.unwrap_or_else(|| {
            if self.selected {
                egui::Stroke::new(1.0, p.accent)
            } else {
                egui::Stroke::new(1.0, crate::base::translucent(p.border_l4, lift.max(0.35)))
            }
        });
        painter.squircle(draw_rect, radius, fill, stroke);
        (draw_rect, resp)
    }
}

/// 浮层面板：抬升表面 + 投影 + 描边。
pub struct Panel {
    radius: Option<f32>,
}

impl Panel {
    pub fn new() -> Self {
        Self { radius: None }
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = Some(r);
        self
    }

    /// 绘制面板底，返回内容区（已扣内边距）。
    pub fn paint(&self, ui: &Ui, d: &Design, rect: Rect, pad: f32) -> Rect {
        let p = d.p();
        let m = d.m();
        let radius = self.radius.unwrap_or(m.radius_card());
        let painter = ui.painter();
        painter.add(crate::base::elevation_soft(d).as_shape(rect, radius));
        painter.squircle(
            rect,
            radius,
            p.bg_layer_1,
            egui::Stroke::new(1.0, p.border_l2),
        );
        inset_all(rect, pad)
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

/// 面板内的一组设置项：小标题 + 内容 + 可选说明。
///
/// 用它取代"每个设置项自己算一堆间距"。高度由内容自动决定，
/// 需要精确预留高度时用 [`Section::measure`]。
pub struct Section<'a> {
    title: Option<&'a str>,
    hint: Option<&'a str>,
}

impl<'a> Section<'a> {
    pub fn new() -> Self {
        Self {
            title: None,
            hint: None,
        }
    }
    pub fn title(mut self, t: &'a str) -> Self {
        self.title = Some(t);
        self
    }
    pub fn hint(mut self, h: &'a str) -> Self {
        self.hint = Some(h);
        self
    }

    /// 标题行高度。
    pub fn title_h(d: &Design) -> f32 {
        d.m().s(18.0)
    }
    /// 说明行高度。
    pub fn hint_h(d: &Design) -> f32 {
        d.m().s(16.0)
    }
    /// 标题到控件的间距。
    pub fn gap(d: &Design) -> f32 {
        d.m().s(6.0)
    }
    /// 组与组之间的间距。
    pub fn group_gap(d: &Design) -> f32 {
        d.m().s(18.0)
    }

    /// 画标题（若有），返回标题区矩形。
    pub fn draw_title(&self, ui: &mut Ui, d: &Design, width: f32) -> Option<Rect> {
        let t = self.title?;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, Self::title_h(d)), Sense::hover());
        section_label(ui.painter(), d, rect, t);
        ui.add_space(Self::gap(d));
        Some(rect)
    }

    /// 画说明文字（若有）。
    pub fn draw_hint(&self, ui: &mut Ui, d: &Design, width: f32) {
        let Some(h) = self.hint else { return };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, Self::hint_h(d)), Sense::hover());
        crate::base::text_left(
            ui.painter(),
            rect,
            h,
            d.font(d.t().caption),
            d.p().label_caption,
        );
    }

    /// 预留高度：标题 + 内容高 + 说明 + 组间距。
    pub fn measure(&self, d: &Design, content_h: f32) -> f32 {
        let mut h = content_h + Self::group_gap(d);
        if self.title.is_some() {
            h += Self::title_h(d) + Self::gap(d);
        }
        if self.hint.is_some() {
            h += Self::hint_h(d);
        }
        h
    }
}

impl<'a> Default for Section<'a> {
    fn default() -> Self {
        Self::new()
    }
}

/// 尺寸小工具：把 `Size` 换算成按钮区常用高度。
pub fn control_height(d: &Design, size: Size) -> f32 {
    let m = d.m();
    match size {
        Size::Sm => m.s(28.0),
        Size::Md => m.s(36.0),
        Size::Lg => m.s(44.0),
    }
}
