//! 空态（hero）。
//!
//! 对应 Harness 的 `HeroShell`：整块垂直居中，一列内容 ——
//! 鲸鱼标志 + 标题、workspace chip、输入卡。
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

    let headline_h = t.headline_lh;
    let chip_h = m.s(28.0);
    let gap = m.stack_gap();
    // 标题与下方内容之间留开一点：它是这一列的「组标题」，
    // 与 chip/输入卡的节奏区分开。
    let head_gap = m.s(28.0);

    let total = headline_h + head_gap + chip_h + gap + card_h;
    // 顶部留一点余量，视觉重心比几何中心略高更稳。
    let top = area.center().y - total * 0.5 - area.height() * 0.02;
    let top = top.max(area.top() + m.s(24.0));

    let col = Rect::from_min_size(
        egui::pos2(area.center().x - card_w * 0.5, top),
        Vec2::new(card_w, total),
    );

    // ---- 标题行：鲸鱼 + 文字 ----
    let fish_w = t.fish;
    let fish_h = fish_w / skin.whale.aspect();
    let title = "今天想在课堂上做点什么？";
    let title_font = skin.bold(t.headline);
    let title_w = ui
        .painter()
        .layout_no_wrap(title.to_owned(), title_font.clone(), p.label_primary)
        .size()
        .x;

    let group_w = fish_w + m.s(10.0) + title_w;
    let gx = area.center().x - group_w * 0.5;
    let gy = col.top();

    // 上游 `hero-fish-swim`：悬停在标志上时鲸鱼原地游动，1.6s 一循环，
    // 幅度只有 1px 上下 —— 大屏上恰好是"它活着"而不是"它在晃"。
    let whale_rect = Rect::from_min_size(
        egui::pos2(gx, gy + (headline_h - fish_h) * 0.5),
        Vec2::new(fish_w, fish_h),
    );
    let swim_id = ui.id().with("neo-whale-swim");
    let hit = super::hover_area(
        ui,
        whale_rect.expand(m.s(8.0)),
        ui.id().with("neo-whale-hit"),
    );
    let swim = super::ease(ui, swim_id, if hit.hovered() { 1.0 } else { 0.0 });
    if swim > 0.001 {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let phase = (ui.ctx().time() * (std::f64::consts::TAU / 1.6)) as f32;
    let bob = (phase.sin() * m.s(1.1)) * swim;
    let drift = ((phase * 0.5).sin() * m.s(0.6)) * swim;
    skin.whale.paint(
        ui.painter(),
        whale_rect.translate(egui::vec2(drift, bob)),
        p.label_primary,
    );
    ui.painter().text(
        egui::pos2(gx + fish_w + m.s(10.0), gy + headline_h * 0.5),
        egui::Align2::LEFT_CENTER,
        title,
        title_font,
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
    let chip_w = m.s(14.0 + 16.0 + 4.0) + text_w + m.s(4.0 + 10.0 + 12.0);
    let chip = Rect::from_min_size(
        egui::pos2(col.left() + m.workspace_row_pad(), chip_top),
        Vec2::new(chip_w, chip_h),
    );
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
        painter.squircle_filled(rect, m.radius_workspace(), p.hover);
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
