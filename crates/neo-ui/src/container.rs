//! 容器族：浮层面板。
//!
//! 圆角一律走 [`SquirclePaint`] 的超椭圆，与上游 `corner-shape: superellipse(1.5)` 对齐。

use egui::{Color32, Rect, Ui};
use neo_theme::SquirclePaint;

use crate::base::inset_all;
use crate::Design;

/// 浮层面板：抬升表面 + 投影 + 描边。
///
/// 默认底是层级阶梯的 `surface_2`（卡片 / 面板层）；弹窗 / 菜单这类
/// 最上层表面用 [`Panel::fill`] 换成 `surface_3`。
pub struct Panel {
    radius: Option<f32>,
    fill: Option<Color32>,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            radius: None,
            fill: None,
        }
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = Some(r);
        self
    }
    /// 覆盖默认底色（`surface_2`）。
    pub fn fill(mut self, color: Color32) -> Self {
        self.fill = Some(color);
        self
    }

    /// 绘制面板底，返回内容区（已扣内边距）。
    pub fn paint(&self, ui: &Ui, d: &Design, rect: Rect, pad: f32) -> Rect {
        let p = d.p();
        let m = d.m();
        let radius = self.radius.unwrap_or(m.radius_card());
        let painter = ui.painter();
        painter.add(crate::base::elevation_soft(d).as_shape(rect, radius));
        // 描边走层级 token `muted`（实色弱描边，对 surface_2/3 的对比度
        // 由 neo-theme 的层级契约保底），不再用 alpha 叠加的 border_l2 ——
        // 后者叠在实色表面上对比度随底色漂移，HC 下几乎看不见。
        painter.squircle(
            rect,
            radius,
            self.fill.unwrap_or(p.surface_2),
            egui::Stroke::new(1.0, p.muted),
        );
        inset_all(rect, pad)
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
