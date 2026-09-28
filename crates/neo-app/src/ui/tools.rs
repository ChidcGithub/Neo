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
        debug_assert!(panel.contains_rect(title), "panel={panel:?} title={title:?} pad={pad} title_h={title_h} footer_h={footer_h}");
        debug_assert!(panel.contains_rect(footer), "panel={panel:?} footer={footer:?}");
        debug_assert!(body.height() > 0.0, "body={body:?}");
        Self {
            panel,
            title,
            body,
            footer,
        }
    }
}

/// 卡矩形：渲染层和独立视口里 `ui.max_rect()` 就是整张卡，直接用。
/// 离屏测试把确认视口嵌进根窗口、窗体按内容自收缩 —— 首帧 max_rect 极小，
/// 这时自报标准尺寸（以当前 min 为左上角，与自收缩生长方向一致），
/// 并向宿主 `allocate_rect` 报备尺寸，下一帧窗体长大即稳定在 max_rect 分支。
fn panel_rect(ui: &mut Ui, m: neo_theme::Metrics) -> Rect {
    let rect = ui.max_rect();
    let min = Vec2::new(m.s(280.0), m.s(180.0));
    let panel = if rect.width() < min.x || rect.height() < min.y {
        let screen = ui.ctx().content_rect();
        let size = Vec2::new(m.s(560.0), m.s(420.0))
            .min(screen.size() - egui::vec2(m.s(32.0), m.s(32.0)))
            .max(min);
        Rect::from_min_size(rect.min, size)
    } else {
        rect
    };
    ui.allocate_rect(panel, egui::Sense::hover());
    panel
}

/// 画确认弹窗。只有点击明确的授权/拒绝按钮才返回答复。
///
/// **内联渲染，不开模态**：渲染层里这张卡就是全部可点区域（命中测试只认
/// 卡矩形）；`egui::Modal` 会把面板居中到整层屏幕、逃出命中区 —— 按钮
/// 看着在，点击全穿透。
pub fn confirm(ui: &mut Ui, skin: &Skin<'_>, meta: &ToolMeta, remaining: usize, allow_batch: bool) -> Option<Answer> {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let panel = panel_rect(ui, m);
    let pad = m.s(22.0);
    let gap = m.s(12.0);
    let inner_w = (panel.width() - pad * 2.0).max(1.0);
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
                (Button::new("本会话都允许").elevated().enabled(allow_batch), Answer::Always),
                (Button::new("拒绝").ghost(), Answer::Deny),
            ] {
                let response = button.show(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new(("neo-confirm-button-probe", answer as u8)),
                        (response.rect, ui.clip_rect()));
                });
                debug_assert!(g.panel.expand(0.5).contains_rect(response.rect));
                if response.clicked() {
                    out = Some(answer);
                }
            }
        });
    });
    out
}

/// 用户对一次提问（`ask_user`）的答复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskAnswer {
    /// 点了第 N 个选项（下标按 options 参数顺序）。
    Pick(usize),
    /// 跳过不答；回灌「未作答」，模型按最合理假设继续。
    Skip,
}

/// 画提问弹窗（`ask_user`）。与确认窗同骨架：标题 + 问题面板 + 操作区。
/// 选项纵向排列、第一个（模型认为最可能的）高亮；「跳过」在右下角。
/// 与 confirm 同样内联渲染（不进模态，原因见 confirm 的文档注释）。
pub fn ask(ui: &mut Ui, skin: &Skin<'_>, meta: &ToolMeta) -> Option<AskAnswer> {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let panel = panel_rect(ui, m);
    let pad = m.s(22.0);
    let gap = m.s(12.0);
    let inner_w = (panel.width() - pad * 2.0).max(1.0);

    let question = meta
        .args
        .get("question")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&meta.preview)
        .to_owned();
    let options_raw = meta
        .args
        .get("options")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let options = neo_tools::tools::ask_user::parse_options(options_raw);

    let title = ui.painter().layout(
        "Neo 想问".into(),
        skin.bold(skin.t().headline),
        p.label_primary,
        inner_w,
    );
    let footer_h = m.s(36.0);

    Panel::new().paint(ui, &d, panel, pad);
    let g = Geometry::new(panel, pad, title.size().y, footer_h, gap);
    ui.painter().galley(g.title.min, title, p.label_primary);
    let mut out = None;
    super::at(ui, g.body, |ui| {
        ui.set_clip_rect(ui.clip_rect().intersect(g.body));
        egui::ScrollArea::vertical()
            .id_salt(("neo-ask-body", &meta.call_id))
            .auto_shrink([false, false])
            .max_height(g.body.height())
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                let body_w = ui.available_width().min(inner_w).max(1.0);
                ui.set_width(body_w);
                let question_g = ui.painter().layout(
                    question.clone(),
                    skin.prop(skin.t().body),
                    p.label_primary,
                    (body_w - m.s(20.0)).max(1.0),
                );
                let (r, _) = ui.allocate_exact_size(
                    Vec2::new(body_w, question_g.size().y + m.s(16.0)),
                    egui::Sense::hover(),
                );
                ui.painter().squircle_filled(r, m.s(10.0), p.bg_layer_1);
                ui.painter().galley(
                    r.min + egui::vec2(m.s(10.0), m.s(8.0)),
                    question_g,
                    p.label_primary,
                );
                for (i, opt) in options.iter().enumerate() {
                    ui.add_space(gap);
                    // 完整排版选项，按实际换行高度分配触控区域，不省略待选择内容。
                    let color = if i == 0 { d.c().on_info } else { p.label_primary };
                    let text = ui.painter().layout(
                        opt.clone(),
                        d.font_bold(d.t().label),
                        color,
                        (body_w - m.s(32.0)).max(1.0),
                    );
                    let height = (text.size().y + m.s(20.0)).max(m.hit_target(m.s(36.0)));
                    let (r, _) = ui.allocate_exact_size(
                        Vec2::new(body_w, height),
                        egui::Sense::hover(),
                    );
                    let response = super::tap(ui, r, ui.id().with(("ask-option", &meta.call_id, i)));
                    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), opt));
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(egui::Id::new(("neo-ask-option-probe", i)),
                            (r, ui.clip_rect(), text.size(), text.rows.len(), response.enabled()));
                    });
                    let fill = match (i == 0, response.hovered()) {
                        (true, true) => d.c().btn_info_hover,
                        (true, false) => d.c().btn_info,
                        (false, true) => d.c().hover_solid,
                        (false, false) => d.c().btn_elevated,
                    };
                    ui.painter().squircle_filled(r, m.radius_chip(), fill);
                    ui.painter().galley(
                        egui::pos2(r.left() + m.s(16.0), r.center().y - text.size().y * 0.5),
                        text,
                        color,
                    );
                    if response.clicked() {
                        out = Some(AskAnswer::Pick(i));
                    }
                }
            });
    });
    super::at(ui, g.footer, |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let skip = Button::new("跳过").ghost().show(ui, &d);
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("neo-ask-skip-probe"), (skip.rect, ui.clip_rect()));
            });
            if skip.clicked() {
                out = Some(AskAnswer::Skip);
            }
        });
    });
    out
}

#[cfg(test)]
mod ui_regression {
    use super::*;
    use crate::ui::composer::ui_regression::{context, pointer, probe};

    #[test]
    fn long_confirmation_scrolls_without_moving_actions_and_blocks_batch_in_safe_mode() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for width in [320.0, 560.0] {
                for (enabled, allow_batch) in [(true, false), (true, true), (false, true)] {
                    let ctx = context();
                    let mut meta = ToolMeta::restored("write_file");
                    meta.call_id = "long-confirm".into();
                    meta.preview = "完整动作预览 ".repeat(150);
                    meta.args = serde_json::json!({"content": "long-content-without-spaces".repeat(100)});
                    let size = Vec2::new(width, 420.0);
                    let answer = std::cell::Cell::new(None);
                    let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                        ui.add_enabled_ui(enabled, |ui| {
                            answer.set(confirm(ui, skin, &meta, 3, allow_batch));
                        });
                    };
                    let frame = |events, render: &mut dyn FnMut(&mut Ui, &Skin<'_>)| {
                        crate::ui::composer::ui_regression::frame_themed(&ctx, size, events, mode, render)
                    };
                    for _ in 0..3 { frame(vec![], &mut render); }
                    let (body, _, content_h): (Rect, f32, f32) = probe(&ctx, "neo-confirm-scroll-probe");
                    assert!(content_h > body.height());
                    let (batch, clip): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", Answer::Always as u8));
                    assert!(clip.contains_rect(batch));
                    assert!(batch.top() >= body.bottom());
                    if enabled {
                        for _ in 0..8 {
                            frame(vec![egui::Event::PointerMoved(body.center()), egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -10000.0),
                                phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE,
                            }], &mut render);
                        }
                        let (body, offset, content_h): (Rect, f32, f32) = probe(&ctx, "neo-confirm-scroll-probe");
                        assert!(offset > 0.0);
                        assert!(offset + body.height() >= content_h - 1.0);
                    }
                    let (after, _): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", Answer::Always as u8));
                    assert_eq!(batch, after);
                    frame(pointer(batch.center(), true), &mut render);
                    frame(pointer(batch.center(), false), &mut render);
                    assert_eq!(answer.get(), (enabled && allow_batch).then_some(Answer::Always));
                    for action in [Answer::Once, Answer::Deny] {
                        let (button, _): (Rect, Rect) = probe(&ctx, ("neo-confirm-button-probe", action as u8));
                        frame(pointer(button.center(), true), &mut render);
                        frame(pointer(button.center(), false), &mut render);
                        assert_eq!(answer.get(), enabled.then_some(action));
                    }
                }
            }
        }
    }

    #[test]
    fn long_ask_options_wrap_scroll_to_tail_and_respect_disabled() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let frame = |ctx: &egui::Context, size, events, draw: &mut dyn FnMut(&mut Ui, &Skin<'_>)| {
            crate::ui::composer::ui_regression::frame_themed(ctx, size, events, mode, draw)
        };
        for enabled in [false, true] {
            let ctx = context();
            let meta = ToolMeta {
                call_id: "pure-ask".into(), name: "ask_user".into(), title: "提问", risk: "read",
                preview: "question".into(), state: crate::state::ToolState::AwaitingConfirm,
                outcome: None,
                args: serde_json::json!({"question": "Choose", "options": format!("{}|{}", "long option with wrapping ".repeat(35), "last option tail ".repeat(35))}),
            };
            let size = Vec2::new(360.0, 420.0);
            let answer = std::cell::Cell::new(None);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                ui.add_enabled_ui(enabled, |ui| { answer.set(ask(ui, skin, &meta)); });
            };
            for _ in 0..3 { frame(&ctx, size, vec![], &mut render); }
            let (first, clip, text, rows, response_enabled): (Rect, Rect, Vec2, usize, bool) =
                probe(&ctx, ("neo-ask-option-probe", 0usize));
            assert!(rows > 1);
            assert!(first.height() >= text.y + 19.0);
            assert!(text.x <= first.width() - 31.0);
            assert_eq!(response_enabled, enabled);
            let (skip, skip_clip): (Rect, Rect) = probe(&ctx, "neo-ask-skip-probe");
            assert!(skip_clip.contains_rect(skip));
            let (tail, _, _, _, _): (Rect, Rect, Vec2, usize, bool) =
                probe(&ctx, ("neo-ask-option-probe", 1usize));
            assert!(tail.bottom() > clip.bottom());
            // 实际滚轮事件只滚动正文，底部操作保持固定。
            if enabled {
                for _ in 0..8 {
                    frame(&ctx, size, vec![egui::Event::PointerMoved(clip.center()), egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -2000.0), phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    }], &mut render);
                }
                let (tail, tail_clip, _, _, _): (Rect, Rect, Vec2, usize, bool) =
                    probe(&ctx, ("neo-ask-option-probe", 1usize));
                assert!(tail.bottom() <= tail_clip.bottom() + 1.0, "{tail:?}, {tail_clip:?}");
                let pos = egui::pos2(tail.center().x, tail.bottom() - 12.0);
                frame(&ctx, size, pointer(pos, true), &mut render);
                frame(&ctx, size, pointer(pos, false), &mut render);
                assert_eq!(answer.get(), Some(AskAnswer::Pick(1)));
                let (after, _): (Rect, Rect) = probe(&ctx, "neo-ask-skip-probe");
                assert_eq!(skip, after);
            } else {
                let pos = first.intersect(clip).center();
                frame(&ctx, size, pointer(pos, true), &mut render);
                frame(&ctx, size, pointer(pos, false), &mut render);
                assert_eq!(answer.get(), None);
                frame(&ctx, size, pointer(skip.center(), true), &mut render);
                frame(&ctx, size, pointer(skip.center(), false), &mut render);
                assert_eq!(answer.get(), None);
            }
        }
        }
    }
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
