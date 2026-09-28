//! 空态（hero）。
//!
//! 单列欢迎区：整体垂直居中，标题、工作区与输入卡共享左基线。
//! 标题按实际列宽换行，工作区路径截断而不挤出窄窗口。
//!
//! ```text
//! .headline { gap: 10px; font-size: 26px; line-height: 32px; font-weight: 500 }
//! .stack    { flex-direction: column; gap: 12px; max-width: --dsh-composer-card-max-width }
//! ```

use egui::{Rect, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::Icon;

use super::{composer, elide, text_left, Skin};
use crate::state::AppState;

/// hero 上发生的用户动作。
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    pub workspace_clicked: bool,
    pub composer: composer::Outcome,
}

/// 绘制空态：鲸鱼 + 标题 / workspace chip / 输入卡，三段垂直居中。
pub fn draw(ui: &mut Ui, skin: &Skin<'_>, area: Rect, state: &mut AppState) -> Outcome {
    let p = skin.p();
    let m = skin.m();
    let t = skin.t();
    let mut out = Outcome::default();

    let card_w = m.card_max(area.width());
    let card_h = composer::block_height(ui, skin, state, card_w, true);

    let fish_w = t.fish;
    let title = ui.painter().layout(
        "今天想在课堂上做点什么？".to_owned(),
        skin.bold(t.headline),
        p.label_primary,
        (card_w - fish_w - m.s(16.0)).max(1.0),
    );
    let headline_h = title.size().y.max(t.headline_lh);
    let chip_h = m.hit_target(m.s(28.0));
    let gap = m.s(8.0);
    let head_gap = m.s(20.0);

    let fixed = headline_h + head_gap + chip_h + gap;
    // 卡片高 clamp 进剩余空间：长草稿 + 矮窗口（如四分屏）时文本区变矮
    // （内部滚动），而不是整列冲出屏幕把发送钮裁掉。
    let card_h = card_h.min((area.height() - fixed - m.s(48.0)).max(m.s(120.0)));
    let total = fixed + card_h;
    // 顶部留一点余量，视觉重心比几何中心略高更稳。
    let top = area.center().y - total * 0.5 - area.height() * 0.02;
    let top = top.max(area.top() + m.s(24.0));

    let col = Rect::from_min_size(
        egui::pos2(area.center().x - card_w * 0.5, top),
        Vec2::new(card_w, total),
    );

    // 标题、工作区与输入卡共用左基线；窄窗口按实际列宽换行。
    let fish_h = fish_w / skin.whale.aspect();
    let gx = col.left();
    let gy = col.top();
    let whale_rect = Rect::from_min_size(
        egui::pos2(gx, gy + (t.headline_lh - fish_h) * 0.5),
        Vec2::new(fish_w, fish_h),
    );
    skin.whale
        .paint(ui.painter(), whale_rect, p.label_secondary);
    ui.painter().galley(
        egui::pos2(gx + fish_w + m.s(16.0), gy),
        title,
        p.label_primary,
    );

    // ---- workspace chip（宽度随内容收缩，不撑满也不留白）----
    let chip_top = gy + headline_h + head_gap;
    let chip_font = skin.bold(skin.t().label);
    let label = state.workspace.as_deref().unwrap_or("选择工作区");
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), chip_font.clone(), p.label_primary)
        .size()
        .x
        .min(m.s(280.0));
    // icon(16) + 间距(4) + 文字 + 间距(4) + chevron(10) + 两侧内边距(14+12)
    let chip_w = (m.s(14.0 + 16.0 + 4.0) + text_w + m.s(4.0 + 10.0 + 12.0)).min(card_w);
    let chip = Rect::from_min_size(egui::pos2(col.left(), chip_top), Vec2::new(chip_w, chip_h));
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::new("neo-hero-workspace-probe"), chip);
    });
    if workspace_chip(ui, skin, chip, state.workspace.as_deref()) {
        out.workspace_clicked = true;
    }

    // ---- 输入卡 ----
    let card = Rect::from_min_size(
        egui::pos2(col.left(), chip_top + chip_h + gap),
        Vec2::new(card_w, card_h),
    );
    out.composer = composer::draw(ui, skin, card, state, true);

    out
}

/// workspace chip（folder + 标签 + chevron）。
fn workspace_chip(ui: &Ui, skin: &Skin<'_>, rect: Rect, label: Option<&str>) -> bool {
    let p = skin.p();
    let m = skin.m();
    let chip_resp = super::tap(ui, rect, ui.id().with("neo-workspace-chip"));
    let resp = chip_resp.on_hover_text("选择文件夹作为工作区");
    let st = super::State::of(&resp);
    let painter = ui.painter();

    if st.hovered {
        painter.squircle_filled(rect, m.radius_chip(), p.hover);
    }

    let icon_r = Rect::from_center_size(
        egui::pos2(rect.left() + m.s(14.0), rect.center().y),
        Vec2::splat(m.s(16.0)),
    );
    Icon::Folder.paint(painter, icon_r, p.label_primary);

    let label_font = skin.bold(skin.t().label);
    let chevron_w = m.s(16.0);
    let text_rect = Rect::from_min_max(
        egui::pos2(icon_r.right() + m.s(4.0), rect.top()),
        egui::pos2(rect.right() - chevron_w, rect.bottom()),
    );
    let text = label.unwrap_or("选择工作区");
    let shown = elide(painter, text, &label_font, text_rect.width());
    text_left(
        painter,
        text_rect,
        &shown,
        label_font,
        if label.is_some() {
            p.label_primary
        } else {
            p.label_caption
        },
    );

    Icon::ChevronDown.paint(
        painter,
        Rect::from_center_size(
            egui::pos2(rect.right() - m.s(12.0), rect.center().y),
            Vec2::splat(m.s(10.0)),
        ),
        p.label_caption,
    );

    resp.clicked()
}

#[cfg(test)]
mod ui_regression {
    use super::*;
    use crate::ui::composer::ui_regression::{context, probe};

    #[test]
    fn welcome_column_wraps_and_keeps_workspace_and_send_inside_viewport() {
        for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
            for (width, height, scale) in [
                (320.0, 600.0, 1.0),
                (360.0, 640.0, 0.85),
                (480.0, 720.0, 1.25),
                (768.0, 1024.0, 1.6),
                (1280.0, 720.0, 1.0),
                (1920.0, 1080.0, 1.25),
                (3840.0, 2160.0, 2.8),
            ] {
                let ctx = context();
                let theme =
                    neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let area = Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(width, height));
                let mut state = AppState::default();
                state.workspace = Some("long-workspace-path/".repeat(30));
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(area),
                            ..Default::default()
                        },
                        |ui| {
                            draw(ui, &skin, area, &mut state);
                        },
                    );
                    // 纯布局测试不上传 GPU；显式消费本帧纹理增量，遵守 egui 帧生命周期。
                    output.textures_delta.clear();
                    let mut found_title = false;
                    for shape in output.shapes {
                        if let egui::Shape::Text(text) = shape.shape {
                            if text.galley.job.text == "今天想在课堂上做点什么？" {
                                found_title = true;
                                let bounds = Rect::from_min_size(text.pos, text.galley.size());
                                assert!(area.contains_rect(bounds), "{mode:?} {width}: {bounds:?}");
                            }
                        }
                    }
                    assert!(found_title, "欢迎标题必须实际绘制");
                }
                let chip: Rect = probe(&ctx, "neo-hero-workspace-probe");
                let (send, _): (Rect, Rect) = probe(&ctx, "neo-composer-send-probe");
                assert!(area.contains_rect(chip));
                assert!(area.contains_rect(send));
                assert!(chip.bottom() < send.top());
                // 矩形坐标相减会有亚像素舍入，容差不改变触控目标尺寸。
                assert!(chip.height() + 0.01 >= skin.m().hit_target(0.0));
            }
        }
    }
}
