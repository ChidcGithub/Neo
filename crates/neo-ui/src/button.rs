//! 按钮族。
//!
//! 对标上游 `--dsw-alias-button-*` token。三种核心变体 + 尺寸档 + 图标，
//! 覆盖界面里绝大多数"点一下做事"的控件：
//!
//! - [`Button::primary`]：主操作（发送 / 确认），上游 `button-primary-fill`
//! - [`Button::elevated`]：次级操作（设置顶栏的圆形钮），`button-elevated-fill`
//! - [`Button::ghost`]：最弱操作（侧栏的「新对话」），无填充仅悬停出底
//! - [`Button::danger`]：危险操作（删除确认），`state-error-*`
//!
//! 全部按钮共用：悬停过渡（0.1s）、按下下压、禁用置灰、触控命中区扩展。

use egui::{Painter, Rect, Response, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{ease, elide, inset, tap, text_center, translucent, State};
use crate::design::Size;
use crate::icons::Icon;
use crate::Design;

/// 按钮变体。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// 主操作（业务蓝），上游 `--dsw-alias-button-info-fill`。
    #[default]
    Primary,
    /// 次级操作（抬升表面），上游 `--dsw-alias-button-elevated-fill`。
    Elevated,
    /// 幽灵（无填充，仅悬停出底）。
    Ghost,
    /// 危险操作，上游 `--dsw-alias-state-error-*`。
    Danger,
    /// 反色（深底浅字 / 浅底深字），上游 `--dsw-alias-button-contrast-fill`。
    Contrast,
}

/// 一个按钮。
///
/// 链式配置 + 一次 [`Button::show`]：
///
/// ```no_run
/// # use neo_ui::{Button, Variant, Design};
/// # fn demo(ui: &mut egui::Ui, d: &Design) {
/// Button::new("删除会话").danger().icon(neo_ui::Icon::Trash).show(ui, d);
/// # }
/// ```
pub struct Button<'a> {
    label: &'a str,
    variant: Variant,
    size: Size,
    icon: Option<Icon>,
    /// 占满分配宽度（用于表单的"整行按钮"）。
    full_width: bool,
    enabled: bool,
    /// 加载态：显示转圈、点击无效。
    loading: bool,
    id_salt: Option<egui::Id>,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            variant: Variant::default(),
            size: Size::Md,
            icon: None,
            full_width: false,
            enabled: true,
            loading: false,
            id_salt: None,
        }
    }

    pub fn variant(mut self, v: Variant) -> Self {
        self.variant = v;
        self
    }
    pub fn primary(self) -> Self {
        self.variant(Variant::Primary)
    }
    pub fn elevated(self) -> Self {
        self.variant(Variant::Elevated)
    }
    pub fn ghost(self) -> Self {
        self.variant(Variant::Ghost)
    }
    pub fn danger(self) -> Self {
        self.variant(Variant::Danger)
    }
    pub fn contrast(self) -> Self {
        self.variant(Variant::Contrast)
    }

    pub fn size(mut self, s: Size) -> Self {
        self.size = s;
        self
    }
    pub fn small(self) -> Self {
        self.size(Size::Sm)
    }
    pub fn large(self) -> Self {
        self.size(Size::Lg)
    }

    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn loading(mut self, on: bool) -> Self {
        self.loading = on;
        self
    }
    /// 让两个相同 label 的按钮共存而不串 id。
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    fn height(&self, d: &Design) -> f32 {
        let m = d.m();
        match self.size {
            Size::Sm => m.s(28.0),
            Size::Md => m.s(36.0),
            Size::Lg => m.s(44.0),
        }
    }

    /// 绘制并返回交互结果。
    pub fn show(self, ui: &mut Ui, d: &Design) -> Response {
        let m = d.m();
        let h = self.height(d);
        let font = d.font_bold(d.t().label);

        // 先量出所需宽度。
        let icon_w = if self.icon.is_some() {
            m.s(16.0) + m.s(6.0)
        } else {
            0.0
        };
        let spinner_w = if self.loading {
            m.s(14.0) + m.s(6.0)
        } else {
            0.0
        };
        let text_w = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x;
        let content_w = icon_w + spinner_w + text_w;
        let width = if self.full_width {
            ui.available_width()
        } else {
            content_w + m.s(16.0) * 2.0
        };

        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
        // 默认 Id 带上位置：只按标签区分时，同一个界面里两个同名按钮
        // （例如两处「保存」）会撞成一个 Id。
        let id = self.id_salt.unwrap_or_else(|| {
            ui.id()
                .with(("btn", self.label, rect.left() as i32, rect.top() as i32))
        });
        let interact_rect = Rect::from_center_size(
            rect.center(),
            Vec2::new(m.hit_target(width), m.hit_target(h)),
        );
        // 禁用 / 加载态：只感知悬停（不注册点击），这样 `clicked()` 天然为 false。
        let actionable = self.enabled && !self.loading;
        let resp = if actionable {
            tap(ui, interact_rect, id)
        } else {
            crate::base::hover_area(ui, interact_rect, id)
        };
        let st = State::of(&resp);
        let st = State {
            enabled: actionable,
            ..st
        };

        // ---- 配色 ----
        let p = d.p();
        let c = d.c();
        let (fill_idle, fill_hover, stroke, label_idle) = match self.variant {
            Variant::Primary => (
                c.btn_info,
                c.btn_info_hover,
                egui::Color32::TRANSPARENT,
                c.on_info,
            ),
            Variant::Elevated => (c.btn_elevated, c.hover_solid, p.border_l2, p.label_primary),
            Variant::Ghost => (
                egui::Color32::TRANSPARENT,
                c.hover,
                egui::Color32::TRANSPARENT,
                p.label_secondary,
            ),
            Variant::Danger => (c.error, c.error, egui::Color32::TRANSPARENT, c.on_danger),
            Variant::Contrast => (c.btn_contrast, c.btn_contrast, p.border_l2, c.on_contrast),
        };
        let (fill_idle, label_idle) = if self.enabled && !self.loading {
            (fill_idle, label_idle)
        } else {
            (c.btn_primary_dimmed, translucent(c.on_primary, 0.6))
        };

        // 悬停叠一层：抬起填充 + 提亮文字。
        let lift = ease(
            ui,
            id.with("lift"),
            if st.hovered && actionable { 1.0 } else { 0.0 },
        );
        let label_color = if st.hovered
            && actionable
            && matches!(self.variant, Variant::Elevated | Variant::Ghost)
        {
            p.label_primary
        } else {
            label_idle
        };

        let radius = m.radius_chip().max(m.s(10.0));
        let painter = ui.painter();
        // 按下时轻微收缩（98%），大屏上"按到了"的反馈。
        let press_scale = if st.pressed && self.enabled {
            0.985
        } else {
            1.0
        };
        let draw_rect = if press_scale != 1.0 {
            let shrink = rect.width() * (1.0 - press_scale) * 0.5;
            inset(
                rect,
                shrink,
                rect.height() * (1.0 - press_scale) * 0.5,
                shrink,
                rect.height() * (1.0 - press_scale) * 0.5,
            )
        } else {
            rect
        };
        painter.squircle_filled(draw_rect, radius, fill_idle);
        if lift > 0.001 {
            painter.squircle_filled(draw_rect, radius, fill_hover.gamma_multiply(lift));
        }
        if stroke != egui::Color32::TRANSPARENT {
            painter.squircle_stroked(draw_rect, radius, egui::Stroke::new(1.0, stroke));
        }

        // ---- 内容：图标 + 文本（或转圈） ----
        let mut cx = draw_rect.center().x - content_w * 0.5;
        if let Some(icon) = self.icon.filter(|_| !self.loading) {
            let ir = Rect::from_center_size(
                egui::pos2(cx + m.s(8.0), draw_rect.center().y),
                Vec2::splat(m.s(16.0)),
            );
            icon.paint(painter, ir, label_color, 1.6);
            cx += icon_w;
        }
        if self.loading {
            let ir = Rect::from_center_size(
                egui::pos2(cx + m.s(7.0), draw_rect.center().y),
                Vec2::splat(m.s(14.0)),
            );
            crate::feedback::Spinner::new().show_painter(painter, d, ir, label_color);
            cx += spinner_w;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
        let text_rect = Rect::from_center_size(
            egui::pos2(cx + text_w * 0.5, draw_rect.center().y),
            Vec2::new(text_w, draw_rect.height()),
        );
        let shown = elide(painter, self.label, &font, text_rect.width() + m.s(4.0));
        text_center(painter, text_rect, &shown, font, label_color);

        resp
    }
}

/// 圆形图标按钮（侧栏动作 / 顶栏控制）。
///
/// 视觉直径沿用上游 28/34px；命中区自动扩到触控下限。
pub struct IconButton {
    icon: Icon,
    style: IconButtonStyle,
    size: Size,
    enabled: bool,
    id_salt: Option<egui::Id>,
}

/// 图标按钮的底。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconButtonStyle {
    /// 透明，悬停出底 —— 侧栏/列表行内动作。
    Ghost,
    /// 抬升底（`bg-layer-2`），顶栏的常态圆形钮。
    #[default]
    Elevated,
    /// 浮动（`button-floating-fill`），hero 右上角的档位钮。
    Floating,
    /// 危险（悬停变红）。
    Danger,
    /// 填充强调色（`button-info-fill`）—— 发送 / 确认这类主圆形动作。
    /// 禁用时填充降到 40% 透明度，与上游 disabled 一致。
    Accent,
    /// 弱填充（`specific-selector`）—— 输入卡工具栏的 `+` 这类"低调但可见"的钮。
    Subtle,
}

impl IconButton {
    pub fn new(icon: Icon) -> Self {
        Self {
            icon,
            style: IconButtonStyle::default(),
            size: Size::Md,
            enabled: true,
            id_salt: None,
        }
    }
    pub fn ghost(mut self) -> Self {
        self.style = IconButtonStyle::Ghost;
        self
    }
    pub fn elevated(mut self) -> Self {
        self.style = IconButtonStyle::Elevated;
        self
    }
    pub fn floating(mut self) -> Self {
        self.style = IconButtonStyle::Floating;
        self
    }
    pub fn danger(mut self) -> Self {
        self.style = IconButtonStyle::Danger;
        self
    }
    pub fn accent(mut self) -> Self {
        self.style = IconButtonStyle::Accent;
        self
    }
    pub fn subtle(mut self) -> Self {
        self.style = IconButtonStyle::Subtle;
        self
    }
    pub fn size(mut self, s: Size) -> Self {
        self.size = s;
        self
    }
    pub fn small(self) -> Self {
        self.size(Size::Sm)
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    fn visual_d(&self, d: &Design) -> f32 {
        match self.size {
            Size::Sm => d.m().s(24.0),
            Size::Md => d.m().s(28.0),
            Size::Lg => d.m().s(34.0),
        }
    }

    /// 按钮内图标的边长。
    ///
    /// **不跟着按钮直径走** —— 上游是「28/34px 圆钮里放 16px 图标」，
    /// 小号 24px 圆钮放 14px（上游图标集里 14/16 两档正是为这两种场景准备的）。
    /// 这也意味着图标**不能铺满**按钮：上游图标的墨迹基本填满自己的 viewBox，
    /// 铺满就会顶到圆边。
    fn icon_d(&self, d: &Design) -> f32 {
        match self.size {
            Size::Sm => d.m().s(14.0),
            Size::Md | Size::Lg => d.m().s(16.0),
        }
    }

    /// 在给定的中心点绘制。
    pub fn show_at(self, ui: &Ui, d: &Design, center: egui::Pos2) -> Response {
        let m = d.m();
        let c = d.c();
        let p = d.p();
        let visual_d = self.visual_d(d);
        let hit = m.hit_target(visual_d);
        let id = self
            .id_salt
            .unwrap_or_else(|| ui.id().with(("ibtn", center.x as i32, center.y as i32)));
        let resp = if self.enabled {
            tap(ui, Rect::from_center_size(center, Vec2::splat(hit)), id)
        } else {
            crate::base::hover_area(ui, Rect::from_center_size(center, Vec2::splat(hit)), id)
        };
        let st = State::of(&resp);

        let (base, hover, glyph_idle, glyph_hover) = match self.style {
            IconButtonStyle::Ghost => (
                egui::Color32::TRANSPARENT,
                c.hover,
                p.label_secondary,
                p.label_primary,
            ),
            IconButtonStyle::Elevated => (
                p.bg_layer_2,
                c.hover_solid,
                p.label_secondary,
                p.label_primary,
            ),
            IconButtonStyle::Floating => (
                c.btn_floating,
                c.btn_floating_hover,
                p.label_secondary,
                p.label_primary,
            ),
            IconButtonStyle::Danger => (
                egui::Color32::TRANSPARENT,
                c.hover_danger,
                p.label_caption,
                c.error,
            ),
            IconButtonStyle::Accent => (c.btn_info, c.btn_info_hover, c.on_info, c.on_info),
            IconButtonStyle::Subtle => {
                (p.selector, c.hover_solid, p.label_primary, p.label_primary)
            }
        };
        let glyph_idle = if self.enabled {
            glyph_idle
        } else {
            translucent(glyph_idle, 0.4)
        };
        let glyph = if st.hovered && self.enabled {
            glyph_hover
        } else {
            glyph_idle
        };
        let fill = if st.hovered && self.enabled {
            hover
        } else if !self.enabled && self.style == IconButtonStyle::Accent {
            // 发送钮不可用时的"淡出的强调色"：形状还在，告诉用户"这里有个按钮"。
            translucent(base, 0.4)
        } else {
            base
        };

        let painter = ui.painter();
        // 正圆：完全圆形元素退出超椭圆（上游 `corner-shape: round`）。
        painter.circle_filled(center, visual_d * 0.5, fill);
        self.icon.paint(
            painter,
            Rect::from_center_size(center, Vec2::splat(self.icon_d(d))),
            glyph,
            1.7,
        );
        resp
    }

    /// 走布局流（分配一个正方形）。
    pub fn show(self, ui: &mut Ui, d: &Design) -> Response {
        let visual_d = self.visual_d(d);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(visual_d), egui::Sense::hover());
        self.show_at(ui, d, rect.center())
    }
}

/// 文本胶囊（Plan / 只读开关 / 模型选择器）。
///
/// 输入卡工具栏里的"点一下切换状态"的小块：静息无底，激活时浮起
/// `nav-active` 底。宽度由 [`Chip::width`] 预先量出，调用方负责摆位。
pub struct Chip<'a> {
    label: &'a str,
    chevron: bool,
    active: bool,
    id_salt: Option<egui::Id>,
}

impl<'a> Chip<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            chevron: false,
            active: false,
            id_salt: None,
        }
    }
    /// 尾部带下拉箭头（模型选择器）。
    pub fn chevron(mut self, on: bool) -> Self {
        self.chevron = on;
        self
    }
    pub fn active(mut self, on: bool) -> Self {
        self.active = on;
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    /// 宽度 = 文本宽 + 左右内边距（+ 右侧箭头位）。
    pub fn width(painter: &Painter, d: &Design, label: &str, chevron: bool) -> f32 {
        let m = d.m();
        let font = d.font_bold(d.t().label);
        let tw = painter
            .layout_no_wrap(label.to_owned(), font, d.p().label_secondary)
            .size()
            .x;
        m.chip_width(tw, chevron)
    }

    /// 在给定矩形内绘制。
    pub fn show_at(self, ui: &Ui, d: &Design, rect: Rect) -> Response {
        let p = d.p();
        let m = d.m();
        let id = self
            .id_salt
            .unwrap_or_else(|| ui.id().with(("chip", self.label, rect.left() as i32)));
        let resp = tap(ui, rect, id);
        let st = State::of(&resp);
        let painter = ui.painter();

        if self.active {
            painter.squircle_filled(rect, m.radius_chip(), p.nav_active);
        } else if st.hovered {
            painter.squircle_filled(rect, m.radius_chip(), p.hover);
        }

        let color = if self.active {
            p.label_primary
        } else {
            p.label_secondary
        };
        let font = d.font_bold(d.t().label);
        let inner = crate::base::inset(
            rect,
            m.chip_pad_x(),
            0.0,
            m.chip_pad_x() + if self.chevron { m.chip_chevron() } else { 0.0 },
            0.0,
        );
        let shown = elide(painter, self.label, &font, inner.width());
        crate::base::text_left(painter, inner, &shown, font, color);

        if self.chevron {
            let c = egui::pos2(rect.right() - m.s(9.0), rect.center().y);
            Icon::ChevronDown.paint(
                painter,
                Rect::from_center_size(c, Vec2::splat(m.s(10.0))),
                p.label_caption,
                m.s(1.4),
            );
        }
        resp
    }
}

/// 分段控件（主题 / 观看距离 / 页签）。
///
/// 上游 `.select` 的横向排布版本：一整条底槽，选中段浮起。
/// 返回值是被点中的下标；组件不改任何状态，由调用方落。
/// 分段控件里第 `index` 个分段的交互 Id。
///
/// **必须按「整组选项」而不是「单个选项文字」区分。**
/// 早先用的是 `(选项文字, 下标)`：设置面板里页签行第 1 项叫「显示」，
/// 「显示思考过程」那一行第 1 项也叫「显示」，两者在同一个 `Ui` 里就撞成了
/// 同一个 Id —— 点一下会**同时**命中两个控件，于是「把思考过程设为显示」
/// 顺手把页签翻到了「显示」页。
///
/// 现在把整组选项拼进 Id：同容器里两组不同的选项天然分开；
/// 两组一字不差的选项则由调用方给 [`Segmented::id_salt`]。
fn seg_id(ui_id: egui::Id, salt: Option<&str>, options: &[&str], index: usize) -> egui::Id {
    let group = match salt {
        Some(s) => std::borrow::Cow::Borrowed(s),
        // 用不可能出现在标签里的分隔符，避免 ["a","b"] 与 ["a\\u{1f}b"] 混为一谈。
        None => std::borrow::Cow::Owned(options.join("\u{1f}")),
    };
    ui_id.with(("seg", group.as_ref(), index))
}

/// 测试用的 Id 探针。
///
/// egui 的 `warn_on_id_clash` 只覆盖"真正的 widget"（`create_widget`），
/// 而组件库是直接用 `ui.interact` 画的，撞车不会被它发现 ——
/// 所以自己记一份，供"渲染真实界面并断言 Id 唯一"的测试使用。
#[cfg(any(test, feature = "probe"))]
pub mod probe {
    use std::cell::RefCell;

    thread_local! {
        static SEGMENT_IDS: RefCell<Vec<egui::Id>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(id: egui::Id) {
        SEGMENT_IDS.with(|v| v.borrow_mut().push(id));
    }

    /// 取走并清空本帧记录到的分段 Id。
    pub fn take() -> Vec<egui::Id> {
        SEGMENT_IDS.with(|v| std::mem::take(&mut *v.borrow_mut()))
    }
}

pub struct Segmented<'a> {
    options: &'a [&'a str],
    selected: usize,
    full_width: bool,
    /// 显式区分同一容器里的多个分段控件（两组**完全一样**的选项时必需）。
    id_salt: Option<&'a str>,
}

impl<'a> Segmented<'a> {
    pub fn new(options: &'a [&'a str], selected: usize) -> Self {
        Self {
            options,
            selected,
            full_width: true,
            id_salt: None,
        }
    }

    /// 给本控件一个显式 Id 前缀。
    ///
    /// 默认已经按**整组选项**区分（见 [`seg_id`]），只有"同一面板里两组选项一字不差"
    /// 才需要它。
    pub fn id_salt(mut self, salt: &'a str) -> Self {
        self.id_salt = Some(salt);
        self
    }
    pub fn fixed_width(mut self, _w: f32) -> Self {
        self.full_width = false;
        self
    }

    pub fn show(self, ui: &mut Ui, d: &Design, width: f32) -> Option<usize> {
        let p = d.p();
        let m = d.m();
        let h = m.s(32.0);
        let w = if self.full_width {
            width.max(ui.available_width())
        } else {
            width
        };
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, h), egui::Sense::hover());
        let painter = ui.painter().clone();

        painter.squircle_filled(rect, m.radius_chip(), d.c().hover);

        let pad = m.s(2.0);
        let inner = inset(rect, pad, pad, pad, pad);
        let seg_w = inner.width() / self.options.len() as f32;
        let mut clicked = None;

        for (i, opt) in self.options.iter().enumerate() {
            let seg = Rect::from_min_size(
                egui::pos2(inner.left() + seg_w * i as f32, inner.top()),
                Vec2::new(seg_w, inner.height()),
            );
            let id = seg_id(ui.id(), self.id_salt, self.options, i);
            #[cfg(any(test, feature = "probe"))]
            probe::record(id);
            let resp = tap(ui, seg, id);
            if resp.clicked() {
                clicked = Some(i);
            }
            let selected = i == self.selected;
            if selected {
                let r = m.radius_chip() - pad;
                painter.squircle_filled(seg, r, p.bg_layer_1);
                if !d.is_dark() {
                    painter.squircle_stroked(seg, r, egui::Stroke::new(1.0, p.border_l2));
                }
            }
            let color = if selected {
                p.label_primary
            } else {
                p.label_secondary
            };
            let font = if selected {
                d.font_bold(d.t().caption)
            } else {
                d.font(d.t().caption)
            };
            text_center(&painter, seg, opt, font, color);
        }
        clicked
    }
}

#[cfg(test)]
mod id_tests {
    use super::*;

    /// **回归测试**：同一面板里两组不同选项，在相同下标上不能拿到同一个 Id。
    ///
    /// 这里的四组选项就是设置面板实际在用的四行（页签 / 主题 / 思考过程 / 观看距离）。
    /// 页签第 1 项与思考过程第 1 项都是「显示」—— 正是线上撞车的那一对。
    #[test]
    fn segment_groups_in_settings_do_not_collide() {
        let base = egui::Id::new("neo-settings-panel");
        let groups: [&[&str]; 4] = [
            &["外观", "显示", "模型", "关于"],
            &["暗色", "亮色"],
            &["隐藏", "显示"],
            &["近距", "教室", "远距"],
        ];
        for (i, a) in groups.iter().enumerate() {
            for b in groups.iter().skip(i + 1) {
                for idx in 0..a.len().min(b.len()) {
                    assert_ne!(
                        seg_id(base, None, a, idx),
                        seg_id(base, None, b, idx),
                        "「{}」与「{}」的第 {idx} 段撞了同一个 Id",
                        a.join("/"),
                        b.join("/")
                    );
                }
            }
        }
    }

    /// 同一组选项、不同下标仍然是不同的 Id（否则段与段之间互相覆盖）。
    #[test]
    fn segments_within_one_group_are_distinct() {
        let base = egui::Id::new("panel");
        let opts = ["隐藏", "显示"];
        assert_ne!(seg_id(base, None, &opts, 0), seg_id(base, None, &opts, 1));
    }

    /// 两组一字不差的选项：默认会撞，给了 salt 就分开 —— 这就是 salt 存在的理由。
    #[test]
    fn identical_option_groups_need_an_explicit_salt() {
        let base = egui::Id::new("panel");
        let opts = ["隐藏", "显示"];
        assert_eq!(seg_id(base, None, &opts, 1), seg_id(base, None, &opts, 1));
        assert_ne!(
            seg_id(base, Some("a"), &opts, 1),
            seg_id(base, Some("b"), &opts, 1)
        );
    }
}
