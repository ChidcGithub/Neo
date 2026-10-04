//! 设置面板（全屏遮罩 + 居中卡片，正式版布局）。
//!
//! 左列：「设置」标题 + 竖排导航（通用 / 外观 / 显示 / 模型 / 关于）；
//! 右列：页标题 + 页描述 + 可滚动内容区。内容超出时滚动，面板尺寸固定，
//! 不再像原型期那样按内容精确预算高度。
//!
//! # 组件归口
//!
//! 面板底、导航项、开关、输入框、键值行、提示行全部来自 [`neo_ui`] —— 本文件
//! 只做排版与业务编排。加新设置项时照抄现有一组即可。

use egui::{Rect, Sense, Ui, Vec2};
use neo_theme::fonts::LoadedFonts;
use neo_ui::{FieldRow, Icon, IconButton, NavItem, Panel, Switch, TextField};

use super::{at, Skin};
use crate::i18n::{set_language, tf, tr, Language};
use crate::state::{AppState, SettingsTab};

/// 面板尺寸。内容区带滚动，尺寸固定；调用方负责按视口收窄。
pub fn panel_size(skin: &Skin<'_>) -> Vec2 {
    let m = skin.m();
    Vec2::new(m.s(720.0), m.s(540.0))
}

/// 档位控件的点击结果。
#[derive(Default, Clone, Copy)]
pub struct ClusterOutcome {
    pub theme: bool,
    pub distance: bool,
}

/// 右上角的档位控件：距离 chip + 主题按钮。
///
/// 直接操作（点一下就切），不弹面板 —— 教室场景下老师站在屏幕前，
/// 一键搞定比"打开设置再选"少两步。想看缩放链路细节再进设置 → 显示。
pub fn profile_cluster(ui: &Ui, skin: &Skin<'_>, rect: Rect, state: &AppState) -> ClusterOutcome {
    let d = skin.d();
    let m = skin.m();
    let h = m.chip_h();
    let mut out = ClusterOutcome::default();

    // 主题按钮（最右）：组件库的抬升圆形钮，图标按当前主题选。
    let icon = match state.theme_mode {
        neo_theme::ThemeMode::Dark => Icon::Moon,
        neo_theme::ThemeMode::Light => Icon::Sun,
    };
    let btn_d = m.s(30.0);
    let btn_center = egui::pos2(rect.right() - btn_d * 0.5, rect.center().y);
    out.theme = IconButton::new(icon)
        .elevated()
        .id_salt("neo-theme-toggle")
        .show_at(ui, &d, btn_center)
        .clicked();

    // 距离 chip：组件库 Chip（无描边，比手搓的描边胶囊更安静）。
    let label = tf(
        "观看距离 · {distance}",
        &[("distance", tr(state.distance.label()).to_owned())],
    );
    let chip_w = neo_ui::Chip::width(&ui.painter(), &d, &label, false);
    let chip = Rect::from_min_size(
        egui::pos2(
            btn_center.x - btn_d * 0.5 - m.s(8.0) - chip_w,
            rect.center().y - h * 0.5,
        ),
        Vec2::new(chip_w, h),
    );
    out.distance = neo_ui::Chip::new(&label)
        .id_salt("neo-distance-chip")
        .show_at(ui, &d, chip)
        .clicked();

    out
}

/// 绘制设置面板。返回 `true` 表示本帧被关闭。
///
/// 页签内的控件直接改 `state`：设置没有需要回滚的中间态，
/// 走一圈「回传动作」反而让调用点更难读。
pub fn panel(
    ui: &mut Ui,
    skin: &Skin<'_>,
    rect: Rect,
    state: &mut AppState,
    viewport_h: f32,
    loaded: &LoadedFonts,
) -> bool {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let mut closed = false;

    // ---- 面板底：投影 + 卡片（组件库 Panel）----
    let inner = Panel::new().paint(ui, &d, rect, m.s(24.0).min(rect.width() * 0.05));

    // 窄窗把导航移到顶部，避免固定侧栏挤掉内容；宽窗导航独立滚动。
    let compact = inner.width() < m.s(560.0);
    let nav_w = m.s(152.0);
    let gap = m.s(16.0);
    let content_rect = if compact {
        let nav_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(32.0)));
        at(ui, nav_rect, |ui| {
            let nav = egui::ComboBox::from_id_salt("settings-compact-nav")
                .selected_text(page_name(state.settings_tab))
                .width((inner.width() - m.s(12.0)).max(0.0))
                .show_ui(ui, |ui| {
                    for &(tab, name) in SettingsTab::ALL {
                        let item = ui.selectable_value(&mut state.settings_tab, tab, tr(name));
                        #[cfg(test)]
                        if tab == SettingsTab::Logs {
                            ui.ctx().data_mut(|data| {
                                data.insert_temp(
                                    egui::Id::new("settings-log-item-probe"),
                                    (item.rect, ui.clip_rect()),
                                )
                            });
                        }
                        let _ = item;
                    }
                });
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("settings-nav-probe"), nav.response.rect)
            });
            let _ = nav;
        });
        Rect::from_min_max(egui::pos2(inner.left(), nav_rect.bottom()), inner.max)
    } else {
        let nav_rect = Rect::from_min_size(inner.min, Vec2::new(nav_w, inner.height()));
        at(ui, nav_rect, |ui| {
            ui.label(egui::RichText::new(tr("设置")).strong());
            ui.add_space(m.s(16.0));
            egui::ScrollArea::vertical()
                .id_salt("settings-navigation")
                .show(ui, |ui| {
                    for &(tab, name) in SettingsTab::ALL {
                        if NavItem::new(tr(name), nav_icon(tab))
                            .active(state.settings_tab == tab)
                            .id_salt(("settings-nav", name))
                            .show(ui, &d, nav_w)
                            .clicked()
                        {
                            state.settings_tab = tab;
                        }
                        ui.add_space(m.s(4.0));
                    }
                });
        });
        Rect::from_min_max(egui::pos2(nav_rect.right() + gap, inner.top()), inner.max)
    };

    // ---- 右：页标题 + 描述 + 滚动内容 ----
    at(ui, content_rect, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let cw = content_rect.width();

        // 标题按实际字高布局，并为关闭按钮单独留出点击区。
        let close_d = m.hit_target(m.s(28.0)).min(cw * 0.5);
        let title = ui.painter().layout(
            page_name(state.settings_tab).to_owned(),
            d.font_bold(d.t().label + m.s(4.0)),
            p.label_primary,
            (cw - close_d - m.s(12.0)).max(1.0),
        );
        let (title_rect, _) =
            ui.allocate_exact_size(Vec2::new(cw, title.size().y.max(close_d)), Sense::hover());
        ui.painter().galley(title_rect.min, title, p.label_primary);
        let close_center = egui::pos2(title_rect.right() - close_d * 0.5, title_rect.center().y);
        let close_rect = Rect::from_min_max(
            egui::pos2(title_rect.right() - close_d, title_rect.top()),
            title_rect.max,
        );
        let close = ui
            .scope(|ui| {
                ui.shrink_clip_rect(close_rect);
                IconButton::new(Icon::Close)
                    .ghost()
                    .label(tr("关闭设置"))
                    .id_salt("neo-settings-close")
                    .show_at(ui, &d, close_center)
            })
            .inner;
        if close.clicked() {
            closed = true;
        }
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("settings-title-probe"),
                (title_rect, close.rect),
            )
        });
        ui.add_space(m.s(8.0));
        // 低矮窗口把说明和完整错误放入同一滚动区，不让固定头部吃掉正文。
        let scroll_intro = content_rect.height() < m.s(360.0);
        if !scroll_intro {
            page_intro(ui, skin, state);
        }
        row_divider(ui, skin, cw);

        // 内容区滚动：行多也不顶破面板（小窗里调用方会把面板收窄）。
        // 滚动状态按页签分开：长页签滚到底切页签不该继承偏移。
        egui::ScrollArea::vertical()
            .id_salt(("neo-settings-tab", state.settings_tab as usize))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                // 给滚动条让位。
                let w = (ui.available_width() - m.s(12.0)).max(1.0);
                ui.set_max_width(w);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("settings-body-probe"), ui.clip_rect())
                });
                if scroll_intro {
                    page_intro(ui, skin, state);
                }
                ui.add_space(m.s(8.0));
                match state.settings_tab {
                    SettingsTab::General => general_tab(ui, skin, w, state),
                    SettingsTab::WakeTest => super::waketest::draw(ui, skin, w, state),
                    SettingsTab::Appearance => appearance_tab(ui, skin, w, state),
                    SettingsTab::Display => display_tab(ui, skin, w, state, viewport_h),
                    SettingsTab::Model => model_tab(ui, skin, w, state),
                    SettingsTab::Memory => memory_tab(ui, skin, w, state),
                    SettingsTab::Logs => super::logs::draw(ui, skin, w),
                    SettingsTab::About => about_tab(ui, skin, w, state, loaded),
                }
            });
    });

    closed
}

fn page_intro(ui: &mut Ui, skin: &Skin<'_>, state: &AppState) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(page_desc(state.settings_tab))
                .font(skin.prop(skin.t().caption))
                .color(skin.p().label_tertiary),
        )
        .wrap(),
    );
    ui.add_space(skin.m().s(10.0));
    if state.preferences_unsaved {
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr("设置尚未保存；安全限制仅本次生效，重启可能恢复旧值。"))
                    .font(skin.prop(skin.t().caption))
                    .color(skin.p().error),
            )
            .wrap(),
        );
        ui.add_space(skin.m().s(10.0));
    }
}

fn page_name(tab: SettingsTab) -> &'static str {
    SettingsTab::ALL
        .iter()
        .find(|(t, _)| *t == tab)
        .map(|(_, n)| tr(n))
        .unwrap_or_else(|| tr("设置"))
}

fn page_desc(tab: SettingsTab) -> &'static str {
    tr(match tab {
        SettingsTab::General => "安全、后台与语音",
        SettingsTab::WakeTest => "本机测试麦克风与唤醒词，不听写、不发送",
        SettingsTab::Appearance => "主题与回复展示",
        SettingsTab::Display => "观看距离与缩放",
        SettingsTab::Model => "接口、密钥与模型列表",
        SettingsTab::Memory => "管理与备份记忆",
        SettingsTab::Logs => "本地诊断，不进入模型上下文",
        SettingsTab::About => "版本、存储与字体",
    })
}

fn nav_icon(tab: SettingsTab) -> Icon {
    match tab {
        SettingsTab::General => Icon::Cog,
        SettingsTab::WakeTest => Icon::Info,
        SettingsTab::Appearance => Icon::Sun,
        SettingsTab::Display => Icon::Board,
        SettingsTab::Model => Icon::Sparkle,
        SettingsTab::Memory => Icon::Checklist,
        SettingsTab::Logs => Icon::Checklist,
        SettingsTab::About => Icon::Info,
    }
}

// ---------------------------------------------------------------------------
// 各页
// ---------------------------------------------------------------------------

/// 通用页：安全策略、后台运行、悬浮按钮与音频功能。
fn general_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    section_label_row(ui, skin, width, tr("语言"));
    let selected = match state.language {
        Language::ZhCn => 0,
        Language::EnUs => 1,
    };
    // 自称名称保持不变，切换后仍能找到自己的语言。
    if let Some(index) = choice_row(
        ui,
        skin,
        width,
        &["中文", "English"],
        selected,
        "settings-language",
    ) {
        state.language = if index == 0 {
            Language::ZhCn
        } else {
            Language::EnUs
        };
        set_language(state.language);
        ui.ctx().request_repaint();
    }
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("安全与权限"));
    let mut safe = state.classroom_safe;
    switch_row(
        ui,
        skin,
        width,
        tr("课堂安全模式"),
        tr("默认开启，限制后台采集和工具权限"),
        &mut safe,
        "neo-set-safe",
    );
    state.set_classroom_safe(safe);
    ui.add(egui::Label::new(tr("非离线模式：问答和文件内容仍可发送给模型，仍可联网读取。开启后暂停语音唤醒、课堂采集和桌面观察，禁止打开、写入、执行。")).wrap());
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        tr("致命启动错误静默退出"),
        tr("仅安全模式下生效，默认关闭"),
        &mut state.silent_startup_errors,
        "neo-set-silent-startup",
    );
    hint_row(
        ui,
        skin,
        width,
        tr("仍记本地日志；普通工具/网络故障不自动退出。关闭此项则弹窗报告致命错误。"),
    );
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("窗口与后台"));
    switch_row(
        ui,
        skin,
        width,
        tr("关闭时最小化到托盘"),
        tr("从托盘菜单重新打开；安全模式下不监听唤醒词"),
        &mut state.minimize_to_tray,
        "neo-set-tray",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        tr("启动时最小化到托盘"),
        tr("需关闭安全模式并启用语音唤醒"),
        &mut state.start_in_tray,
        "neo-set-start-tray",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        tr("桌面悬浮按钮"),
        tr("单击听写，拖动移动，长按不动展开菜单。独立于语音唤醒；安全模式下不采集桌面或麦克风"),
        &mut state.floating_enabled,
        "neo-set-floating",
    );
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("语音与课堂"));
    switch_row(
        ui,
        skin,
        width,
        tr("语音唤醒「Hi, Neo」"),
        tr("开启后监听唤醒词；安全模式下暂停"),
        &mut state.wake_enabled,
        "neo-set-wake",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        tr("课堂总结"),
        tr("应用全屏/最大化时后台截屏分析、录音转写；退出后闲置两分钟弹出总结。记录保存到记忆目录 class/；安全模式下暂停"),
        &mut state.class_enabled,
        "neo-set-class",
    );
    if let Some(status) = &state.class_status {
        ui.add_space(skin.m().s(4.0));
        hint_row(ui, skin, width, status);
    }
}

fn appearance_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    let m = skin.m();
    section_label_row(ui, skin, width, tr("主题"));
    let idx = match state.theme_mode {
        neo_theme::ThemeMode::Dark => 0,
        neo_theme::ThemeMode::Light => 1,
    };
    if let Some(sel) = choice_row(
        ui,
        skin,
        width,
        &[tr("暗色"), tr("亮色")],
        idx,
        "settings-theme",
    ) {
        state.theme_mode = if sel == 0 {
            neo_theme::ThemeMode::Dark
        } else {
            neo_theme::ThemeMode::Light
        };
    }
    ui.add_space(m.s(16.0));

    switch_row(
        ui,
        skin,
        width,
        tr("显示思考过程"),
        tr("适用于推理模型"),
        &mut state.show_reasoning,
        "neo-set-reasoning",
    );
}

fn display_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState, viewport_h: f32) {
    let m = skin.m();
    section_label_row(ui, skin, width, tr("观看距离"));
    let idx = match state.distance {
        neo_theme::Distance::Standard => 0,
        neo_theme::Distance::Classroom => 1,
        neo_theme::Distance::Auditorium => 2,
    };
    if let Some(sel) = choice_row(
        ui,
        skin,
        width,
        &[tr("近距"), tr("教室"), tr("远距")],
        idx,
        "settings-distance",
    ) {
        state.distance = match sel {
            0 => neo_theme::Distance::Standard,
            1 => neo_theme::Distance::Classroom,
            _ => neo_theme::Distance::Auditorium,
        };
    }
    ui.add_space(m.s(16.0));

    // ---- 缩放链路 ----
    section_label_row(ui, skin, width, tr("缩放详情"));
    let rows: [(&str, String); 5] = [
        (tr("视口高"), format!("{viewport_h:.0} pt")),
        (tr("观看距离"), tr(state.distance.label()).to_owned()),
        (tr("距离系数"), format!("{:.2} ×", state.distance.factor())),
        (tr("最终倍率"), format!("{:.2} ×", m.scale())),
        (tr("正文字号"), format!("{:.0} pt", skin.t().body)),
    ];
    for (k, v) in rows {
        kv_row(ui, skin, width, k, &v);
    }
}

fn model_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    let m = skin.m();
    section_label_row(ui, skin, width, tr("接口地址"));
    input_row(ui, skin, width, &mut state.api_base, false, "neo-api-base");
    ui.add_space(m.s(6.0));
    hint_row(
        ui,
        skin,
        width,
        tr("OpenAI 兼容接口，如 https://api.deepseek.com"),
    );
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, tr("API 密钥"));
    input_row(ui, skin, width, &mut state.api_key, true, "neo-api-key");
    ui.add_space(m.s(6.0));
    hint_row(
        ui,
        skin,
        width,
        tr("密钥仅本机保存；认证时发送至所填接口，刷新前请核对地址"),
    );
    ui.add_space(m.s(16.0));

    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("模型与推理"));
    let name = if state.has_models() {
        tf(
            "{name}（{id}）",
            &[
                ("name", state.model_display().to_owned()),
                ("id", state.model_id().to_owned()),
            ],
        )
    } else {
        tr("未选择模型").to_owned()
    };
    kv_row(ui, skin, width, tr("当前模型"), &name);
    ui.add_space(m.s(6.0));
    let switch_hint = if state.model_def().is_some_and(|m| m.reasoning) {
        tr("点击输入卡的模型名循环切换；当前模型默认开启思考")
    } else {
        tr("点击输入卡的模型名循环切换")
    };
    hint_row(ui, skin, width, switch_hint);
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, tr("上下文预算（估算 token）"));
    ui.add(
        egui::DragValue::new(&mut state.context_tokens)
            .range(neo_llm::MIN_CONTEXT_TOKENS..=neo_llm::MAX_CONTEXT_TOKENS)
            .speed(1024.0),
    );
    hint_row(ui, skin, width, tr("默认 1,000,000，按接口与模型能力调整，含输出和工具预留。接近预算时后台摘要，保留原记录；失败不丢弃历史。"));
    hint_row(
        ui,
        skin,
        width,
        tr("每任务最多 500 次工具调用（含错误、拒绝、询问）；续轮与历史压缩不重置计数。"),
    );
    ui.add_space(m.s(16.0));

    // ---- 思考强度：对应请求体顶层的 thinking / reasoning_effort ----
    section_label_row(ui, skin, width, tr("思考强度"));
    let labels: Vec<&str> = neo_llm::Thinking::ALL
        .iter()
        .map(|t| tr(t.label()))
        .collect();
    let sel = neo_llm::Thinking::ALL
        .iter()
        .position(|t| *t == state.thinking)
        .unwrap_or(0);
    if let Some(i) = choice_row(ui, skin, width, &labels, sel, "settings-thinking") {
        state.thinking = neo_llm::Thinking::ALL[i];
    }
    ui.add_space(m.s(6.0));
    hint_row(ui, skin, width, tr(state.thinking.hint()));
    ui.add_space(m.s(16.0));

    // ---- 可选模型：来自模型商的 /models，内置表只是兜底 ----
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, tr("可选模型"));
    // 左：数量；右：刷新键。交给布局系统排，别手量宽度（Button 会自己算内边距）。
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(if state.has_models() {
                tf(
                    "{count} 个可用",
                    &[("count", state.models.len().to_string())],
                )
            } else {
                tr("暂无模型").to_owned()
            })
            .font(skin.prop(skin.t().label))
            .color(skin.p().label_secondary),
        );
        let refresh_width = ui
            .painter()
            .layout_no_wrap(
                tr("从模型商刷新").to_owned(),
                skin.d().font_bold(skin.t().label),
                skin.p().label_primary,
            )
            .size()
            .x
            + m.s(32.0);
        let refresh = if width < refresh_width {
            ui.add(egui::Button::new(tr("从模型商刷新")).wrap())
        } else {
            neo_ui::Button::new(tr("从模型商刷新"))
                .id_salt("settings-model-refresh")
                .elevated()
                .show(ui, &skin.d())
        };
        if refresh.clicked() {
            state.start_model_fetch();
        }
    });
    ui.add_space(m.s(6.0));
    let hint = match (&state.model_fetch, &state.model_fetch_error) {
        (Some(_), _) => tr("正在刷新…").to_owned(),
        (None, Some(e)) if !state.has_models() => tf(
            "刷新失败：{error} · 核对接口与密钥后重试",
            &[("error", e.clone())],
        ),
        (None, Some(e)) => tf("刷新失败：{error}（保留原列表）", &[("error", e.clone())]),
        (None, None) if state.has_models() => {
            tr("模型商列表仅手动刷新；启动（含安全模式）或修改配置不会自动刷新").to_owned()
        }
        (None, None) if state.api_key.trim().is_empty() => {
            tr("填写 API 密钥后，点击「从模型商刷新」").to_owned()
        }
        (None, None) => tr("点击「从模型商刷新」获取列表").to_owned(),
    };
    hint_row(ui, skin, width, &hint);
}

fn about_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState, loaded: &LoadedFonts) {
    let m = skin.m();
    section_label_row(ui, skin, width, tr("关于"));
    let db = state.db_path.as_deref().unwrap_or_else(|| tr("不可用"));
    let rows: [(&str, String); 3] = [
        (tr("版本"), env!("CARGO_PKG_VERSION").to_owned()),
        (
            tr("存储"),
            if state.store_ok {
                tr("已启用").to_owned()
            } else {
                tr("不可用").to_owned()
            },
        ),
        (tr("数据库"), db.to_owned()),
    ];
    for (k, v) in rows {
        kv_row(ui, skin, width, k, &v);
    }
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, tr("软件更新"));
    switch_row(
        ui,
        skin,
        width,
        tr("自动检查更新"),
        tr("启动后联网 GitHub 检查一次；不自动下载或安装"),
        &mut state.auto_check_updates,
        "neo-set-auto-updates",
    );
    let checking = matches!(state.update_status, crate::updates::Status::Checking);
    if ui
        .add_enabled(!checking, egui::Button::new(tr("立即检查")).wrap())
        .clicked()
    {
        state.update_check_requested = true;
    }
    ui.add_space(m.s(8.0));
    match &state.update_status {
        crate::updates::Status::Idle => {
            hint_row(ui, skin, width, tr("尚未检查；手动检查间隔至少 10 秒"))
        }
        crate::updates::Status::Checking => hint_row(ui, skin, width, tr("正在检查 GitHub 更新…")),
        crate::updates::Status::UpToDate => hint_row(ui, skin, width, tr("已是最新发布版本")),
        crate::updates::Status::Available { version, url } => {
            hint_row(
                ui,
                skin,
                width,
                &tf(
                    "新版本 {version}，请查看发布说明并手动下载",
                    &[("version", version.clone())],
                ),
            );
            ui.hyperlink_to(tr("查看 GitHub 发布页面"), url);
        }
        crate::updates::Status::Failed(_) => hint_row(
            ui,
            skin,
            width,
            tr("检查失败，请检查网络后重试；不影响使用"),
        ),
    }
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, tr("字体"));
    let name_of = |path: &Option<String>| -> String {
        match path {
            Some(full) => std::path::Path::new(full)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| full.clone()),
            None => tr("内置回退").to_owned(),
        }
    };
    let fonts: [(&str, String); 4] = [
        (tr("界面"), name_of(&loaded.ui)),
        (tr("中文"), name_of(&loaded.cjk)),
        (tr("粗体"), name_of(&loaded.bold)),
        (tr("等宽"), name_of(&loaded.mono)),
    ];
    for (k, v) in fonts {
        kv_row(ui, skin, width, k, &v);
    }

    if !loaded.has_cjk() {
        ui.add_space(m.s(6.0));
        hint_row(ui, skin, width, tr("缺少中文字体，中文将显示为方块"));
    }
}

/// 文字按剩余宽度换行；窄屏将整组操作移到下一行，不拆散两个按钮。
fn memory_list_row(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    item: &neo_tools::tools::memory::Memory,
) -> (egui::Response, egui::Response) {
    let d = skin.d();
    let m = skin.m();
    let width = width.min(ui.available_width()).max(1.0);
    let gap = m.s(6.0).min(width / 3.0);
    let hit = m.hit_target(m.s(28.0));
    let cell_w = hit.min((width - gap) / 2.0);
    let actions_w = cell_w * 2.0 + gap;
    let inline = width >= actions_w + gap + m.s(160.0);
    let text_w = if inline {
        width - actions_w - gap
    } else {
        width
    };
    let galley = ui.painter().layout(
        format!("#{} · {}", item.id, item.content),
        d.font(d.t().label),
        d.p().label_primary,
        text_w,
    );
    let text_h = galley.size().y;
    let height = if inline {
        text_h.max(hit)
    } else {
        text_h + gap + hit
    };
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let text_rect = Rect::from_min_size(row.min, Vec2::new(text_w, text_h));
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    text_ui.add(egui::Label::new(galley));

    // 独立热区不参与横向流式布局，避免 show_touch 内部 scope 再次推进游标。
    // 极窄/大缩放时只收窄热区，保持既有图标尺寸与样式，并限制绘制/点击范围。
    let actions_top = if inline {
        row.top()
    } else {
        text_rect.bottom() + gap
    };
    let pen_rect = Rect::from_min_size(
        egui::pos2(row.right() - actions_w, actions_top),
        Vec2::new(cell_w, hit),
    );
    let trash_rect = pen_rect.translate(Vec2::new(cell_w + gap, 0.0));
    let mut actions_ui = ui.new_child(egui::UiBuilder::new().max_rect(pen_rect));
    actions_ui.shrink_clip_rect(pen_rect);
    let pen = IconButton::new(Icon::Pen)
        .ghost()
        .label(tr("编辑记忆"))
        .id_salt(("neo-mem-pen", item.id))
        .show_at(&actions_ui, &d, pen_rect.center());
    let mut actions_ui = ui.new_child(egui::UiBuilder::new().max_rect(trash_rect));
    actions_ui.shrink_clip_rect(trash_rect);
    let trash = IconButton::new(Icon::Trash)
        .danger()
        .label(tr("删除记忆"))
        .id_salt(("neo-mem-trash", item.id))
        .show_at(&actions_ui, &d, trash_rect.center());
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(
            egui::Id::new(("neo-memory-actions-probe", item.id)),
            (pen.rect, trash.rect),
        );
        data.insert_temp(
            egui::Id::new(("neo-memory-row-probe", item.id)),
            (row, text_rect),
        );
    });
    (pen, trash)
}

// ConfirmBar 的取消文案固定为中文；在消费端组合相同组件，保留首次防误触与 Esc 取消。
fn memory_confirm(
    ui: &mut Ui,
    skin: &Skin<'_>,
    question: &str,
    memory_id: u64,
    opening: bool,
) -> neo_ui::list::ConfirmResponse {
    let d = skin.d();
    let id = egui::Id::new(("memory-confirm", memory_id));
    let inner = ui.push_id(id, |ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(d.m().s(8.0));
        let question = ui.add(
            egui::Label::new(
                egui::RichText::new(question)
                    .font(d.font(d.t().label))
                    .color(d.p().label_primary),
            )
            .wrap(),
        );
        let button_w = [tr("取消"), tr("删除")].map(|text| {
            d.m().hit_target(
                ui.painter()
                    .layout_no_wrap(
                        text.to_owned(),
                        d.font_bold(d.t().label),
                        d.p().label_primary,
                    )
                    .size()
                    .x
                    + d.m().s(32.0),
            )
        });
        let stacked = button_w[0] + button_w[1] + d.m().s(8.0) > ui.available_width();
        let buttons = ui
            .with_layout(
                if stacked {
                    egui::Layout::top_down(egui::Align::Min)
                } else {
                    egui::Layout::left_to_right(egui::Align::Center)
                },
                |ui| {
                    let cancel = neo_ui::Button::new(tr("取消"))
                        .elevated()
                        .touch_layout()
                        .id_salt(id.with("cancel"))
                        .show(ui, &d);
                    let confirm = neo_ui::Button::new(tr("删除"))
                        .danger()
                        .touch_layout()
                        .id_salt(id.with("confirm"))
                        .show(ui, &d);
                    (cancel, confirm)
                },
            )
            .inner;
        (question, buttons.0, buttons.1)
    });
    let (question, cancel, confirm) = inner.inner;
    if opening {
        cancel.request_focus();
    }
    let escaped = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let outcome = if escaped || (!opening && cancel.clicked()) {
        neo_ui::list::ConfirmOutcome::Cancel
    } else if !opening && confirm.clicked() {
        neo_ui::list::ConfirmOutcome::Confirm
    } else {
        neo_ui::list::ConfirmOutcome::None
    };
    neo_ui::list::ConfirmResponse {
        response: inner.response,
        question,
        cancel,
        confirm,
        outcome,
    }
}

/// 记忆页：AI 跨对话记住的事 —— 查看、行内编辑、删除、手动添加、导入导出。
fn memory_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    memory_tab_with_edit(
        ui,
        skin,
        width,
        state,
        neo_tools::tools::memory::edit_memory,
    );
}

fn memory_tab_with_edit(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    state: &mut AppState,
    edit: impl FnMut(
        u64,
        &str,
    ) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
) {
    memory_tab_with_actions(ui, skin, width, state, edit, |id| {
        neo_tools::tools::memory::forget_memory(&format!("#{id}"))
    });
}

fn memory_tab_with_actions(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    state: &mut AppState,
    mut edit: impl FnMut(
        u64,
        &str,
    ) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
    mut delete: impl FnMut(
        u64,
    )
        -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
) {
    use neo_tools::tools::memory as mem;

    let d = skin.d();
    let m = skin.m();

    let error_id = egui::Id::new("neo-memory-write-error");
    let mut error = ui.ctx().data(|data| data.get_temp::<String>(error_id));
    let owner_id = error_id.with("row");
    let mut error_owner = ui.ctx().data(|data| data.get_temp::<u64>(owner_id));
    let confirm_id = egui::Id::new("neo-memory-delete-confirm");
    let mut confirming = ui
        .ctx()
        .data(|data| data.get_temp::<(u64, bool)>(confirm_id));
    let scroll_id = error_id.with("scroll");
    let scroll_error = ui
        .ctx()
        .data_mut(|data| data.remove_temp::<bool>(scroll_id))
        .unwrap_or(false);

    section_label_row(ui, skin, width, tr("AI 记住的事"));

    if state.memories.is_empty() {
        hint_row(
            ui,
            skin,
            width,
            tr("暂无记忆。对 Neo 说「记住……」，或在下方添加"),
        );
    }

    // 行内动作先记账、循环外落地 —— 避免边遍历边改 Vec / 编辑态。
    enum Act {
        Edit(u64, String),
        RequestDelete(u64),
        Delete(u64),
        Save,
        Cancel,
    }
    let mut act: Option<Act> = None;

    for item in &state.memories {
        if error_owner == Some(item.id) {
            if let Some(message) = &error {
                let notice = neo_ui::InlineNotice::new(("memory-error", item.id), message)
                    .tone(neo_ui::NoticeTone::Error)
                    .show(ui, &d);
                if scroll_error {
                    notice.scroll_to_me(Some(egui::Align::Min));
                }
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new("neo-memory-error-probe"),
                        (notice.rect, ui.clip_rect()),
                    )
                });
                ui.add_space(m.s(6.0));
            }
        }
        if let Some((id, opening)) = confirming.filter(|(id, _)| *id == item.id) {
            let question = tf(
                "删除记忆 #{id}？此操作无法撤销。\n{content}",
                &[("id", id.to_string()), ("content", item.content.clone())],
            );
            let result = memory_confirm(ui, skin, &question, id, opening);
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new("neo-memory-confirm-probe"),
                    (result.cancel.rect, result.confirm.rect),
                )
            });
            confirming = Some((id, false));
            match result.outcome {
                neo_ui::list::ConfirmOutcome::Confirm => act = Some(Act::Delete(id)),
                neo_ui::list::ConfirmOutcome::Cancel => confirming = None,
                neo_ui::list::ConfirmOutcome::None => {}
            }
            ui.add_space(m.s(8.0));
            continue;
        }
        let editing = matches!(&state.memory_editing, Some((id, _)) if *id == item.id);
        if editing {
            // 窄屏让输入与操作分行，避免按钮挤出内容区。
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(m.s(6.0), m.s(6.0));
                let input_w = width;
                if let Some((_, draft)) = state.memory_editing.as_mut() {
                    TextField::new(draft)
                        .id_salt(("neo-mem-edit", item.id))
                        .show(ui, &d, input_w);
                }
                let save = IconButton::new(Icon::Check)
                    .ghost()
                    .label(tr("保存记忆"))
                    .id_salt(("neo-mem-save", item.id))
                    .show_touch(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new("neo-memory-save-probe"), save.rect)
                });
                if save.clicked() {
                    act = Some(Act::Save);
                }
                if IconButton::new(Icon::Close)
                    .ghost()
                    .label(tr("取消编辑记忆"))
                    .id_salt(("neo-mem-cancel", item.id))
                    .show_touch(ui, &d)
                    .clicked()
                {
                    act = Some(Act::Cancel);
                }
            });
            ui.add_space(m.s(6.0));
            continue;
        }

        let (pen, trash) = memory_list_row(ui, skin, width, item);
        if pen.clicked() {
            act = Some(Act::Edit(item.id, item.content.clone()));
        }
        if trash.clicked() {
            act = Some(Act::RequestDelete(item.id));
        }
        ui.add_space(m.s(8.0));
    }

    let acted = act.is_some();
    match act {
        Some(Act::Edit(id, content)) => {
            if let Some((editing_id, _)) = state.memory_editing.as_ref() {
                error_owner = Some(*editing_id);
                error = Some(tr("请先保存或取消编辑；草稿已保留").to_owned());
            } else {
                state.memory_editing = Some((id, content));
            }
        }
        Some(Act::RequestDelete(id)) => confirming = Some((id, true)),
        Some(Act::Delete(id)) => {
            confirming = None;
            error_owner = Some(id);
            match delete(id) {
                Ok(Some(_)) => {
                    error = None;
                    state.memories.retain(|item| item.id != id);
                    state.memories_file_ms = None;
                }
                Ok(None) => error = Some(tr("删除失败：该记忆已不存在").to_owned()),
                Err(e) => error = Some(tf("删除失败：{error}", &[("error", e.message)])),
            }
        }
        Some(Act::Save) => {
            if let Some((id, draft)) = &state.memory_editing {
                error_owner = Some(*id);
                let trimmed = draft.trim();
                if trimmed.is_empty() {
                    error = Some(tr("内容不能为空；删除前请先取消编辑").to_owned());
                } else {
                    match edit(*id, trimmed) {
                        Ok(Some(updated)) => {
                            if let Some(item) =
                                state.memories.iter_mut().find(|item| item.id == updated.id)
                            {
                                *item = updated;
                            }
                            state.memory_editing = None;
                            error = None;
                            state.memories_file_ms = None;
                        }
                        Ok(None) => {
                            error = Some(tr("保存失败：该记忆已不存在，草稿已保留").to_owned())
                        }
                        Err(e) => {
                            error = Some(tf(
                                "保存失败：{error}（草稿已保留）",
                                &[("error", e.message)],
                            ))
                        }
                    }
                }
            }
        }
        Some(Act::Cancel) => {
            state.memory_editing = None;
            error = None;
        }
        None => {}
    }
    ui.ctx().data_mut(|data| {
        if let Some(value) = confirming {
            data.insert_temp(confirm_id, value);
        } else {
            data.remove::<(u64, bool)>(confirm_id);
        }
        if acted && error.is_some() && error_owner.is_some() {
            data.insert_temp(scroll_id, true);
        }
    });

    // 添加与备份。
    ui.add_space(m.s(10.0));
    section_label_row(ui, skin, width, tr("添加与备份"));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
        let input_w = width;
        input_row(
            ui,
            skin,
            input_w,
            &mut state.memory_draft,
            false,
            "neo-mem-new",
        );
        if neo_ui::Button::new(tr("记下"))
            .id_salt("settings-memory-add")
            .touch_layout()
            .elevated()
            .show(ui, &d)
            .clicked()
        {
            let content = state.memory_draft.trim().to_owned();
            if !content.is_empty() {
                match mem::add_memory(&content) {
                    Ok(_) => {
                        state.memory_draft.clear();
                        error = None;
                        reload_memories(state);
                    }
                    Err(e) => {
                        error_owner = None;
                        error = Some(tf(
                            "添加失败：{error}（草稿已保留）",
                            &[("error", e.message)],
                        ));
                    }
                }
            }
        }
    });
    if error_owner.is_none() {
        if let Some(message) = &error {
            neo_ui::InlineNotice::new("memory-add-error", message)
                .tone(neo_ui::NoticeTone::Error)
                .show(ui, &d);
        }
    }
    ui.ctx().data_mut(|data| {
        if let Some(owner) = error_owner.filter(|_| error.is_some()) {
            data.insert_temp(owner_id, owner);
        } else {
            data.remove::<u64>(owner_id);
        }
        if let Some(message) = error {
            data.insert_temp(error_id, message);
        } else {
            data.remove::<String>(error_id);
        }
    });
    ui.add_space(m.s(8.0));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
        if neo_ui::Button::new(tr("导入…"))
            .id_salt("settings-memory-import")
            .touch_layout()
            .ghost()
            .show(ui, &d)
            .clicked()
        {
            state.import_memories_dialog(ui.ctx());
        }
        if neo_ui::Button::new(tr("导出…"))
            .id_salt("settings-memory-export")
            .touch_layout()
            .ghost()
            .show(ui, &d)
            .clicked()
        {
            state.export_memories_dialog(ui.ctx());
        }
    });
    ui.add_space(m.s(6.0));
    hint_row(
        ui,
        skin,
        width,
        &tf(
            "记忆保存在 {path}",
            &[("path", mem::memories_path().display().to_string())],
        ),
    );
}

#[cfg(test)]
#[path = "settings_ui_regression.rs"]
mod ui_regression;

/// 本地刚写完记忆文件：清掉 mtime 缓存强制重读
/// （mtime 粒度在某些文件系统上可能骗过沿检）。
fn reload_memories(state: &mut AppState) {
    state.memories_file_ms = None;
    state.maybe_reload_memories();
}

// ---------------------------------------------------------------------------
// 行控件 —— 全部是 neo-ui 组件的薄包装，页面层不再自己画输入框
// ---------------------------------------------------------------------------

/// 「标题 + 描述」居左、开关居右的设置行（布尔项的标准形态）。
fn switch_row(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    title: &str,
    desc: &str,
    on: &mut bool,
    salt: &str,
) {
    let d = skin.d();
    let p = skin.p();
    let m = skin.m();
    let sw = Switch::size(&d);
    let tw = (width - sw.x - m.s(12.0)).max(1.0);
    let title = ui.painter().layout(
        title.to_owned(),
        d.font_bold(d.t().label),
        p.label_primary,
        tw,
    );
    let desc = ui.painter().layout(
        desc.to_owned(),
        skin.prop(skin.t().caption),
        p.label_tertiary,
        width,
    );
    let title_h = title.size().y.max(sw.y);
    let h = title_h + desc.size().y + m.s(18.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());

    // 右侧开关：组件只负责画与报点击，取值翻转在这里做。
    let sw_rect = Rect::from_center_size(
        egui::pos2(
            rect.right() - sw.x * 0.5,
            rect.top() + m.s(6.0) + title_h * 0.5,
        ),
        sw,
    );
    #[cfg(test)]
    if salt == "neo-set-floating" {
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new("settings-floating-probe"), sw_rect));
    }
    if Switch::new(*on)
        .id_salt(salt)
        .show_at(ui, &d, sw_rect)
        .clicked()
    {
        *on = !*on;
    }

    ui.painter()
        .galley(rect.min + egui::vec2(0.0, m.s(6.0)), title, p.label_primary);
    ui.painter().galley(
        rect.min + egui::vec2(0.0, title_h + m.s(12.0)),
        desc,
        p.label_tertiary,
    );
}

/// 行间细分隔线。
fn row_divider(ui: &mut Ui, skin: &Skin<'_>, width: f32) {
    let m = skin.m();
    ui.add_space(m.s(2.0));
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, m.s(6.0)), Sense::hover());
    ui.painter().hline(
        rect.left()..=rect.right(),
        rect.center().y,
        egui::Stroke::new(1.0, skin.p().border_l1),
    );
    ui.add_space(m.s(2.0));
}

/// 小标题随字体实际高度换行，避免放大后与下一行相撞。
pub(super) fn section_label_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, label: &str) {
    let galley = ui.painter().layout(
        label.to_owned(),
        skin.d().font_bold(skin.t().caption),
        skin.p().label_secondary,
        width.max(1.0),
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, galley.size().y), Sense::hover());
    ui.painter()
        .galley(rect.min, galley, skin.p().label_secondary);
    ui.add_space(skin.m().s(8.0));
}

/// 能放下才均分；否则改为可换行的独立选项，不缩小字体或隐藏选项。
fn choice_row(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    labels: &[&str],
    selected: usize,
    salt: &str,
) -> Option<usize> {
    let d = skin.d();
    let m = skin.m();
    let widest = labels
        .iter()
        .flat_map(|label| {
            [d.font(d.t().caption), d.font_bold(d.t().caption)].map(|font| {
                ui.painter()
                    .layout_no_wrap((*label).to_owned(), font, skin.p().label_primary)
                    .size()
                    .x
            })
        })
        .fold(0.0_f32, f32::max);
    if width >= (widest + m.s(20.0)) * labels.len() as f32 + m.s(4.0) {
        let top = ui.cursor().top();
        let result = ui
            .push_id(salt, |ui| {
                neo_ui::Segmented::new(labels, selected)
                    .id_salt(salt)
                    .show(ui, &d, width)
            })
            .inner;
        #[cfg(test)]
        for i in 0..labels.len() {
            let segment_w = (width - m.s(4.0)) / labels.len() as f32;
            let rect = Rect::from_min_size(
                egui::pos2(
                    ui.min_rect().left() + m.s(2.0) + segment_w * i as f32,
                    top + m.s(2.0),
                ),
                Vec2::new(segment_w, m.s(28.0)),
            );
            ui.ctx()
                .data_mut(|data| data.insert_temp(egui::Id::new((salt, i)), rect));
        }
        let _ = top;
        result
    } else {
        let mut result = None;
        ui.push_id(salt, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(m.s(6.0));
                for (i, label) in labels.iter().enumerate() {
                    let response = ui.add(
                        egui::Button::new(egui::RichText::new(*label).font(d.font(d.t().caption)))
                            .selected(i == selected)
                            .wrap(),
                    );
                    #[cfg(test)]
                    ui.ctx()
                        .data_mut(|data| data.insert_temp(egui::Id::new((salt, i)), response.rect));
                    if response.clicked() {
                        result = Some(i);
                    }
                }
            });
        });
        result
    }
}

/// 单行输入框 —— [`neo_ui::TextField`]：容器、聚焦环、密码掩码都由组件库负责。
///
/// `salt` 必须是常量（不能随内容变），否则每次按键 id 变化、焦点丢失。
fn input_row(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    value: &mut String,
    secret: bool,
    salt: &str,
) {
    let d = skin.d();
    let mut tf = TextField::new(value).id_salt(salt);
    if secret {
        tf = tf.secret(true);
    }
    let response = tf.show(ui, &d, width);
    #[cfg(test)]
    ui.ctx().data_mut(|data| {
        data.insert_temp(egui::Id::new(("settings-input-probe", salt)), response.rect)
    });
    let _ = response;
}

/// 安全与操作提示必须完整换行，不能依赖悬停才能读全。
fn hint_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, text: &str) {
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .font(skin.prop(skin.t().caption))
                    .color(skin.p().label_caption),
            )
            .wrap(),
        );
    });
}

/// 短值保留紧凑对齐；长值改为标签在上、全文在下，触屏也能读全。
pub(super) fn kv_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, key: &str, value: &str) {
    let d = skin.d();
    let key_size = ui
        .painter()
        .layout_no_wrap(
            key.to_owned(),
            d.font(d.t().caption),
            skin.p().label_caption,
        )
        .size();
    let value_size = ui
        .painter()
        .layout_no_wrap(
            value.to_owned(),
            d.font_mono(d.t().caption),
            skin.p().label_secondary,
        )
        .size();
    if !value.contains(['\r', '\n'])
        && key_size.x <= width * 0.4
        && key_size.x + skin.m().s(16.0) + value_size.x <= width
        && key_size.y.max(value_size.y) <= FieldRow::height(&d)
    {
        FieldRow::new(key, value).show(ui, &d, width);
    } else {
        ui.scope(|ui| {
            ui.set_max_width(width);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(key)
                        .font(d.font(d.t().caption))
                        .color(skin.p().label_caption),
                )
                .wrap(),
            );
            ui.add_space(skin.m().s(4.0));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(value)
                        .font(d.font_mono(d.t().caption))
                        .color(skin.p().label_secondary),
                )
                .wrap(),
            );
            ui.add_space(skin.m().s(10.0));
        });
    }
}
