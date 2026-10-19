//! MOD 设置页：只读发现 +「允许加载」的启用意图，不启动进程。
//!
//! 能力与限制都是清单**声明值**，不是实际授权 —— 文案必须保持诚实
//! （见 [`crate::mods`] 模块头）。开关意图绑定 `id + revision + capabilities`。

use egui::{Rect, Sense, Vec2};
use neo_tools::mods::Capability;
use neo_ui::{Badge, BadgeTone, Button, Switch};

use super::{
    settings::{hint_row, kv_row, row_divider, section_label_row},
    Skin,
};
use crate::i18n::{tf, tr};
use crate::mods::{ModRow, ModStatus};
use crate::state::AppState;

/// 设置 → MOD 页。首绘触发一次只读扫描（根目录存在才扫）。
pub fn draw(ui: &mut egui::Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    let m = skin.m();
    // 首绘守卫：目录缺失只标记已扫描，不制造敌意错误、不自动重试。
    if !state.mods_scanned && state.mods_scan.is_none() {
        if crate::mods::root().exists() {
            state.start_mods_scan();
        } else {
            state.mods_scanned = true;
            state.mods = Vec::new();
            state.mods_scan_error = None;
        }
    }

    section_label_row(ui, skin, width, tr("MOD 目录"));
    hint_row(
        ui,
        skin,
        width,
        tr("程序同级的 mods 文件夹，或用 NEO_MODS_DIR 环境变量指定；只读扫描，不创建目录。"),
    );
    ui.add_space(m.s(16.0));

    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("已发现的 MOD"));
    // 左：数量；右：重新扫描键（Button 自带 wrap，窄宽不溢出）。
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(m.s(6.0));
        let count = if state.mods.is_empty() {
            tr("未发现 MOD").to_owned()
        } else {
            tf(
                "{count} 个已发现",
                &[("count", state.mods.len().to_string())],
            )
        };
        ui.label(
            egui::RichText::new(count)
                .font(skin.prop(skin.t().label))
                .color(skin.p().label_secondary),
        );
        let rescan = Button::new(tr("重新扫描"))
            .ghost()
            .small()
            .wrap()
            .id_salt("settings-mods-rescan")
            .show(ui, &skin.d());
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new("settings-mods-rescan"), rescan.rect)
        });
        if rescan.clicked() {
            state.start_mods_scan();
        }
    });
    ui.add_space(m.s(6.0));
    let hint = if state.mods_scan.is_some() {
        tr("正在扫描 MOD 目录…").to_owned()
    } else if let Some(error) = &state.mods_scan_error {
        tf(
            "扫描失败：{error}（不显示部分结果）",
            &[("error", error.clone())],
        )
    } else if !crate::mods::root().exists() {
        tr("MOD 目录不存在；将 MOD 包放入程序同级 mods 文件夹后重新扫描").to_owned()
    } else if state.mods.is_empty() {
        tr("未发现 MOD 包").to_owned()
    } else {
        tr("开关只记录允许加载的意图；清单修订或声明能力变化后需重新确认").to_owned()
    };
    hint_row(ui, skin, width, &hint);
    ui.add_space(m.s(8.0));

    let mut actions = Vec::new();
    for (index, row) in state.mods.iter().enumerate() {
        if index > 0 {
            row_divider(ui, skin, width);
        }
        mod_row(ui, skin, width, row, &mut actions);
    }
    for action in actions {
        match action {
            RowAction::Toggle(id) => state.toggle_mod(&id),
            RowAction::Expand(id) => {
                if let Some(row) = state.mods.iter_mut().find(|row| row.manifest.id == id) {
                    row.expanded = !row.expanded;
                }
            }
        }
    }
}

/// 单行产生的动作；循环后再作用到 [`AppState`]，避免与不可变遍历冲突。
enum RowAction {
    Toggle(String),
    Expand(String),
}

/// 「id + 状态徽标 + 开关」一行；展开后列出声明的能力、限制与工具。
fn mod_row(
    ui: &mut egui::Ui,
    skin: &Skin<'_>,
    width: f32,
    row: &ModRow,
    actions: &mut Vec<RowAction>,
) {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let id = row.manifest.id.as_str();
    let (badge_text, badge_tone) = match row.status {
        ModStatus::Off => (tr("未允许"), BadgeTone::Neutral),
        ModStatus::On => (tr("已允许加载"), BadgeTone::Success),
        ModStatus::NeedsReconfirm => (tr("需重新确认"), BadgeTone::Warn),
    };
    let badge = Badge::new(badge_text).tone(badge_tone).prop();
    let badge_w = badge.width(ui, &d);
    let badge_h = m.s(18.0);
    let sw = Switch::size(&d);
    let gap = m.s(8.0);
    let id_budget = (width - sw.x - badge_w - gap * 2.0).max(1.0);
    let id_galley = ui.painter().layout(
        id.to_owned(),
        d.font_mono(d.t().label),
        p.label_primary,
        id_budget,
    );
    let line1_h = id_galley.size().y.max(sw.y).max(badge_h);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, line1_h + m.s(4.0)), Sense::hover());
    let top = rect.top() + m.s(2.0);

    let sw_rect = Rect::from_center_size(
        egui::pos2(rect.right() - sw.x * 0.5, top + line1_h * 0.5),
        sw,
    );
    if Switch::new(row.status == ModStatus::On)
        .id_salt(("neo-mod-switch", id))
        .show_at(ui, &d, sw_rect)
        .clicked()
    {
        actions.push(RowAction::Toggle(id.to_owned()));
    }

    // 徽标贴在 id 之后、开关之前；宽度不够就不画，绝不越界或压住开关。
    let badge_x_max = rect.right() - sw.x - gap - badge_w;
    if badge_x_max >= rect.left() {
        let badge_x = (rect.left() + id_galley.size().x + gap).min(badge_x_max);
        badge.show_at(
            ui,
            &d,
            Rect::from_min_size(
                egui::pos2(badge_x, top + (line1_h - badge_h) * 0.5),
                Vec2::new(badge_w, badge_h),
            ),
        );
    }
    ui.painter().galley(
        egui::pos2(rect.left(), top + (line1_h - id_galley.size().y) * 0.5),
        id_galley,
        p.label_primary,
    );

    if matches!(row.status, ModStatus::NeedsReconfirm) {
        hint_row(ui, skin, width, tr("清单修订或能力已变化，重新打开以重新确认"));
    }

    let summary = tf(
        "修订 {revision} · {tools} 个工具 · {caps} 项声明能力",
        &[
            ("revision", row.manifest.revision.to_string()),
            ("tools", row.manifest.tools.len().to_string()),
            ("caps", row.manifest.capabilities.len().to_string()),
        ],
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(m.s(6.0));
        ui.add(
            egui::Label::new(
                egui::RichText::new(summary)
                    .font(skin.prop(skin.t().caption))
                    .color(p.label_tertiary),
            )
            .wrap(),
        );
        let details = Button::new(if row.expanded { tr("收起") } else { tr("详情") })
            .ghost()
            .small()
            .wrap()
            .id_salt(("neo-mod-details", id))
            .show(ui, &d);
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(egui::Id::new(("neo-mod-details", id)), details.rect)
        });
        if details.clicked() {
            actions.push(RowAction::Expand(id.to_owned()));
        }
    });

    if !row.expanded {
        return;
    }
    ui.add_space(m.s(4.0));
    let status_desc = match row.status {
        ModStatus::Off => tr("记录允许加载该 MOD；当前版本不会启动进程"),
        ModStatus::On => tr("已记录允许加载；当前版本不会启动进程"),
        ModStatus::NeedsReconfirm => tr("清单修订或能力已变化，重新打开以重新确认"),
    };
    hint_row(ui, skin, width, status_desc);
    ui.add_space(m.s(4.0));
    kv_row(ui, skin, width, tr("目录"), &row.directory.display().to_string());
    kv_row(
        ui,
        skin,
        width,
        tr("可执行文件"),
        &row.executable.display().to_string(),
    );
    ui.add_space(m.s(6.0));

    section_label_row(ui, skin, width, tr("声明的能力（非实际授权）"));
    let mut capabilities = row.manifest.capabilities.clone();
    capabilities.sort();
    let caps: Vec<&str> = capabilities
        .iter()
        .map(|capability| capability_label(*capability))
        .collect();
    hint_row(ui, skin, width, &caps.join(" · "));
    ui.add_space(m.s(6.0));

    section_label_row(ui, skin, width, tr("声明的限制（进程层未强制）"));
    let limits = &row.manifest.limits;
    kv_row(ui, skin, width, tr("超时"), &format!("{} ms", limits.timeout_ms));
    kv_row(
        ui,
        skin,
        width,
        tr("内存上限"),
        &format!("{} MiB", limits.memory_mib),
    );
    kv_row(
        ui,
        skin,
        width,
        tr("并发上限"),
        &limits.max_in_flight.to_string(),
    );
    kv_row(
        ui,
        skin,
        width,
        tr("消息上限"),
        &format!("{} B", limits.max_message_bytes),
    );
    kv_row(
        ui,
        skin,
        width,
        tr("结果上限"),
        &format!("{} B", limits.max_result_bytes),
    );
    ui.add_space(m.s(6.0));

    section_label_row(
        ui,
        skin,
        width,
        &tf(
            "工具（{count}）",
            &[("count", row.manifest.tools.len().to_string())],
        ),
    );
    for tool in &row.manifest.tools {
        kv_row(ui, skin, width, &tool.name, &tool.description);
    }
    ui.add_space(m.s(4.0));
}

fn capability_label(capability: Capability) -> &'static str {
    tr(match capability {
        Capability::WorkspaceRead => "读取工作区",
        Capability::WorkspaceWrite => "写入工作区",
        Capability::Network => "网络访问",
        Capability::DesktopRead => "读取屏幕",
        Capability::DesktopInput => "桌面输入",
        Capability::ProcessSpawn => "启动进程",
    })
}
