//! 输入卡（composer）。
//!
//! 直接对应 Harness 的 `packages/client/ui-conversation/src/client/skeleton/InputBar.module.css`：
//!
//! ```text
//! .card    { gap: 12px; padding-top: 8px; border-radius: 22px;
//!            background: var(--dsw-specific-input-major); box-shadow: --dsw-elevation-soft }
//! .input   { padding: 4px 8px 0 14px; font: 14px/24px }
//! .hero .input { min-height: 52px }   .input { min-height: 36px }
//! .row     { padding: 2px 8px 6px; gap: 12px }
//! .add     { 28px 圆 }  .select { height: 28px }  .primary { 34px 圆 }
//! ```
//!
//! 两处偏离上游，都是为教室大屏服务：
//!
//! - **触控热区**：28/34px 的圆形控件视觉不变，命中区扩到 48pt 下限；
//! - **字号**：整卡跟随 `Metrics::scale` 放大，远距仍可读。

use egui::{Frame, Margin, Rect, Sense, TextEdit, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::{Chip, Icon, IconButton};

use super::{ease, elide, inset, text_left, translucent, Skin};
use crate::attachments::{Attachment, MAX_FILES};
use crate::state::AppState;

/// 本帧输入卡上发生的用户动作。
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    pub send: bool,
    pub attach: bool,
    pub remove_attachment: Option<usize>,
    pub cancel_import: bool,
    pub clear_attachment_error: bool,
    /// 生成中点了发送位上的停止按钮。
    pub stop: bool,
    pub toggle_plan: bool,
    pub toggle_read_only: bool,
    pub next_model: bool,
}

/// 模式提示条的高度（无提示时为 0）。
///
/// 对应 Harness `InputBar.module.css` 的 `.notice`：宽度撑满、下边距 6px、
/// 12/18 的次级文字。它画在**卡片外、卡片上方**，与上游一致。
pub fn notice_height(skin: &Skin<'_>, state: &AppState) -> f32 {
    let m = skin.m();
    if state.plan_mode || state.read_only || missing_model(state) {
        m.s(18.0) + m.s(6.0)
    } else {
        0.0
    }
}

/// 配了密钥却还没有模型列表 —— 这是**唯一**挡在真实对话前面的东西，
/// 值得占一行提示条把"该怎么做"说清楚。
fn missing_model(state: &AppState) -> bool {
    state.needs_model_list()
}

/// 整块高度 = 提示条 + 输入卡。调用方据此分配空间。
pub fn block_height(ui: &Ui, skin: &Skin<'_>, state: &AppState, width: f32, hero: bool) -> f32 {
    notice_height(skin, state)
        + attachment_area_height(skin, state)
        + card_height(ui, skin, &state.draft, width, hero)
}

fn attachment_strip_height(skin: &Skin<'_>, state: &AppState) -> f32 {
    if state.draft_attachments.is_empty() {
        0.0
    } else {
        skin.m().s(92.0)
    }
}

fn attachment_status_height(skin: &Skin<'_>) -> f32 {
    let m = skin.m();
    m.hit_target(m.s(28.0)).max(48.0) + m.s(6.0)
}

fn attachment_area_height(skin: &Skin<'_>, state: &AppState) -> f32 {
    attachment_strip_height(skin, state)
        + attachment_status_height(skin)
            * (usize::from(state.attachment_status.is_some())
                + usize::from(state.attachment_error.is_some())) as f32
}

/// 当前提示条文案。
fn notice_text(state: &AppState) -> &'static str {
    if state.plan_mode {
        "Plan 模式 · 我会先列出步骤，经你确认后再执行"
    } else if state.read_only {
        "只读模式 · 本次对话不会改动工作区文件"
    } else {
        "还没有可用模型 · 到「设置 → 模型」点「从模型商刷新」"
    }
}

/// 静息与悬停的卡片描边色。
fn card_stroke(skin: &Skin<'_>, hovered: bool) -> egui::Stroke {
    let p = skin.p();
    if hovered {
        egui::Stroke::new(1.0, p.border_l3)
    } else {
        egui::Stroke::new(1.0, p.border_l2)
    }
}

/// 文本区高度：随内容增长，`[min, max]` 之间收敛。
fn text_height(ui: &Ui, skin: &Skin<'_>, draft: &str, text_w: f32, hero: bool) -> f32 {
    let m = skin.m();
    let min_h = if hero {
        m.input_min_hero()
    } else {
        m.input_min_docked()
    };
    let max_h = m.input_max_height().max(min_h);

    let font = skin.prop(skin.t().body);
    let galley = ui
        .painter()
        .layout(draft.to_owned(), font, skin.p().label_primary, text_w);
    // 空草稿时 galley 的高度是 0，用一行行高兜底。
    let content = galley.size().y.max(m.line_height());
    content.clamp(min_h, max_h)
}

/// 卡片总高（供调用方在分配空间前预知）。
pub fn card_height(ui: &Ui, skin: &Skin<'_>, draft: &str, width: f32, hero: bool) -> f32 {
    let m = skin.m();
    let text_w = width - m.s(14.0) - m.s(12.0);
    m.card_pad_top()
        + text_height(ui, skin, draft, text_w, hero)
        + m.card_gap()
        + toolbar_height(skin)
}

fn draw_attachment_area(
    ui: &mut Ui,
    skin: &Skin<'_>,
    card: Rect,
    state: &AppState,
    out: &mut Outcome,
) {
    let m = skin.m();
    let left = card.left() + m.s(12.0);
    let width = (card.width() - m.s(24.0)).max(1.0);
    let mut y = card.top() + m.card_pad_top();
    let strip_h = attachment_strip_height(skin, state);
    if strip_h > 0.0 {
        let strip = Rect::from_min_size(egui::pos2(left, y), Vec2::new(width, strip_h));
        super::at(ui, strip, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("neo-draft-attachments")
                .auto_shrink([false, false])
                .max_height(strip_h)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = m.s(8.0);
                        for (index, attachment) in state.draft_attachments.iter().enumerate() {
                            let (rect, _) = ui.allocate_exact_size(
                                Vec2::new(m.s(274.0).min(width), m.s(80.0)),
                                Sense::hover(),
                            );
                            if attachment_card(ui, skin, rect, attachment, Some(index)) {
                                out.remove_attachment = Some(index);
                            }
                        }
                    });
                });
        });
        y += strip_h;
    }
    for (message, is_error) in [
        (state.attachment_status.as_deref(), false),
        (state.attachment_error.as_deref(), true),
    ] {
        let Some(message) = message else { continue };
        let row_h = attachment_status_height(skin);
        let row = Rect::from_min_size(egui::pos2(left, y), Vec2::new(width, row_h - m.s(6.0)));
        let color = if is_error {
            skin.p().error
        } else {
            skin.p().label_secondary
        };
        let font = skin.prop(skin.t().caption);
        let text = message.replace('\n', " · ");
        let shown = elide(ui.painter(), &text, &font, (width - m.s(52.0)).max(0.0));
        text_left(ui.painter(), row, &shown, font, color);
        ui.interact(
            row,
            ui.id().with(("neo-attachment-status", is_error)),
            Sense::hover(),
        )
        .on_hover_text(message);
        let response = IconButton::new(Icon::Close)
            .id_salt(("neo-attachment-status-close", is_error))
            .show_at(
                ui,
                &skin.d(),
                egui::pos2(row.right() - m.s(24.0), row.center().y),
            )
            .on_hover_text(if is_error {
                "关闭错误提示"
            } else {
                "取消附件导入"
            });
        if response.clicked() {
            if is_error {
                out.clear_attachment_error = true
            } else {
                out.cancel_import = true
            }
        }
        y += row_h;
    }
}

pub(super) fn attachment_card(
    ui: &mut Ui,
    skin: &Skin<'_>,
    rect: Rect,
    attachment: &Attachment,
    removable: Option<usize>,
) -> bool {
    let m = skin.m();
    let p = skin.p();
    let painter = ui.painter().clone();
    painter.squircle(
        rect,
        m.s(12.0),
        p.input_surface,
        egui::Stroke::new(1.0, p.border_l2),
    );
    let preview = Rect::from_min_size(rect.min + Vec2::splat(m.s(10.0)), Vec2::splat(m.s(46.0)));
    let icon = match attachment.kind.as_str() {
        "image" | "presentation" => Icon::Board,
        _ => Icon::Folder,
    };
    let thumbnail = if ui.is_rect_visible(rect) {
        attachment
            .image_url
            .as_deref()
            .and_then(|url| attachment_thumbnail(ui.ctx(), url))
    } else {
        None
    };
    if let Some(texture) = thumbnail {
        let size = texture.size_vec2();
        let factor = (preview.width() / size.x).min(preview.height() / size.y);
        let target = Rect::from_center_size(preview.center(), size * factor);
        painter.image(
            texture.id(),
            target,
            Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        icon.paint(&painter, preview.shrink(m.s(10.0)), p.label_tertiary, 1.5);
    }
    let left = preview.right() + m.s(10.0);
    let right = rect.right() - m.s(if removable.is_some() { 46.0 } else { 12.0 });
    let font = skin.prop(skin.t().label);
    let name = elide(&painter, &attachment.name, &font, (right - left).max(0.0));
    painter.text(
        egui::pos2(left, rect.top() + m.s(12.0)),
        egui::Align2::LEFT_TOP,
        name,
        font,
        p.label_primary,
    );
    let format = std::path::Path::new(&attachment.name)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or(&attachment.kind)
        .to_uppercase();
    let size = if attachment.bytes >= 1024 * 1024 {
        format!("{:.1} MiB", attachment.bytes as f64 / (1024.0 * 1024.0))
    } else if attachment.bytes >= 1024 {
        format!("{:.1} KiB", attachment.bytes as f64 / 1024.0)
    } else {
        format!("{} B", attachment.bytes)
    };
    let font = skin.prop(skin.t().caption);
    let detail = elide(
        &painter,
        &format!("{format} · {size}"),
        &font,
        (right - left).max(0.0),
    );
    painter.text(
        egui::pos2(left, rect.top() + m.s(34.0)),
        egui::Align2::LEFT_TOP,
        detail,
        font.clone(),
        p.label_caption,
    );
    if let Some(warning) = &attachment.warning {
        let warning = elide(
            &painter,
            warning,
            &font,
            (rect.width() - m.s(24.0)).max(0.0),
        );
        painter.text(
            egui::pos2(rect.left() + m.s(12.0), rect.top() + m.s(58.0)),
            egui::Align2::LEFT_TOP,
            warning,
            font,
            p.accent,
        );
    }
    let hover = ui.interact(
        rect,
        ui.id().with((
            "attachment-card",
            rect.top().to_bits(),
            rect.left().to_bits(),
        )),
        Sense::hover(),
    );
    hover.on_hover_text(match &attachment.warning {
        Some(warning) => format!("{}\n{warning}", attachment.name),
        None => attachment.name.clone(),
    });
    if let Some(index) = removable {
        #[cfg(test)]
        if index == 0 {
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("neo-test-first-attachment-remove"),
                    Rect::from_center_size(
                        egui::pos2(rect.right() - m.s(24.0), rect.top() + m.s(30.0)),
                        Vec2::splat(m.s(28.0)),
                    ),
                )
            });
        }
        return IconButton::new(Icon::Close)
            .id_salt(("neo-attachment-remove", index))
            .show_at(
                ui,
                &skin.d(),
                egui::pos2(rect.right() - m.s(24.0), rect.top() + m.s(30.0)),
            )
            .on_hover_text("移除此附件")
            .clicked();
    }
    false
}

#[derive(Clone, Default)]
struct ThumbnailCache {
    entries: std::collections::VecDeque<(egui::Id, Option<egui::TextureHandle>)>,
}

fn attachment_thumbnail(ctx: &egui::Context, url: &str) -> Option<egui::TextureHandle> {
    use base64::Engine;
    let cache_id = egui::Id::new("neo-attachment-thumbnails");
    let key = egui::Id::new(url);
    let cached = ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<ThumbnailCache>(cache_id);
        let index = cache.entries.iter().position(|(id, _)| *id == key)?;
        let entry = cache.entries.remove(index)?;
        let texture = entry.1.clone();
        cache.entries.push_back(entry);
        Some(texture)
    });
    if let Some(texture) = cached {
        return texture;
    }
    let texture = (|| {
        if url.len() > 16 * 1024 * 1024 {
            return None;
        }
        let (encoded, format) = if let Some(encoded) = url.strip_prefix("data:image/png;base64,") {
            (encoded, image::ImageFormat::Png)
        } else {
            (
                url.strip_prefix("data:image/jpeg;base64,")?,
                image::ImageFormat::Jpeg,
            )
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(1536);
        limits.max_image_height = Some(1536);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let pixels = reader.decode().ok()?.thumbnail(96, 96).to_rgba8();
        let size = [pixels.width() as usize, pixels.height() as usize];
        let image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
        Some(ctx.load_texture(
            format!("neo-attachment-{key:?}"),
            image,
            egui::TextureOptions::LINEAR,
        ))
    })();
    ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<ThumbnailCache>(cache_id);
        // 无效图片也缓存，避免每一帧重新解码；仅保留小尺寸纹理，不复制附件数据。
        if cache.entries.len() >= 64 {
            cache.entries.pop_front();
        }
        cache.entries.push_back((key, texture.clone()));
    });
    texture
}

/// 工具栏行高：内容最高者（发送按钮）+ 上下内边距。
fn toolbar_height(skin: &Skin<'_>) -> f32 {
    let m = skin.m();
    m.btn_send() + m.s(2.0) + m.s(6.0)
}

/// 绘制输入卡。
///
/// `hero` 决定文本区地板高度（空态两行 / 停靠态一行），其余完全一致。
pub fn draw(ui: &mut Ui, skin: &Skin<'_>, rect: Rect, state: &mut AppState, hero: bool) -> Outcome {
    let p = skin.p();
    let m = skin.m();
    let mut out = Outcome::default();

    // `rect` 是"提示条 + 卡片"整块；两者在这里切开。
    let notice_h = notice_height(skin, state);
    let notice_rect = Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + notice_h));
    let rect = Rect::from_min_max(egui::pos2(rect.left(), notice_rect.bottom()), rect.max);

    // ---- 模式提示条（卡片上方，随开关出现）----
    if notice_h > 0.0 {
        let font = skin.prop(skin.t().caption);
        let shown = elide(
            &ui.painter().clone(),
            notice_text(state),
            &font,
            notice_rect.width(),
        );
        ui.painter().text(
            egui::pos2(notice_rect.left() + m.s(8.0), notice_rect.top()),
            egui::Align2::LEFT_TOP,
            shown,
            font,
            p.label_secondary,
        );
    }

    // ---- 卡片本体 ----
    // 悬停判定用整张卡的范围，让"鼠标进入卡片即高亮"这种大屏下的粗操作手感成立。
    let card_hit = ui.interact(
        rect,
        ui.id()
            .with(("composer", rect.left() as i32, rect.top() as i32)),
        Sense::click(),
    );
    let hovered = card_hit.hovered();
    let painter = ui.painter().clone();

    let shadow = super::elevation_soft(skin);
    painter.add(shadow.as_shape(rect, m.radius_card()));
    painter.squircle(
        rect,
        m.radius_card(),
        p.input_surface,
        card_stroke(skin, hovered),
    );

    // 聚焦环：键盘焦点落在编辑区时，卡片外沿浮出一圈 accent 描边（0.1s 缓动，
    // 对应 spec fast 档）。画在卡片本体之上、内容之下；失焦淡出到 0 即不画。
    let focused =
        ui.ctx().memory(|mem| mem.focused()) == Some(egui::Id::new(super::COMPOSER_ID));
    let focus_k = ease(ui, ui.id().with("focus-ring"), if focused { 1.0 } else { 0.0 });
    if focus_k > 0.0 {
        let expand = m.s(1.5);
        painter.squircle_stroked(
            rect.expand(expand),
            m.radius_card() + expand,
            egui::Stroke::new(m.s(2.0), translucent(p.accent, focus_k)),
        );
    }

    draw_attachment_area(ui, skin, rect, state, &mut out);

    // ---- 文本区 ----
    let text_rect = Rect::from_min_max(
        egui::pos2(
            rect.left() + m.s(14.0),
            rect.top() + m.card_pad_top() + attachment_area_height(skin, state),
        ),
        egui::pos2(
            rect.right() - m.s(12.0),
            rect.bottom() - m.card_gap() - toolbar_height(skin),
        ),
    );
    let text_h = text_height(ui, skin, &state.draft, text_rect.width(), hero);
    let text_rect = Rect::from_min_size(text_rect.min, Vec2::new(text_rect.width(), text_h));

    // 占位符：上游用 caption 色 + 与文本区同内缩的绝对定位元素。
    // 输入法组合期间 `state.draft` 还来不及写进预编辑串（TextEdit 在本段之后才
    // 处理本帧的 `Ime::Preedit`），占位符若仍按 `draft.is_empty()` 显示，会与
    // 拼音候选字重叠一帧。用 `ime_active` 一并兜住。
    if state.draft.is_empty() && !super::ime_active(ui.ctx()) {
        let font = skin.prop(skin.t().body);
        let ph_rect = inset(text_rect, 0.0, m.s(4.0), 0.0, 0.0);
        let hint = if hero {
            "问点什么，或者从下面挑一个课堂场景"
        } else {
            "继续追问…"
        };
        let shown = elide(&painter, hint, &font, ph_rect.width());
        painter.text(
            egui::pos2(ph_rect.left(), ph_rect.top()),
            egui::Align2::LEFT_TOP,
            shown,
            font,
            p.label_caption,
        );
    }

    // 真正的编辑区：egui 只保留输入法与选区这两件事。
    super::at(ui, text_rect, |ui| {
        let font = skin.prop(skin.t().body);
        ui.add_sized(
            text_rect.size(),
            TextEdit::multiline(&mut state.draft)
                .id(egui::Id::new(super::COMPOSER_ID))
                .frame(Frame::NONE)
                .margin(Margin::symmetric(0, 4))
                .font(font)
                .text_color(p.label_primary)
                .desired_width(text_rect.width()),
        );
    });

    // ---- 工具栏 ----
    let d = skin.d();
    let row = Rect::from_min_max(
        egui::pos2(rect.left() + m.s(8.0), text_rect.bottom() + m.card_gap()),
        egui::pos2(rect.right() - m.s(8.0), rect.bottom()),
    );
    let row = inset(row, 0.0, m.s(2.0), 0.0, m.s(6.0));
    let mid = row.center().y;

    // 左侧：+ / Plan / 只读 —— 全部来自组件库。
    let add_center = egui::pos2(row.left() + m.btn_add() * 0.5, mid);
    let can_attach = !state.generating
        && !state.tool_open
        && !state.tool_round
        && !state.attachment_busy()
        && !state.attachment_picker_open
        && state.draft_attachments.len() < MAX_FILES;
    if IconButton::new(Icon::Plus)
        .subtle()
        .enabled(can_attach)
        .id_salt("neo-composer-add")
        .show_at(ui, &d, add_center)
        .on_hover_text("添加图片、Word、PowerPoint 或文本文件")
        .clicked()
    {
        out.attach = true;
    }

    let chip_h = m.chip_h();
    let mut x = add_center.x + m.btn_add() * 0.5 + m.toolbar_gap();
    let plan_w = Chip::width(&painter, &d, "Plan", false);
    let plan_rect =
        Rect::from_min_size(egui::pos2(x, mid - chip_h * 0.5), Vec2::new(plan_w, chip_h));
    if Chip::new("Plan")
        .active(state.plan_mode)
        .id_salt("neo-composer-plan")
        .show_at(ui, &d, plan_rect)
        .clicked()
    {
        out.toggle_plan = true;
    }
    x = plan_rect.right() + m.toolbar_gap();

    let ro_w = Chip::width(&painter, &d, "只读", false);
    let ro_rect = Rect::from_min_size(egui::pos2(x, mid - chip_h * 0.5), Vec2::new(ro_w, chip_h));
    if Chip::new("只读")
        .active(state.read_only)
        .id_salt("neo-composer-readonly")
        .show_at(ui, &d, ro_rect)
        .clicked()
    {
        out.toggle_read_only = true;
    }

    // 右侧：模型选择器 + 发送/停止
    let send_d = m.btn_send();
    let send_center = egui::pos2(row.right() - send_d * 0.5, mid - m.s(2.0));
    let generating = state.generating;
    let can_send = state.can_submit();
    // 工具轮（等确认 / 后台执行中）也算"进行中"：否则那段时间按钮变回
    // 发送且不可点，用户只能看着工具跑完并自动开始下一轮 —— 没有停止入口。
    let busy = generating || state.tool_open || state.tool_round;
    // 生成中按钮变成"停止"（仍可点）；无可发内容时淡出强调色（不可点）。
    let send_icon = if busy { Icon::Stop } else { Icon::ArrowUp };
    if IconButton::new(send_icon)
        .accent()
        .enabled(busy || can_send)
        .id_salt("neo-composer-send")
        .show_at(ui, &d, send_center)
        .clicked()
    {
        if busy {
            out.stop = true;
        } else if can_send {
            out.send = true;
        }
    }

    let model_w = Chip::width(&painter, &d, state.model_display(), true);
    let model_rect = Rect::from_min_size(
        egui::pos2(
            send_center.x - send_d * 0.5 - m.toolbar_gap() - model_w,
            mid - chip_h * 0.5,
        ),
        Vec2::new(model_w, chip_h),
    );
    if Chip::new(state.model_display())
        .chevron(true)
        .id_salt("neo-composer-model")
        .show_at(ui, &d, model_rect)
        .clicked()
    {
        out.next_model = true;
    }

    // 提示文本：把"回车发送"这类约定讲清楚，大屏上用户很少看文档。
    let hint_x = ro_rect.right() + m.toolbar_gap();
    if hint_x < model_rect.left() - m.s(8.0) {
        let hint = "Enter 发送 · Shift+Enter 换行";
        let avail = model_rect.left() - m.s(8.0) - hint_x;
        let font = skin.prop(skin.t().caption);
        let shown = elide(&painter, hint, &font, avail);
        if !shown.is_empty() {
            text_left(
                &painter,
                Rect::from_min_max(
                    egui::pos2(hint_x, mid - chip_h * 0.5),
                    egui::pos2(model_rect.left(), mid + chip_h * 0.5),
                ),
                &shown,
                font,
                p.label_caption,
            );
        }
    }

    out
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn thumbnail_is_small_and_reuses_texture() {
        let ctx = egui::Context::default();
        let image = image::DynamicImage::new_rgba8(240, 120);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
        );
        let first = attachment_thumbnail(&ctx, &url).unwrap();
        let second = attachment_thumbnail(&ctx, &url).unwrap();
        assert_eq!(first.size(), [96, 48]);
        assert_eq!(first.id(), second.id());
        let cache = ctx.data(|data| {
            data.get_temp::<ThumbnailCache>(egui::Id::new("neo-attachment-thumbnails"))
                .unwrap()
        });
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn failed_thumbnails_are_cached_and_bounded() {
        let ctx = egui::Context::default();
        let first = "data:image/png;base64,broken";
        assert!(attachment_thumbnail(&ctx, first).is_none());
        assert!(attachment_thumbnail(&ctx, first).is_none());
        let cache_id = egui::Id::new("neo-attachment-thumbnails");
        assert_eq!(
            ctx.data(|data| data
                .get_temp::<ThumbnailCache>(cache_id)
                .unwrap()
                .entries
                .len()),
            1
        );
        for index in 0..70 {
            assert!(attachment_thumbnail(&ctx, &format!("invalid-{index}")).is_none());
        }
        let cache = ctx.data(|data| data.get_temp::<ThumbnailCache>(cache_id).unwrap());
        assert_eq!(cache.entries.len(), 64);
        assert!(cache.entries.iter().all(|(_, texture)| texture.is_none()));
        assert!(!cache
            .entries
            .iter()
            .any(|(id, _)| *id == egui::Id::new(first)));
    }
}
