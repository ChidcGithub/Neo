//! 空态（hero）。
//!
//! 对应 Harness 的 `HeroShell`：整块垂直居中，一列内容 ——
//! 鲸鱼标志 + 标题、workspace chip、输入卡。
//!
//! ```text
//! .headline { gap: 10px; font-size: 26px; line-height: 32px; font-weight: 500 }
//! .stack    { flex-direction: column; gap: 12px; max-width: --dsh-composer-card-max-width }
//! ```
//!
//! Neo 在下方追加一行场景卡：教室一体机前，老师需要一个"点一下就开始"的入口，
//! 而不是先想清楚要问什么。

use egui::{Rect, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::{Card, Icon};

use super::{composer, elide, inset, text_left, Skin};
use crate::state::{AppState, SceneIcon, SCENES};

/// hero 上发生的用户动作。
#[derive(Default, Clone, Copy)]
pub struct Outcome {
    pub workspace_clicked: bool,
    pub scene_clicked: Option<usize>,
    pub composer: composer::Outcome,
}

/// 只在每张场景卡都有足够宽度时排成单行；窄屏改为两列。
fn scene_columns(width: f32, min_cell: f32, gap: f32, count: usize) -> usize {
    let count = count.max(1);
    if width >= count as f32 * min_cell + (count - 1) as f32 * gap {
        count
    } else {
        count.min(2)
    }
}

/// 同行卡片等高，并按提示语的实际换行高度为每一行留足空间。
fn scene_h(ui: &Ui, skin: &Skin<'_>, rows: usize, cell_w: f32) -> f32 {
    let m = skin.m();
    let hint_h = SCENES
        .iter()
        .map(|scene| {
            ui.painter()
                .layout(
                    scene.hint.into(),
                    skin.prop(skin.t().caption),
                    skin.p().label_caption,
                    (cell_w - m.s(32.0)).max(1.0),
                )
                .size()
                .y
        })
        .fold(0.0_f32, f32::max);
    let cell_h = m.s(116.0).max(m.s(84.0) + hint_h);
    rows as f32 * cell_h + rows.saturating_sub(1) as f32 * m.s(12.0)
}

/// 绘制空态。
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
    let scenes_gap = m.s(36.0);
    let n = SCENES.len();
    let cols = scene_columns(card_w, m.s(174.0), m.s(12.0), n);
    let rows = n.div_ceil(cols);
    let scene_cell_w = (card_w - m.s(12.0) * (cols - 1) as f32) / cols as f32;
    let scenes_h = scene_h(ui, skin, rows, scene_cell_w);

    let total = headline_h + gap + chip_h + gap + card_h + scenes_gap + scenes_h;
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

    // ---- workspace chip ----
    let chip_top = gy + headline_h + gap;
    let chip = Rect::from_min_size(
        egui::pos2(col.left() + m.workspace_row_pad(), chip_top),
        Vec2::new(m.s(360.0), chip_h),
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

    // ---- 场景卡 ----
    let scenes_top = card.bottom() + scenes_gap;
    let cgap = m.s(12.0);
    let cell_w = (card_w - cgap * (cols as f32 - 1.0)) / cols as f32;
    let cell_h = (scenes_h - cgap * (rows as f32 - 1.0)) / rows as f32;

    for (i, scene) in SCENES.iter().enumerate() {
        let r = i / cols;
        let c = i % cols;
        let rect = Rect::from_min_size(
            egui::pos2(
                col.left() + c as f32 * (cell_w + cgap),
                scenes_top + r as f32 * (cell_h + cgap),
            ),
            Vec2::new(cell_w, cell_h),
        );
        let selected = state.active_scene == Some(i);
        if scene_card(ui, skin, rect, scene, selected) {
            out.scene_clicked = Some(i);
        }
    }

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
    Icon::Folder.paint(painter, icon_r, p.label_primary, 1.6);

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
        m.s(1.4),
    );

    resp.clicked()
}

/// 一张场景卡。
///
/// 表面 + 悬停抬升 + 选中描边全部来自组件库 [`Card`]；本函数只画内容
/// （图标、标题、提示语）。
fn scene_card(
    ui: &Ui,
    skin: &Skin<'_>,
    rect: Rect,
    scene: &crate::state::Scene,
    selected: bool,
) -> bool {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();

    // 悬停态取自卡片的交互结果（Card 内部注册了点击 + 抬升动画）。
    let (rect, resp) = Card::bubble()
        .interactive()
        .selected(selected)
        .id_salt(("neo-scene", scene.title))
        .paint(ui, &d, rect);
    let st = super::State::of(
        resp.as_ref()
            .expect("interactive card always returns a response"),
    );
    let painter = ui.painter();

    let inner = inset(rect, m.s(16.0), m.s(14.0), m.s(16.0), m.s(14.0));
    let icon_box = Rect::from_min_size(inner.min, Vec2::splat(m.s(22.0)));
    let icon_color = if selected || st.hovered {
        p.accent
    } else {
        p.label_secondary
    };
    let icon = match scene.icon {
        SceneIcon::Board => Icon::Board,
        SceneIcon::Checklist => Icon::Checklist,
        SceneIcon::Pen => Icon::Pen,
        SceneIcon::Mic => Icon::Mic,
    };
    icon.paint(painter, icon_box, icon_color, 1.6);

    let title_font = d.font_bold(d.t().label + m.s(1.0));
    painter.text(
        egui::pos2(inner.left(), icon_box.bottom() + m.s(12.0)),
        egui::Align2::LEFT_TOP,
        scene.title,
        title_font,
        p.label_primary,
    );

    let hint_font = d.font(d.t().caption);
    let hint_top = icon_box.bottom() + m.s(12.0) + m.s(22.0);
    let hint_rect = Rect::from_min_max(
        egui::pos2(inner.left(), hint_top),
        egui::pos2(inner.right(), inner.bottom()),
    );
    let galley = painter.layout(
        scene.hint.to_owned(),
        hint_font,
        p.label_caption,
        hint_rect.width(),
    );
    debug_assert!(
        galley.size().y <= hint_rect.height() + 0.5,
        "场景卡提示语溢出"
    );
    painter.galley(hint_rect.min, galley, p.label_caption);

    resp.expect("interactive card always returns a response")
        .clicked()
}

#[cfg(test)]
mod tests {
    use super::scene_columns;

    #[test]
    fn scene_columns_keep_cards_readable() {
        assert_eq!(scene_columns(714.0, 174.0, 12.0, 4), 2);
        assert_eq!(scene_columns(732.0, 174.0, 12.0, 4), 4);
        assert_eq!(scene_columns(400.0, 174.0, 12.0, 1), 1);
    }
}
