//! 左侧会话栏。
//!
//! Harness 的侧栏承载会话列表；Neo 在其上加了两层：顶部品牌行（标志 + 版本）、
//! 底部「设置」入口。主题与观看距离这类设置统一收进设置面板 —— 侧栏只保留
//! 高频动作（新对话、切会话、重命名、删除）。
//!
//! # 组件归口
//!
//! 导航行、会话行（含恒显动作钮）、删除确认条、行内重命名容器全部来自
//! [`neo_ui::list`] —— 本文件只做编排（哪一行处于哪种形态、动作落到哪个状态）。

use egui::{Id, Rect, ScrollArea, Ui, Vec2};
use neo_theme::SquirclePaint;
use neo_ui::list::{ConfirmBar, ConfirmOutcome, ListRow, NavItem, RowAction};
use neo_ui::Icon;

use super::{at, inset, section_label, text_center, text_left, Skin};
use crate::state::AppState;

/// 侧栏上发生的用户动作。
#[derive(Default, Clone)]
pub struct Outcome {
    pub new_session: bool,
    /// 点中的会话 id。
    pub open_session: Option<i64>,
    pub open_settings: bool,
    /// 提交的重命名：(会话 id, 新标题)。
    pub renamed: Option<(i64, String)>,
    /// 确认删除的会话 id。
    pub delete_confirmed: Option<i64>,
}

/// 绘制侧栏。
pub fn draw(
    ui: &mut Ui,
    skin: &Skin<'_>,
    rect: Rect,
    state: &mut AppState,
    escape: bool,
) -> Outcome {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let mut out = Outcome::default();

    ui.painter().rect_filled(rect, 0.0, p.sidebar_fill);
    // 与主区之间的那道 1px 分隔，比用投影更贴近规范里的"平面分层"。
    ui.painter().vline(
        rect.right() - 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, p.border_l1),
    );

    let pad = m.s(12.0);
    let content = inset(rect, pad, pad, pad, pad);
    let mut y = content.top();

    // ---- 品牌行 ----
    let brand_h = m.s(30.0);
    let brand = Rect::from_min_size(
        egui::pos2(content.left() + m.s(4.0), y),
        Vec2::new(content.width() - m.s(8.0), brand_h),
    );
    let whale_w = m.s(24.0);
    skin.whale.paint(
        ui.painter(),
        Rect::from_min_size(
            egui::pos2(
                brand.left(),
                brand.center().y - whale_w * 0.5 / skin.whale.aspect(),
            ),
            Vec2::new(whale_w, whale_w / skin.whale.aspect()),
        ),
        p.label_primary,
    );
    let wordmark = Rect::from_min_max(
        egui::pos2(brand.left() + whale_w + m.s(9.0), brand.top()),
        egui::pos2(brand.right(), brand.bottom()),
    );
    text_left(
        ui.painter(),
        wordmark,
        "Neo",
        d.font_bold(d.t().label + m.s(3.0)),
        p.label_primary,
    );

    // 版本徽标（对应 Harness 的 previewBadge：等宽字 + 胶囊）
    let font = d.font_mono(m.s(10.5));
    let vw = ui
        .painter()
        .layout_no_wrap("v0.2.0".to_owned(), font.clone(), p.label_tertiary)
        .size()
        .x;
    let badge = Rect::from_min_size(
        egui::pos2(brand.right() - vw - m.s(14.0), brand.center().y - m.s(9.0)),
        Vec2::new(vw + m.s(14.0), m.s(18.0)),
    );
    ui.painter()
        .squircle(badge, m.s(9.0), p.tip, egui::Stroke::new(0.5, p.border_l1));
    text_center(ui.painter(), badge, "v0.2.0", font, p.label_tertiary);
    y = brand.bottom() + m.s(14.0);

    // ---- 新对话（组件库 NavItem）----
    let new_h = NavItem::height(&d);
    let new_rect = Rect::from_min_size(
        egui::pos2(content.left(), y),
        Vec2::new(content.width(), new_h),
    );
    // NavItem 走布局流，侧栏是绝对定位坐标系 —— 包进 `at()` 定位。
    let new_clicked = at(ui, new_rect, |ui| {
        NavItem::new("新对话", Icon::Plus)
            .id_salt("neo-new-session")
            .show(ui, &d, new_rect.width())
            .clicked()
    });
    if new_clicked {
        out.new_session = true;
    }
    y = new_rect.bottom() + m.s(18.0);

    // ---- 底部：设置入口（先算出来，好给列表留出准确高度）----
    let footer_row_h = m.s(34.0);
    let footer_h = footer_row_h + m.s(8.0) + 1.0 + m.s(10.0);
    let footer_top = content.bottom() - footer_h;

    // ---- 会话列表 ----
    let list_top = y + m.s(24.0);
    if footer_top > list_top + footer_row_h {
        section_label(
            ui.painter(),
            skin,
            Rect::from_min_size(
                egui::pos2(content.left() + m.s(8.0), y),
                Vec2::new(content.width(), m.s(20.0)),
            ),
            "最近会话",
        );
        let list_rect = Rect::from_min_max(
            egui::pos2(content.left(), list_top),
            egui::pos2(content.right(), footer_top - m.s(6.0)),
        );
        let row_out = draw_session_list(ui, skin, list_rect, state, escape);
        out.open_session = row_out.open_session;
        out.renamed = row_out.renamed;
        out.delete_confirmed = row_out.delete_confirmed;
    }

    // ---- 底部：设置 ----
    let mut fy = footer_top;
    super::divider(
        ui.painter(),
        skin,
        Rect::from_min_size(
            egui::pos2(content.left() + m.s(4.0), fy),
            Vec2::new(content.width() - m.s(8.0), 1.0),
        ),
    );
    fy += m.s(11.0);

    let settings_row = Rect::from_min_size(
        egui::pos2(content.left(), fy),
        Vec2::new(content.width(), footer_row_h),
    );
    let sresp = super::tap(ui, settings_row, ui.id().with("neo-row-settings"));
    let sst = super::State::of(&sresp);
    if sst.hovered {
        ui.painter()
            .squircle_filled(settings_row, m.radius_chip(), p.hover);
    }
    let icon_rect = Rect::from_center_size(
        egui::pos2(settings_row.left() + m.s(19.0), settings_row.center().y),
        Vec2::splat(m.s(17.0)),
    );
    Icon::Cog.paint(
        ui.painter(),
        icon_rect,
        if sst.hovered || state.show_settings {
            p.label_primary
        } else {
            p.label_secondary
        },
        1.6,
    );
    text_left(
        ui.painter(),
        inset(settings_row, m.s(36.0), 0.0, m.s(8.0), 0.0),
        "设置",
        skin.prop(skin.t().label),
        if sst.hovered || state.show_settings {
            p.label_primary
        } else {
            p.label_secondary
        },
    );
    if sresp.clicked() {
        out.open_settings = true;
    }

    out
}

/// 会话列表（可滚动）。
///
/// 每行三种形态，绘制全部交组件库：常规（[`ListRow::show_normal`]，恒显
/// 「重命名 / 删除」动作钮）、行内重命名（[`neo_ui::list::rename_frame`] +
/// `TextEdit`）、删除确认（[`neo_ui::list::confirm_row`]）。
fn draw_session_list(
    ui: &mut Ui,
    skin: &Skin<'_>,
    rect: Rect,
    state: &mut AppState,
    escape: bool,
) -> Outcome {
    let mut out = Outcome::default();
    let d = skin.d();
    let m = skin.m();
    let item_h = ListRow::height(&d);
    let gap = m.nav_item_gap();

    // 每帧克隆行数据：行内编辑要写 `state`，与迭代借用冲突。
    // 会话量级是个位数到几十条，克隆开销可忽略。
    let rows = state.sessions.clone();

    at(ui, rect, |ui| {
        ScrollArea::vertical()
            .id_salt("neo-session-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = gap;
                for row in &rows {
                    let active = state.active_session == Some(row.id);

                    // ---- 删除确认态 ----
                    if state.confirming_delete == Some(row.id) {
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::new(rect.width(), item_h),
                            egui::Sense::hover(),
                        );
                        if escape {
                            state.confirming_delete = None;
                            continue;
                        }
                        match neo_ui::list::confirm_row(
                            ui,
                            &d,
                            r,
                            row.id,
                            &ConfirmBar::new("删除这条会话？"),
                        ) {
                            ConfirmOutcome::Confirm => {
                                out.delete_confirmed = Some(row.id);
                                state.confirming_delete = None;
                            }
                            ConfirmOutcome::Cancel => {
                                state.confirming_delete = None;
                            }
                            ConfirmOutcome::None => {}
                        }
                        continue;
                    }

                    // ---- 行内重命名态 ----
                    if state.renaming == Some(row.id) {
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::new(rect.width(), item_h),
                            egui::Sense::hover(),
                        );
                        if escape {
                            state.renaming = None;
                            continue;
                        }
                        ui.painter()
                            .squircle_filled(r, m.radius_chip(), d.p().nav_active);
                        let edit_rect = neo_ui::list::rename_frame(ui, &d, r);
                        let editor = at(ui, edit_rect, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.rename_draft)
                                    .id(Id::new(("neo-rename", row.id)))
                                    .font(d.font(d.t().label))
                                    .text_color(d.p().label_primary)
                                    .frame(egui::Frame::NONE)
                                    .desired_width(edit_rect.width()),
                            )
                        });
                        if state.rename_request_focus {
                            editor.request_focus();
                            state.rename_request_focus = false;
                        }
                        // Enter 或点击别处提交；清空视为取消。
                        if editor.lost_focus() {
                            let title = state.rename_draft.trim().to_owned();
                            state.renaming = None;
                            if !title.is_empty() && title != row.title {
                                out.renamed = Some((row.id, title));
                            }
                        }
                        continue;
                    }

                    // ---- 常规行（组件库：动作钮恒显）----
                    let meta = time_label(row.updated_ms);
                    match ListRow::new(row.id, &row.title, &meta)
                        .active(active)
                        .show_normal(ui, &d, rect.width())
                    {
                        Some(RowAction::Open) => out.open_session = Some(row.id),
                        Some(RowAction::Rename) => {
                            state.renaming = Some(row.id);
                            state.rename_draft = row.title.clone();
                            state.rename_request_focus = true;
                            state.confirming_delete = None;
                        }
                        Some(RowAction::Delete) => {
                            state.confirming_delete = Some(row.id);
                            state.renaming = None;
                        }
                        None => {}
                    }
                }
            });
    });

    out
}

/// 会话列表的副标题：最近更新的时刻。
///
/// # ponytail
/// 时区硬编码 UTC+8 —— 部署环境就是国内教室，不值得为它引入 chrono。
/// 需要正确跨时区显示时换成 `chrono::Local` 或 `jiff`。
fn time_label(ms: i64) -> String {
    // 毫秒 → 当天秒数（东八区），再拆时 / 分。
    let secs = ms.div_euclid(1000) + 8 * 3600;
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
}
