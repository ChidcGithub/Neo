//! 工具调用的权限确认弹窗。
//!
//! 标题和操作区固定，工具元信息、动作预览及完整 JSON 在中间独立滚动。
//! 不截断待授权内容，也不依赖 JSON 行数估计实际换行高度。

use egui::{Rect, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::{Button, Panel};

use super::Skin;
use crate::state::ToolMeta;

/// 用户对一次待确认调用的答复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// 只批准这一次。
    Once,
    /// 本会话后续工具都不再询问（与 AppState::auto_approve_tools 一致）。
    Always,
    /// 拒绝；结果会回灌给模型。
    Deny,
}

/// 固定区几何，正文高度只影响面板总高，不能挤走操作区。
#[derive(Clone, Copy, Debug)]
struct Geometry {
    panel: Rect,
    title: Rect,
    body: Rect,
    footer: Rect,
}

impl Geometry {
    fn new(panel: Rect, pad: f32, title_h: f32, footer_h: f32, gap: f32) -> Self {
        let inner = panel.shrink(pad);
        let title = Rect::from_min_size(inner.min, Vec2::new(inner.width(), title_h));
        let footer = Rect::from_min_max(
            egui::pos2(inner.left(), inner.bottom() - footer_h),
            inner.max,
        );
        let body = Rect::from_min_max(
            egui::pos2(inner.left(), title.bottom() + gap),
            egui::pos2(inner.right(), footer.top() - gap),
        );
        debug_assert!(panel.contains_rect(title));
        debug_assert!(panel.contains_rect(footer));
        debug_assert!(body.height() > 0.0);
        Self {
            panel,
            title,
            body,
            footer,
        }
    }
}

/// 弹窗遮罩淡入时长：spec 标准档（0.2s）。
const ENTER_FADE: f32 = 0.2;

/// 画确认弹窗。只有点击明确的授权/拒绝按钮才返回答复；点遮罩不会批准。
pub fn confirm(ui: &mut Ui, skin: &Skin<'_>, meta: &ToolMeta, remaining: usize) -> Option<Answer> {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let screen = ui.ctx().content_rect();
    let pad = m.s(22.0);
    let gap = m.s(12.0);
    let width = m.s(520.0).min((screen.width() - m.s(32.0)).max(1.0));
    let inner_w = (width - pad * 2.0).max(1.0);
    // 始终给正文预留滚动条宽度，测量宽度与最终绘制宽度一致。
    let scroll_w = m.s(16.0);
    let body_w = (inner_w - scroll_w).max(1.0);
    let title = ui.painter().layout(
        "需要你的许可".into(),
        skin.bold(skin.t().headline),
        p.label_primary,
        inner_w,
    );
    let risk = match meta.risk {
        "exec" => "执行命令",
        "write" => "修改文件",
        "open" => "打开内容",
        "read" => "读取内容",
        other => other,
    };
    let mut badge = format!("{} · {} · {}", meta.title, meta.name, risk);
    if remaining > 1 {
        badge.push_str(&format!("\n另有 {} 个调用等待确认", remaining - 1));
    }
    let badge = ui
        .painter()
        .layout(badge, skin.prop(skin.t().caption), p.label_caption, body_w);
    let preview_font = if meta.risk == "exec" {
        skin.mono(skin.t().body)
    } else {
        skin.prop(skin.t().body)
    };
    let preview = ui.painter().layout(
        meta.preview.clone(),
        preview_font,
        p.label_primary,
        (body_w - m.s(20.0)).max(1.0),
    );
    let args_label = ui.painter().layout_no_wrap(
        "完整参数".into(),
        skin.prop(skin.t().caption),
        p.label_caption,
    );
    let args = ui.painter().layout(
        serde_json::to_string_pretty(&meta.args).unwrap_or_else(|_| meta.args.to_string()),
        skin.mono(skin.t().caption),
        p.label_secondary,
        (body_w - m.s(20.0)).max(1.0),
    );
    let preview_h = preview.size().y + m.s(16.0);
    let args_h = args.size().y + m.s(16.0);
    let body_h = badge.size().y + gap + preview_h + gap + args_label.size().y + m.s(6.0) + args_h;
    let button_h = m.s(36.0);
    let labels = ["允许", "本会话都允许", "拒绝"];
    let button_width: f32 = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap((*label).into(), d.font_bold(d.t().label), p.label_primary)
                .size()
                .x
                + m.s(32.0)
        })
        .sum::<f32>()
        + gap * 2.0;
    let stacked = button_width > inner_w;
    let footer_h = if stacked {
        3.0 * button_h + 2.0 * gap
    } else {
        button_h
    };
    let fixed_h = 2.0 * pad + title.size().y + 2.0 * gap + footer_h;
    let height = (fixed_h + body_h).min((screen.height() - m.s(32.0)).max(fixed_h + 1.0));
    let ctx = ui.ctx().clone();
    // 弹出淡入：身份随 call_id。egui 动画首调直接返回目标值，所以第一次
    // 见到这个弹窗时先播种 0 再启动到 1，并用一条 temp 标记记住「见过」。
    // 只有遮罩随 k 淡入：面板内部是预排版的 galley，缩放/平移都会溢出。
    let enter_id = egui::Id::new(("neo-tool-enter", &meta.call_id));
    let seen = ctx.memory(|m| m.data.get_temp::<bool>(enter_id).unwrap_or(false));
    let enter_k = if seen {
        ctx.animate_value_with_time(enter_id, 1.0, ENTER_FADE)
    } else {
        ctx.animate_value_with_time(enter_id, 0.0, ENTER_FADE);
        let k = ctx.animate_value_with_time(enter_id, 1.0, ENTER_FADE);
        ctx.memory_mut(|m| m.data.insert_temp(enter_id, true));
        k
    };
    let response = egui::Modal::new(egui::Id::new(("neo-tool-confirm", &meta.call_id)))
        .frame(egui::Frame::NONE)
        .backdrop_color(d.c().mask_modal.gamma_multiply(enter_k))
        .show(&ctx, |ui| {
            let (panel, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
            Panel::new().paint(ui, &d, panel, pad);
            let g = Geometry::new(panel, pad, title.size().y, footer_h, gap);
            ui.painter().galley(g.title.min, title, p.label_primary);
            super::at(ui, g.body, |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(g.body));
                let scroll = egui::ScrollArea::vertical()
                    .id_salt(("neo-confirm-body", &meta.call_id))
                    .auto_shrink([false, false])
                    .max_height(g.body.height())
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        ui.set_width(body_w);
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::new(body_w, badge.size().y),
                            egui::Sense::hover(),
                        );
                        ui.painter().galley(r.min, badge, p.label_caption);
                        ui.add_space(gap);
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::new(body_w, preview_h),
                            egui::Sense::hover(),
                        );
                        ui.painter().squircle_filled(r, m.s(10.0), p.bg_layer_1);
                        ui.painter().galley(
                            r.min + egui::vec2(m.s(10.0), m.s(8.0)),
                            preview,
                            p.label_primary,
                        );
                        ui.add_space(gap);
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::new(body_w, args_label.size().y),
                            egui::Sense::hover(),
                        );
                        ui.painter().galley(r.min, args_label, p.label_caption);
                        ui.add_space(m.s(6.0));
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::new(body_w, args_h), egui::Sense::hover());
                        ui.painter().squircle_filled(r, m.s(8.0), p.bg_layer_1);
                        ui.painter().galley(
                            r.min + egui::vec2(m.s(10.0), m.s(8.0)),
                            args,
                            p.label_secondary,
                        );
                    });
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("neo-confirm-scroll-probe"),
                        (
                            scroll.inner_rect,
                            scroll.state.offset.y,
                            scroll.content_size.y,
                        ),
                    )
                });
                let _ = scroll;
            });
            let mut out = None;
            super::at(ui, g.footer, |ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(gap);
                let layout = if stacked {
                    egui::Layout::top_down(egui::Align::Max)
                } else {
                    egui::Layout::right_to_left(egui::Align::Center)
                };
                ui.with_layout(layout, |ui| {
                    for (button, answer) in [
                        (Button::new("允许").primary(), Answer::Once),
                        (Button::new("本会话都允许").elevated(), Answer::Always),
                        (Button::new("拒绝").ghost(), Answer::Deny),
                    ] {
                        let response = button.show(ui, &d);
                        debug_assert!(g.panel.expand(0.5).contains_rect(response.rect));
                        if response.clicked() {
                            out = Some(answer);
                        }
                    }
                });
            });
            out
        });
    response.inner
}

#[cfg(test)]
mod tests {
    use super::Geometry;
    use egui::{pos2, Rect, Vec2};

    #[test]
    fn fixed_regions_stay_inside_panel_and_do_not_overlap() {
        for scale in [1.0, 1.25, 1.6, 2.5] {
            for body_h in [80.0, 240.0, 600.0] {
                let pad = 22.0 * scale;
                let title = 34.0 * scale;
                let footer = 36.0 * scale;
                let gap = 12.0 * scale;
                let panel = Rect::from_min_size(
                    pos2(15.0, 25.0),
                    Vec2::new(
                        520.0 * scale,
                        2.0 * pad + title + footer + 2.0 * gap + body_h,
                    ),
                );
                let g = Geometry::new(panel, pad, title, footer, gap);
                assert!(panel.contains_rect(g.body));
                assert!(g.title.bottom() < g.body.top());
                assert!(g.body.bottom() < g.footer.top());
                assert!((g.body.height() - body_h).abs() < 0.01);
            }
        }
    }
}
