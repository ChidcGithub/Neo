//! CommonMark/GFM message rendering, backed by egui_commonmark.
//!
//! The local upstream compatibility patch constrains tables, retains column
//! alignment and prevents model-authored links/images from invoking local files.
//! There is no lossy intermediate block AST or zero-width formula placeholder.

use std::sync::{Arc, Mutex};

use egui::{Id, RichText, TextStyle, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use super::{math, Skin};

/// Cache syntax definitions across messages/frames, but never cache static message
/// geometry: streamed text, width, theme and scale can all change independently.
pub fn render(ui: &mut Ui, skin: &Skin<'_>, text: &str, streaming: bool) {
    let cache = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_insert_with(Id::new("neo-commonmark-cache"), || {
            Arc::new(Mutex::new(CommonMarkCache::default()))
        })
        .clone()
    });
    // Do not hold a Context data lock while the renderer calls back into egui.
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    ui.scope(|ui| {
        configure(ui, skin);
        let body = skin.t().body;
        let color = skin.p().label_primary;
        let math_fn = move |ui: &mut Ui, latex: &str, inline: bool| {
            let size = body * if inline { 1.0 } else { 1.12 };
            if inline {
                draw_math(ui, latex, size, false, color);
            } else {
                // A display formula is a separate block, not an inline widget.
                ui.label("\n");
                ui.vertical(|ui| {
                    ui.add_space((body * 0.3).ceil());
                    draw_math(ui, latex, size, true, color);
                    ui.add_space((body * 0.3).ceil());
                    let mut aligned = ui.min_rect();
                    aligned.max.y = aligned.max.y.ceil();
                    ui.expand_to_include_rect(aligned);
                });
                ui.label("\n");
            }
        };
        CommonMarkViewer::new()
            .explicit_image_uri_scheme(true)
            // 消息间共享一个 cache：开着锚点滚动时，每条消息的 deferred 目标会
            // 互相覆盖，非末尾消息里的 #anchor 点了永不跳转。聊天内容几乎不做
            // 页内跳转，关掉比按消息分 cache 简单得多。
            .enable_scroll_to_heading(false)
            .render_math_fn(Some(&math_fn))
            .show(ui, &mut cache, text);
        if streaming {
            ui.label(RichText::new("▍").color(skin.p().accent));
        }
    });
}

fn configure(ui: &mut Ui, skin: &Skin<'_>) {
    let p = skin.p();
    let body = skin.t().body;
    let style = ui.style_mut();
    style.override_font_id = None;
    style.override_text_style = None;
    style.text_styles.insert(TextStyle::Body, skin.prop(body));
    style.text_styles.insert(TextStyle::Button, skin.prop(body));
    style
        .text_styles
        .insert(TextStyle::Heading, skin.bold(body * 1.45));
    style
        .text_styles
        .insert(TextStyle::Small, skin.prop(body * 0.82));
    style
        .text_styles
        .insert(TextStyle::Monospace, skin.mono(body * 0.93));
    style.visuals.override_text_color = Some(p.label_primary);
    style.visuals.weak_text_color = Some(p.label_secondary);
    style.visuals.hyperlink_color = p.accent;
    style.visuals.code_bg_color = p.components().code_inline;
    style.visuals.extreme_bg_color = p.components().code_block;
    style.visuals.widgets.noninteractive.fg_stroke.color = p.label_primary;
    // 节奏：段距 0.42×body（比 egui 默认的 0.32 略开），行间 1.45× ——
    // 课堂大屏远距离阅读，宁松勿挤。
    style.spacing.item_spacing.y = body * 0.42;
    style.spacing.interact_size.y = body * 1.45;
    style.spacing.icon_width = body;
    style.spacing.icon_width_inner = body * 0.65;
    style.url_in_tooltip = true;
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
}

fn draw_math(ui: &mut Ui, latex: &str, size: f32, display: bool, color: egui::Color32) {
    let Some(measured) = math::measure(latex, size, display) else {
        ui.label(
            RichText::new(if display {
                format!("$${latex}$$")
            } else {
                format!("${latex}$")
            })
            .code(),
        )
        .on_hover_text("公式尚未完整或语法不受支持，保留原文");
        return;
    };
    // Formula widgets reserve their actual width AND height. Oversized formulas
    // get a local horizontal viewport instead of covering following text.
    if measured.x > ui.available_width() {
        egui::ScrollArea::horizontal()
            .max_width(ui.available_width())
            .auto_shrink([false, true])
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                let _ = math::render(ui, color, latex, size, display);
            });
    } else {
        let _ = math::render(ui, color, latex, size, display);
    }
}

#[cfg(test)]
#[path = "markdown_tests.rs"]
mod tests;
