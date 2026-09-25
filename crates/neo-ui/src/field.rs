//! 表单控件：文本输入、开关、键值行。
//!
//! 造型统一自绘，但输入本身仍交给 egui 的 `TextEdit` —— 输入法（中文候选窗）、
//! 光标、选区、复制粘贴这些不该重造。做法是：把 `TextEdit` 的 frame 关掉、
//! 只留文本层，外面套自己画的容器与聚焦环。

use egui::{Rect, Sense, Ui, Vec2};
use neo_theme::SquirclePaint;

use crate::base::{at, ease, inset, inset_all, text_left};
use crate::Design;

/// 单行文本输入框。
pub struct TextField<'a> {
    value: &'a mut String,
    hint: Option<&'a str>,
    secret: bool,
    icon: Option<crate::icons::Icon>,
    enabled: bool,
    id_salt: Option<egui::Id>,
}

impl<'a> TextField<'a> {
    pub fn new(value: &'a mut String) -> Self {
        Self {
            value,
            hint: None,
            secret: false,
            icon: None,
            enabled: true,
            id_salt: None,
        }
    }
    pub fn hint(mut self, h: &'a str) -> Self {
        self.hint = Some(h);
        self
    }
    pub fn secret(mut self, on: bool) -> Self {
        self.secret = on;
        self
    }
    pub fn icon(mut self, i: crate::icons::Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    /// 控件高度。
    pub fn height(d: &Design) -> f32 {
        d.m().s(36.0)
    }

    pub fn show(self, ui: &mut Ui, d: &Design, width: f32) -> egui::Response {
        let m = d.m();
        let p = d.p();
        let h = Self::height(d);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
        // 默认 Id 带上位置：只按宽度区分时，同一个面板里两个等宽输入框会撞成
        // 一个 Id（共享光标/选区状态）。调用方给了 salt 就用 salt。
        let id = self.id_salt.unwrap_or_else(|| {
            ui.id().with((
                "neo-field",
                width as i32,
                rect.left() as i32,
                rect.top() as i32,
            ))
        });

        // 容器底。
        ui.painter()
            .squircle_filled(rect, m.radius_chip(), p.bg_layer_1);

        let mut text_left_pad = m.s(12.0);
        if let Some(icon) = self.icon {
            let ir = Rect::from_center_size(
                egui::pos2(rect.left() + m.s(14.0), rect.center().y),
                Vec2::splat(m.s(16.0)),
            );
            icon.paint(ui.painter(), ir, p.label_caption, 1.6);
            text_left_pad = m.s(34.0);
        }

        let inner = inset(rect, text_left_pad, 0.0, m.s(10.0), 0.0);
        let font = d.font(d.t().body);
        let edit_id = id.with("edit");

        let resp = at(ui, inner, |ui| {
            let edit = egui::TextEdit::singleline(self.value)
                .id(edit_id)
                .frame(egui::Frame::NONE)
                .font(font.clone())
                .text_color(if self.enabled {
                    p.label_primary
                } else {
                    crate::base::translucent(p.label_primary, 0.5)
                })
                .desired_width(inner.width())
                .margin(egui::Margin::symmetric(
                    0,
                    ((h - d.t().body * 1.4) * 0.5) as i8,
                ));
            let edit = if self.secret {
                edit.password(true)
            } else {
                edit
            };
            let edit = if self.enabled {
                edit
            } else {
                edit.interactive(false)
            };
            ui.add(edit)
        });

        // 聚焦环：0.1s 淡入，与按钮的悬停过渡同一语言。
        let focus = ui.ctx().memory(|mm| mm.focused()) == Some(edit_id);
        let ring = ease(ui, id.with("ring"), if focus { 1.0 } else { 0.0 });
        let border = if ring > 0.01 {
            egui::Stroke::new(
                1.0 + 0.6 * ring,
                crate::base::translucent(d.c().business, 0.4 + 0.6 * ring),
            )
        } else {
            egui::Stroke::new(1.0, p.border_l2)
        };
        ui.painter().squircle_stroked(rect, m.radius_chip(), border);

        // 占位符：空且未聚焦时给一句提示。
        if self.value.is_empty() && !focus {
            if let Some(hint) = self.hint {
                text_left(
                    ui.painter(),
                    inset(rect, text_left_pad, 0.0, m.s(10.0), 0.0),
                    hint,
                    d.font(d.t().body),
                    d.c().placeholder,
                );
            }
        }
        resp
    }
}

/// 开关（布尔设置项）。
///
/// 自绘而非用 egui 的 `Checkbox`：上游用的是轨道 + 滑块的开关造型。
pub struct Switch {
    on: bool,
    enabled: bool,
    id_salt: Option<egui::Id>,
}

impl Switch {
    pub fn new(on: bool) -> Self {
        Self {
            on,
            enabled: true,
            id_salt: None,
        }
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    pub fn id_salt(mut self, s: impl std::hash::Hash) -> Self {
        self.id_salt = Some(crate::base::hash_id(s));
        self
    }

    /// 轨道尺寸。
    pub fn size(d: &Design) -> Vec2 {
        Vec2::new(d.m().s(40.0), d.m().s(22.0))
    }

    /// 在给定矩形内绘制（右对齐时用 `show_at`）。
    pub fn show_at(self, ui: &Ui, d: &Design, rect: Rect) -> egui::Response {
        let m = d.m();
        let c = d.c();
        let p = d.p();
        let id = self
            .id_salt
            .unwrap_or_else(|| ui.id().with(("switch", rect.left() as i32)));
        let hit = Rect::from_center_size(rect.center(), Vec2::splat(m.hit_target(rect.height())));
        let resp = if self.enabled {
            crate::base::tap(ui, hit, id)
        } else {
            crate::base::hover_area(ui, hit, id)
        };
        let st = crate::base::State::of(&resp);

        let track = if st.hovered && self.enabled {
            c.hover_solid
        } else {
            p.bg_layer_3
        };
        let off_track = crate::base::translucent(p.border_l2, 0.6);
        let track_color = if self.on { c.business } else { off_track };
        let _ = track;

        let r = rect.height() * 0.5;
        // 轨道与滑块都用正圆/胶囊：半径≈半边的形状退出超椭圆。
        ui.painter().rect_filled(
            rect,
            r,
            if self.enabled {
                track_color
            } else {
                crate::base::translucent(track_color, 0.5)
            },
        );

        let knob_d = rect.height() - m.s(4.0);
        let knob_x = if self.on {
            rect.right() - m.s(2.0) - knob_d * 0.5
        } else {
            rect.left() + m.s(2.0) + knob_d * 0.5
        };
        ui.painter().circle_filled(
            egui::pos2(knob_x, rect.center().y),
            knob_d * 0.5,
            if self.on {
                p.label_on_accent
            } else {
                p.label_tertiary
            },
        );
        resp
    }

    /// 走布局流。
    pub fn show(self, ui: &mut Ui, d: &Design) -> egui::Response {
        let size = Self::size(d);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        self.show_at(ui, d, rect)
    }
}

/// 「标签 —— 键 —— 值」一行（设置面板里的只读信息行）。
pub struct FieldRow<'a> {
    key: &'a str,
    value: &'a str,
    /// 值用等宽字体（路径、版本号、倍率）。
    mono: bool,
}

impl<'a> FieldRow<'a> {
    pub fn new(key: &'a str, value: &'a str) -> Self {
        Self {
            key,
            value,
            mono: true,
        }
    }
    pub fn prop(mut self) -> Self {
        self.mono = false;
        self
    }

    pub fn height(d: &Design) -> f32 {
        d.m().s(22.0)
    }

    pub fn show(self, ui: &mut Ui, d: &Design, width: f32) {
        let h = Self::height(d);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
        response.on_hover_text(format!("{}：{}", self.key, self.value));
        let p = d.p();
        let key_font = d.font(d.t().caption);
        let key_w = ui
            .painter()
            .layout_no_wrap(self.key.into(), key_font.clone(), p.label_caption)
            .size()
            .x
            .min(width * 0.4);
        let key_rect = Rect::from_min_size(rect.min, Vec2::new(key_w, h));
        let key = crate::base::elide(ui.painter(), self.key, &key_font, key_w);
        text_left(ui.painter(), key_rect, &key, key_font, p.label_caption);
        let font = if self.mono {
            d.font_mono(d.t().caption)
        } else {
            d.font(d.t().caption)
        };
        let value_rect = Rect::from_min_max(
            egui::pos2(
                (key_rect.right() + d.m().s(16.0)).min(rect.right()),
                rect.top(),
            ),
            rect.max,
        );
        let shown = crate::base::elide(ui.painter(), self.value, &font, value_rect.width());
        crate::base::text_right(ui.painter(), value_rect, &shown, font, p.label_secondary);
    }
}

/// 一行文本提示（caption 色，用于表单说明）。
pub fn hint_row(ui: &mut Ui, d: &Design, width: f32, text: &str) {
    let h = d.m().s(16.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let font = d.font(d.t().caption);
    let shown = crate::base::elide(ui.painter(), text, &font, width);
    response.on_hover_text(text);
    text_left(ui.painter(), rect, &shown, font, d.p().label_caption);
}

/// 「标签 + 控件」一行的通用容器：左侧标签，右侧控件区。
///
/// 返回右侧控件区的矩形，调用方自己在里面放 `Switch` / 按钮。
pub fn labeled_row(ui: &mut Ui, d: &Design, width: f32, label: &str, control_w: f32) -> Rect {
    let h = d.m().s(34.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    text_left(
        ui.painter(),
        rect,
        label,
        d.font(d.t().label),
        d.p().label_primary,
    );
    Rect::from_min_size(
        egui::pos2(
            rect.right() - control_w,
            rect.top() + (h - d.m().s(22.0)) * 0.5,
        ),
        Vec2::new(control_w, d.m().s(22.0)),
    )
}

/// 输入框外层容器（无 `TextEdit`，供自定义内容使用）。
pub fn field_frame(ui: &Ui, d: &Design, rect: Rect, focused: bool) {
    let m = d.m();
    let p = d.p();
    ui.painter()
        .squircle_filled(rect, m.radius_chip(), p.bg_layer_1);
    let border = if focused {
        egui::Stroke::new(1.6, d.c().business)
    } else {
        egui::Stroke::new(1.0, p.border_l2)
    };
    ui.painter().squircle_stroked(rect, m.radius_chip(), border);
    let _ = inset_all(rect, 0.0);
}
