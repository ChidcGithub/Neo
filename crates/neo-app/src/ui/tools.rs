//! 工具调用的权限确认弹窗。
//!
//! 标题和操作区固定，工具元信息、动作预览及完整 JSON 在中间独立滚动。
//! 不截断待授权内容，也不依赖 JSON 行数估计实际换行高度。

use crate::i18n::{tf, tr};
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
        debug_assert!(
            panel.contains_rect(title),
            "panel={panel:?} title={title:?} pad={pad} title_h={title_h} footer_h={footer_h}"
        );
        debug_assert!(
            panel.contains_rect(footer),
            "panel={panel:?} footer={footer:?}"
        );
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
pub fn confirm(
    ui: &mut Ui,
    skin: &Skin<'_>,
    meta: &ToolMeta,
    remaining: usize,
    allow_batch: bool,
) -> Option<Answer> {
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
        tr("需要你的许可").into(),
        skin.bold(skin.t().headline),
        p.label_primary,
        inner_w,
    );
    let risk = match meta.risk {
        "exec" => tr("执行命令"),
        "write" => tr("修改文件"),
        "open" => tr("打开内容"),
        "read" => tr("读取内容"),
        other => other,
    };
    let mut badge = format!("{} · {} · {}", tr(meta.title), meta.name, risk);
    if remaining > 1 {
        badge.push_str(&tf(
            "\n另有 {count} 个调用等待确认",
            &[("count", (remaining - 1).to_string())],
        ));
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
        tr("完整参数").into(),
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
    let labels = [tr("允许"), tr("本会话都允许"), tr("拒绝")];
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
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(body_w, badge.size().y), egui::Sense::hover());
                ui.painter().galley(r.min, badge, p.label_caption);
                ui.add_space(gap);
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(body_w, preview_h), egui::Sense::hover());
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
                (Button::new(tr("允许")).primary(), Answer::Once),
                (
                    Button::new(tr("本会话都允许"))
                        .elevated()
                        .enabled(allow_batch),
                    Answer::Always,
                ),
                (Button::new(tr("拒绝")).ghost(), Answer::Deny),
            ] {
                let response = button.show(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(("neo-confirm-button-probe", answer as u8)),
                        (response.rect, ui.clip_rect()),
                    );
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
        tr("Neo 想问").into(),
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
                    let color = if i == 0 {
                        d.c().on_info
                    } else {
                        p.label_primary
                    };
                    let text = ui.painter().layout(
                        opt.clone(),
                        d.font_bold(d.t().label),
                        color,
                        (body_w - m.s(32.0)).max(1.0),
                    );
                    let height = (text.size().y + m.s(20.0)).max(m.hit_target(m.s(36.0)));
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(body_w, height), egui::Sense::hover());
                    let response =
                        super::tap(ui, r, ui.id().with(("ask-option", &meta.call_id, i)));
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), opt)
                    });
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new(("neo-ask-option-probe", i)),
                            (
                                r,
                                ui.clip_rect(),
                                text.size(),
                                text.rows.len(),
                                response.enabled(),
                            ),
                        );
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
            let skip = Button::new(tr("跳过")).ghost().show(ui, &d);
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("neo-ask-skip-probe"),
                    (skip.rect, ui.clip_rect()),
                );
            });
            if skip.clicked() {
                out = Some(AskAnswer::Skip);
            }
        });
    });
    out
}

#[cfg(test)]
#[path = "tools_ui_regression.rs"]
mod ui_regression;

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
