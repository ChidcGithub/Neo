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

use super::{at, segmented, Skin};
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
    let label = format!("观看距离 · {}", state.distance.label());
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
        let nav_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(42.0)));
        at(ui, nav_rect, |ui| {
            let nav = egui::ComboBox::from_id_salt("settings-compact-nav")
                .selected_text(page_name(state.settings_tab))
                .width((inner.width() - m.s(12.0)).max(0.0))
                .show_ui(ui, |ui| {
                    for &(tab, name) in SettingsTab::ALL {
                        let item = ui.selectable_value(&mut state.settings_tab, tab, name);
                        #[cfg(test)]
                        if tab == SettingsTab::Logs {
                            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("settings-log-item-probe"), (item.rect, ui.clip_rect())));
                        }
                        let _ = item;
                    }
                });
            #[cfg(test)]
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("settings-nav-probe"), nav.response.rect));
            let _ = nav;
        });
        Rect::from_min_max(egui::pos2(inner.left(), nav_rect.bottom()), inner.max)
    } else {
        let nav_rect = Rect::from_min_size(inner.min, Vec2::new(nav_w, inner.height()));
        at(ui, nav_rect, |ui| {
            ui.label(egui::RichText::new("设置").strong());
            ui.add_space(m.s(16.0));
            egui::ScrollArea::vertical().id_salt("settings-navigation").show(ui, |ui| {
                for &(tab, name) in SettingsTab::ALL {
                    if NavItem::new(name, nav_icon(tab))
                        .active(state.settings_tab == tab)
                        .id_salt(("settings-nav", name))
                        .show(ui, &d, nav_w).clicked()
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
        let close_d = m.s(30.0);
        let title = ui.painter().layout(
            page_name(state.settings_tab).to_owned(),
            d.font_bold(d.t().label + m.s(4.0)),
            p.label_primary,
            (cw - close_d - m.s(12.0)).max(1.0),
        );
        let (title_rect, _) = ui.allocate_exact_size(
            Vec2::new(cw, title.size().y.max(close_d)), Sense::hover());
        ui.painter().galley(title_rect.min, title, p.label_primary);
        let close_center = egui::pos2(title_rect.right() - close_d * 0.5, title_rect.center().y);
        if IconButton::new(Icon::Close)
            .ghost()
            .id_salt("neo-settings-close")
            .show_at(ui, &d, close_center)
            .clicked()
        {
            closed = true;
        }
        #[cfg(test)]
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("settings-title-probe"),
            (title_rect, Rect::from_center_size(close_center, Vec2::splat(close_d)))));
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
            .id_salt(("neo-settings-tab", page_name(state.settings_tab)))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                // 给滚动条让位。
                let w = (ui.available_width() - m.s(12.0)).max(1.0);
                ui.set_max_width(w);
                #[cfg(test)]
                ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("settings-body-probe"), ui.clip_rect()));
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
    ui.add(egui::Label::new(egui::RichText::new(page_desc(state.settings_tab))
        .font(skin.prop(skin.t().caption)).color(skin.p().label_tertiary)).wrap());
    ui.add_space(skin.m().s(10.0));
    if state.preferences_unsaved {
        ui.add(egui::Label::new(egui::RichText::new(
            "设置尚未保存；安全限制仅对本次运行生效，重启可能恢复旧值。",
        ).font(skin.prop(skin.t().caption)).color(skin.p().error)).wrap());
        ui.add_space(skin.m().s(10.0));
    }
}

fn page_name(tab: SettingsTab) -> &'static str {
    SettingsTab::ALL
        .iter()
        .find(|(t, _)| *t == tab)
        .map(|(_, n)| *n)
        .unwrap_or("设置")
}

fn page_desc(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::General => "后台运行与语音唤醒",
        SettingsTab::WakeTest => "本机观测麦克风与唤醒词，不听写、不发送",
        SettingsTab::Appearance => "主题与回复展示",
        SettingsTab::Display => "观看距离与缩放链路",
        SettingsTab::Model => "接口、密钥与模型列表",
        SettingsTab::Memory => "AI 记住的事：查看、修改、导入导出",
        SettingsTab::Logs => "本地诊断，不进入模型上下文",
        SettingsTab::About => "版本、存储与字体装配",
    }
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

/// 通用页：后台运行、启动即后台与语音唤醒三个开关。
fn general_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    section_label_row(ui, skin, width, "安全与权限");
    let mut safe = state.classroom_safe;
    switch_row(ui, skin, width, "课堂安全模式", "默认开启，限制后台采集和工具权限", &mut safe, "neo-set-safe");
    state.set_classroom_safe(safe);
    ui.add(egui::Label::new("这不是离线模式：普通问答和用户文件内容仍可发送给模型，联网读取仍可用。开启时暂停语音唤醒、课堂采集和桌面观察，禁止打开、写入、执行操作。").wrap());
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, "窗口与后台");
    switch_row(
        ui,
        skin,
        width,
        "关闭时最小化到托盘",
        "关闭后进入托盘，可从托盘菜单重新打开；安全模式下不监听唤醒词",
        &mut state.minimize_to_tray,
        "neo-set-tray",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        "启动时最小化到托盘",
        "仅在关闭安全模式且启用语音唤醒时生效",
        &mut state.start_in_tray,
        "neo-set-start-tray",
    );
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, "语音与课堂");
    switch_row(
        ui,
        skin,
        width,
        "语音唤醒「Hi, Neo」",
        "安全模式下暂停；关闭安全模式后按此偏好监听唤醒词",
        &mut state.wake_enabled,
        "neo-set-wake",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        "课堂总结",
        "应用全屏/最大化（如课件放映）时后台截屏分析 + 录音转写；退出后两分钟无操作生成总结，从屏幕上方弹出。记录存进分记忆（记忆目录下的 class/）",
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
    section_label_row(ui, skin, width, "主题");
    let idx = match state.theme_mode {
        neo_theme::ThemeMode::Dark => 0,
        neo_theme::ThemeMode::Light => 1,
    };
    if let Some(sel) = choice_row(ui, skin, width, &["暗色", "亮色"], idx, "settings-theme") {
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
        "显示思考过程",
        "推理模型（DeepSeek-R1）的思考过程是否展示",
        &mut state.show_reasoning,
        "neo-set-reasoning",
    );
}

fn display_tab(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    state: &mut AppState,
    viewport_h: f32,
) {
    let m = skin.m();
    section_label_row(ui, skin, width, "观看距离");
    let idx = match state.distance {
        neo_theme::Distance::Standard => 0,
        neo_theme::Distance::Classroom => 1,
        neo_theme::Distance::Auditorium => 2,
    };
    if let Some(sel) = choice_row(ui, skin, width, &["近距", "教室", "远距"], idx, "settings-distance") {
        state.distance = match sel {
            0 => neo_theme::Distance::Standard,
            1 => neo_theme::Distance::Classroom,
            _ => neo_theme::Distance::Auditorium,
        };
    }
    ui.add_space(m.s(16.0));

    // ---- 缩放链路 ----
    section_label_row(ui, skin, width, "缩放链路");
    let rows: [(&str, String); 5] = [
        ("视口高", format!("{viewport_h:.0} pt")),
        ("观看距离", state.distance.label().to_owned()),
        ("距离系数", format!("{:.2} ×", state.distance.factor())),
        ("最终倍率", format!("{:.2} ×", m.scale())),
        ("正文字号", format!("{:.0} pt", skin.t().body)),
    ];
    for (k, v) in rows {
        kv_row(ui, skin, width, k, &v);
    }
}

fn model_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    let m = skin.m();
    section_label_row(ui, skin, width, "接口地址");
    input_row(ui, skin, width, &mut state.api_base, false, "neo-api-base");
    ui.add_space(m.s(6.0));
    hint_row(ui, skin, width, "OpenAI 兼容接口，如 https://api.deepseek.com");
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, "API 密钥");
    input_row(ui, skin, width, &mut state.api_key, true, "neo-api-key");
    ui.add_space(m.s(6.0));
    hint_row(ui, skin, width, "密钥仅在本机持久化；请求认证时会发送给所配置的接口，请核对地址后再刷新");
    ui.add_space(m.s(16.0));

    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, "模型与推理");
    let name = if state.has_models() {
        format!("{}（{}）", state.model_display(), state.model_id())
    } else {
        "未选择模型".to_owned()
    };
    kv_row(ui, skin, width, "当前模型", &name);
    ui.add_space(m.s(6.0));
    let switch_hint = if state.model_def().is_some_and(|m| m.reasoning) {
        "在输入卡的模型选择器里切换（点一下循环）· 该模型默认开启思考"
    } else {
        "在输入卡的模型选择器里切换（点一下循环）"
    };
    hint_row(ui, skin, width, switch_hint);
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, "上下文预算（估算 token）");
    ui.add(egui::DragValue::new(&mut state.context_tokens)
        .range(neo_llm::MIN_CONTEXT_TOKENS..=neo_llm::MAX_CONTEXT_TOKENS).speed(1024.0));
    hint_row(ui, skin, width, "默认 1,000,000，请依接口与模型实际能力调整；含输出和工具预留。接近预算时后台摘要，原记录保留；摘要失败不会静默丢弃历史。");
    hint_row(ui, skin, width, "每个用户任务最多执行 500 次工具调用（含错误、拒绝与询问），跨续轮和历史压缩累计。");
    ui.add_space(m.s(16.0));

    // ---- 思考强度：对应请求体顶层的 thinking / reasoning_effort ----
    section_label_row(ui, skin, width, "思考强度");
    let labels: Vec<&str> = neo_llm::Thinking::ALL.iter().map(|t| t.label()).collect();
    let sel = neo_llm::Thinking::ALL
        .iter()
        .position(|t| *t == state.thinking)
        .unwrap_or(0);
    if let Some(i) = choice_row(ui, skin, width, &labels, sel, "settings-thinking") {
        state.thinking = neo_llm::Thinking::ALL[i];
    }
    ui.add_space(m.s(6.0));
    hint_row(ui, skin, width, state.thinking.hint());
    ui.add_space(m.s(16.0));

    // ---- 可选模型：来自模型商的 /models，内置表只是兜底 ----
    row_divider(ui, skin, width);
    section_label_row(ui, skin, width, "可选模型");
    // 左：数量；右：刷新键。交给布局系统排，别手量宽度（Button 会自己算内边距）。
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(if state.has_models() {
                format!("{} 个可用", state.models.len())
            } else {
                "还没有模型".to_owned()
            })
            .font(skin.prop(skin.t().label))
            .color(skin.p().label_secondary),
        );
        let refresh = if width < m.s(220.0) {
            ui.add(egui::Button::new("从模型商刷新").wrap())
        } else {
            neo_ui::Button::new("从模型商刷新").elevated().show(ui, &skin.d())
        };
        if refresh.clicked() {
            state.start_model_fetch();
        }
    });
    ui.add_space(m.s(6.0));
    let hint = match (&state.model_fetch, &state.model_fetch_error) {
        (Some(_), _) => "正在拉取…".to_owned(),
        (None, Some(e)) if !state.has_models() => {
            format!("拉取失败：{e} · 检查接口地址与密钥后重试")
        }
        (None, Some(e)) => format!("上次刷新失败：{e}（仍在用上次拉到的列表）"),
        (None, None) if state.has_models() => {
            "列表来自模型商；启动（含安全模式）和编辑配置均不自动刷新，请核对后手动刷新".to_owned()
        }
        (None, None) if state.api_key.trim().is_empty() => {
            "先填上面的 API 密钥，再点「从模型商刷新」".to_owned()
        }
        (None, None) => "点「从模型商刷新」从 /models 拉取".to_owned(),
    };
    hint_row(ui, skin, width, &hint);
}

fn about_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &AppState, loaded: &LoadedFonts) {
    let m = skin.m();
    section_label_row(ui, skin, width, "关于");
    let db = state.db_path.as_deref().unwrap_or("不可用");
    let rows: [(&str, String); 3] = [
        ("版本", env!("CARGO_PKG_VERSION").to_owned()),
        (
            "存储",
            if state.store_ok {
                "已启用".to_owned()
            } else {
                "不可用".to_owned()
            },
        ),
        ("数据库", db.to_owned()),
    ];
    for (k, v) in rows {
        kv_row(ui, skin, width, k, &v);
    }
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, "字体装配");
    let name_of = |path: &Option<String>| -> String {
        match path {
            Some(full) => std::path::Path::new(full)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| full.clone()),
            None => "内置回退".to_owned(),
        }
    };
    let fonts: [(&str, String); 4] = [
        ("界面", name_of(&loaded.ui)),
        ("中文", name_of(&loaded.cjk)),
        ("粗体", name_of(&loaded.bold)),
        ("等宽", name_of(&loaded.mono)),
    ];
    for (k, v) in fonts {
        kv_row(ui, skin, width, k, &v);
    }

    if !loaded.has_cjk() {
        ui.add_space(m.s(6.0));
        hint_row(ui, skin, width, "未找到中文字体，界面中文会显示为方块");
    }
}

/// 记忆页：AI 跨对话记住的事 —— 查看、行内编辑、删除、手动添加、导入导出。
fn memory_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    memory_tab_with_edit(ui, skin, width, state, neo_tools::tools::memory::edit_memory);
}

fn memory_tab_with_edit(
    ui: &mut Ui,
    skin: &Skin<'_>,
    width: f32,
    state: &mut AppState,
    mut edit: impl FnMut(u64, &str) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
) {
    use neo_tools::tools::memory as mem;

    let d = skin.d();
    let p = skin.p();
    let m = skin.m();

    let error_id = egui::Id::new("neo-memory-write-error");
    let mut error = ui.ctx().data(|data| data.get_temp::<String>(error_id));

    section_label_row(ui, skin, width, "AI 记住的事");

    if state.memories.is_empty() {
        hint_row(
            ui,
            skin,
            width,
            "还没有记忆。对话里告诉 Neo「记住……」，或在下面手动添加",
        );
    }

    // 行内动作先记账、循环外落地 —— 避免边遍历边改 Vec / 编辑态。
    enum Act {
        Edit(u64, String),
        Delete(u64),
        Save,
        Cancel,
    }
    let mut act: Option<Act> = None;

    for item in &state.memories {
        let editing = matches!(&state.memory_editing, Some((id, _)) if *id == item.id);
        if editing {
            // 窄屏让输入与操作分行，避免按钮挤出内容区。
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(m.s(6.0), m.s(6.0));
                let input_w = if width < m.s(240.0) { width }
                    else { (width - m.s(30.0) * 2.0 - m.s(12.0)).max(0.0) };
                if let Some((_, draft)) = state.memory_editing.as_mut() {
                    TextField::new(draft)
                        .id_salt(("neo-mem-edit", item.id))
                        .show(ui, &d, input_w);
                }
                let save = IconButton::new(Icon::Check)
                    .ghost()
                    .id_salt(("neo-mem-save", item.id))
                    .show(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-memory-save-probe"), save.rect));
                if save.clicked() {
                    act = Some(Act::Save);
                }
                if IconButton::new(Icon::Close)
                    .ghost()
                    .id_salt(("neo-mem-cancel", item.id))
                    .show(ui, &d)
                    .clicked()
                {
                    act = Some(Act::Cancel);
                }
            });
            ui.add_space(m.s(6.0));
            continue;
        }

        // 常规态：#id + 内容（超长截断），右侧恒显 编辑/删除（教室一体机没有 hover）。
        // 行体不可点 —— 整行刷 hover 底色是在承诺点击行为，别画（项目规矩：
        // 「给没有点击行为的控件画 hover 态是在撒谎」）。
        let (rect, _resp) = ui.allocate_exact_size(Vec2::new(width, m.s(36.0)), Sense::hover());

        let btn_d = m.s(26.0);
        let trash_center = egui::pos2(rect.right() - btn_d * 0.5, rect.center().y);
        let pen_center = egui::pos2(rect.right() - btn_d * 1.5 - m.s(4.0), rect.center().y);
        if IconButton::new(Icon::Pen)
            .ghost()
            .id_salt(("neo-mem-pen", item.id))
            .show_at(ui, &d, pen_center)
            .clicked()
        {
            act = Some(Act::Edit(item.id, item.content.clone()));
        }
        if IconButton::new(Icon::Trash)
            .danger()
            .id_salt(("neo-mem-trash", item.id))
            .show_at(ui, &d, trash_center)
            .clicked()
        {
            act = Some(Act::Delete(item.id));
        }

        // 文字区给按钮让位；内容里的换行压成空格，保证单行不顶破行高。
        let text_rect = Rect::from_min_max(
            rect.min + egui::vec2(m.s(8.0), 0.0),
            egui::pos2(pen_center.x - btn_d * 0.5 - m.s(8.0), rect.bottom()),
        );
        let id_font = skin.prop(skin.t().caption);
        let id_text = format!("#{}", item.id);
        let id_w = ui
            .painter()
            .layout_no_wrap(id_text.clone(), id_font.clone(), p.label_caption)
            .size()
            .x;
        ui.painter().text(
            text_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            id_text,
            id_font,
            p.label_caption,
        );
        let body_rect = Rect::from_min_max(
            egui::pos2(text_rect.left() + id_w + m.s(8.0), text_rect.top()),
            text_rect.max,
        );
        let font = d.font(d.t().label);
        let flat = item.content.replace(['\r', '\n'], " ");
        let shown = neo_ui::elide(ui.painter(), &flat, &font, body_rect.width());
        ui.painter().text(
            body_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            shown,
            font,
            p.label_primary,
        );
        ui.add_space(m.s(2.0));
    }

    match act {
        Some(Act::Edit(id, content)) => {
            if state.memory_editing.is_some() {
                error = Some("请先保存或取消当前编辑，草稿已保留".to_owned());
            } else {
                state.memory_editing = Some((id, content));
            }
        }
        Some(Act::Delete(id)) => match mem::forget_memory(&format!("#{id}")) {
            Ok(Some(_)) => {
                error = None;
                reload_memories(state);
            }
            Ok(None) => error = Some("删除失败：该记忆已不存在".to_owned()),
            Err(e) => error = Some(format!("删除失败：{}", e.message)),
        },
        Some(Act::Save) => {
            if let Some((id, draft)) = &state.memory_editing {
                let trimmed = draft.trim();
                if trimmed.is_empty() {
                    error = Some("记忆内容不能为空；如需删除请取消编辑后使用删除按钮".to_owned());
                } else {
                    match edit(*id, trimmed) {
                        Ok(Some(_)) => {
                            state.memory_editing = None;
                            error = None;
                            reload_memories(state);
                        }
                        Ok(None) => error = Some("保存失败：该记忆已不存在，草稿已保留".to_owned()),
                        Err(e) => error = Some(format!("保存失败：{}（草稿已保留）", e.message)),
                    }
                }
            }
        }
        Some(Act::Cancel) => state.memory_editing = None,
        None => {}
    }

    // 添加与备份。
    ui.add_space(m.s(10.0));
    section_label_row(ui, skin, width, "添加与备份");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
        let input_w = if width < m.s(240.0) { width }
            else { (width - m.s(64.0) - m.s(8.0)).max(0.0) };
        input_row(ui, skin, input_w, &mut state.memory_draft, false, "neo-mem-new");
        if neo_ui::Button::new("记下")
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
                    Err(e) => error = Some(format!("添加失败：{}（草稿已保留）", e.message)),
                }
            }
        }
    });
    if let Some(message) = &error {
        ui.add(egui::Label::new(egui::RichText::new(message).color(p.error)).wrap());
    }
    ui.ctx().data_mut(|data| {
        if let Some(message) = error {
            data.insert_temp(error_id, message);
        } else {
            data.remove::<String>(error_id);
        }
    });
    ui.add_space(m.s(8.0));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
        if neo_ui::Button::new("导入…")
            .ghost()
            .show(ui, &d)
            .clicked()
        {
            state.import_memories_dialog(ui.ctx());
        }
        if neo_ui::Button::new("导出…")
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
        &format!("记忆保存在 {}", mem::memories_path().display()),
    );
}

#[cfg(test)]
mod ui_regression {
    use super::*;
    use crate::ui::composer::ui_regression::{context, frame, pointer, probe};

    #[test]
    fn ui_safety_narrow_settings_log_navigation_is_reachable() {
        for width in [320.0, 460.0] {
            let ctx = context();
            let loaded = neo_theme::fonts::install(&ctx);
            let mut state = AppState::default();
            let size = egui::vec2(width, 640.0);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                panel(ui, skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)), &mut state, size.y, &loaded);
            };
            for _ in 0..2 { frame(&ctx, size, vec![], &mut render); }
            let nav: Rect = probe(&ctx, "settings-nav-probe");
            frame(&ctx, size, pointer(nav.center(), true), &mut render);
            frame(&ctx, size, pointer(nav.center(), false), &mut render);
            frame(&ctx, size, vec![], &mut render);
            let (item, clip): (Rect, Rect) = probe(&ctx, "settings-log-item-probe");
            assert!(clip.contains_rect(item));
            assert!(Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(item));
            frame(&ctx, size, pointer(item.center(), true), &mut render);
            frame(&ctx, size, pointer(item.center(), false), &mut render);
            let output = frame(&ctx, size, vec![], &mut render);
            assert_eq!(state.settings_tab, SettingsTab::Logs);
            assert!(!output.shapes.is_empty());
        }
    }

    #[test]
    fn narrow_scaled_settings_keep_warnings_and_scrolled_safety_text_readable() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for dpi in [0.85, 1.0, 2.8] {
                    let ctx = context();
                    let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                        neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let whale = crate::brand::WhaleMark::cached(&ctx);
                    let skin = Skin::new(theme, &whale);
                    let size = egui::vec2(width, 720.0);
                    let mut state = AppState::default();
                    state.preferences_unsaved = true;
                    let mut seen = std::collections::BTreeSet::new();
                    let mut expected = 0;
                    let mut warning_seen = std::collections::BTreeSet::new();
                    let mut warning_rows = 0;
                    for step in 0..65 {
                        let mut input = egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                            time: Some(step as f64 / 10.0),
                            ..Default::default()
                        };
                        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point = Some(dpi);
                        if step > 1 {
                            input.events = vec![egui::Event::PointerMoved(egui::pos2(width * 0.5, 670.0)),
                                egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                                    delta: egui::vec2(0.0, -45.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }];
                        }
                        let mut output = ctx.run_ui(input, |ui| {
                            panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                                &mut state, size.y, &LoadedFonts::default());
                        });
                        output.textures_delta.clear();
                        for clipped in &output.shapes {
                            if let egui::Shape::Text(text) = &clipped.shape {
                                if text.galley.job.text.starts_with("设置尚未保存") {
                                    warning_rows = text.galley.rows.len();
                                    assert!(!text.galley.elided);
                                    for (index, row) in text.galley.rows.iter().enumerate() {
                                        let rect = row.rect().translate(text.pos.to_vec2());
                                        assert!(rect.left() >= 0.0 && rect.right() <= width);
                                        if clipped.clip_rect.expand(1.0).contains_rect(rect) { warning_seen.insert(index); }
                                    }
                                }
                                if text.galley.job.text.starts_with("这不是离线模式") {
                                    expected = text.galley.rows.len();
                                    for (index, row) in text.galley.rows.iter().enumerate() {
                                        let rect = row.rect().translate(text.pos.to_vec2());
                                        assert!(rect.left() >= 0.0 && rect.right() <= width, "safety overflow {width}/{scale}/{dpi}");
                                        if clipped.clip_rect.expand(1.0).contains_rect(rect) { seen.insert(index); }
                                    }
                                }
                            }
                        }
                    }
                    assert!(warning_rows > 0);
                    assert_eq!(warning_seen.len(), warning_rows, "all warning lines reachable {width}/{scale}/{dpi}");
                    assert!(expected > 0);
                    assert_eq!(seen.len(), expected, "all safety lines reachable {width}/{scale}/{dpi}");
                }
            }
        }
    }

    #[test]
    fn model_credentials_hint_wraps_without_painting_the_key() {
        for width in [240.0, 320.0, 460.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let mut state = AppState::default();
                state.api_key = "PRIVATE-KEY-NOT-FOR-DISPLAY".into();
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 6000.0))),
                    ..Default::default()
                }, |ui| {
                    ui.set_max_width(width - 16.0);
                    model_tab(ui, &skin, width - 16.0, &mut state);
                    assert!(ui.min_rect().width() <= width, "model overflow {width}/{scale}");
                });
                output.textures_delta.clear();
                let mut found = false;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        assert!(!text.galley.job.text.contains(&state.api_key));
                        if text.galley.job.text.starts_with("密钥仅在本机持久化") {
                            found = true;
                            assert!(!text.galley.elided);
                            assert!(clipped.clip_rect.expand(1.0).contains_rect(Rect::from_min_size(text.pos, text.galley.size())));
                        }
                    }
                }
                assert!(found);
                assert!(output.platform_output.commands.is_empty());
            }
        }
    }

    #[test]
    fn five_thinking_choices_wrap_without_overlap_and_all_click() {
        for width in [200.0, 320.0, 720.0, 1200.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                let ctx = context();
                neo_theme::fonts::install(&ctx);
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let labels: Vec<&str> = neo_llm::Thinking::ALL.iter().map(|t| t.label()).collect();
                let mut selected = 0;
                let mut draw = |events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1600.0))),
                        events, ..Default::default()
                    }, |ui| {
                        ui.set_max_width(width - 16.0);
                        if let Some(i) = choice_row(ui, &skin, width - 16.0, &labels, selected, "test-thinking") {
                            selected = i;
                        }
                    });
                    output.textures_delta.clear();
                    output
                };
                draw(vec![]);
                let output = draw(vec![]);
                let rects: Vec<Rect> = (0..labels.len()).map(|i| probe(&ctx, ("test-thinking", i))).collect();
                for (i, rect) in rects.iter().enumerate() {
                    assert!(rect.left() >= 0.0 && rect.right() <= width, "choice overflow {width}/{scale}");
                    for other in &rects[i + 1..] { assert!(!rect.shrink(0.1).intersects(other.shrink(0.1)), "targets overlap {width}/{scale}: {rect:?}, {other:?}"); }
                }
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        if labels.contains(&text.galley.text()) {
                            let i = labels.iter().position(|label| *label == text.galley.text()).unwrap();
                            assert!(!text.galley.elided);
                            assert!(rects[i].expand(1.0).contains_rect(Rect::from_min_size(text.pos, text.galley.size())),
                                "label crosses its target {width}/{scale}");
                        }
                    }
                }
                drop(draw);
                for (i, rect) in rects.iter().enumerate() {
                    for pressed in [true, false] {
                        let mut output = ctx.run_ui(egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1600.0))),
                            events: pointer(rect.center(), pressed), ..Default::default()
                        }, |ui| {
                            ui.set_max_width(width - 16.0);
                            if let Some(index) = choice_row(ui, &skin, width - 16.0, &labels, selected, "test-thinking") { selected = index; }
                        });
                        output.textures_delta.clear();
                    }
                    assert_eq!(selected, i, "choice not operable {width}/{scale}/{i}");
                }
            }
        }
    }

    #[test]
    fn long_values_and_section_titles_use_real_wrapped_height() {
        for width in [200.0, 320.0, 720.0] {
            for scale in [1.0, 1.75, 2.8] {
                let ctx = context();
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let values = ["provider/model-long-name-".repeat(8),
                    "D:\\School\\Neo\\very-long-database-path\\".repeat(6),
                    "VeryLongFontFamilyNameWithoutBreaks".repeat(8)];
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 12000.0))),
                    ..Default::default()
                }, |ui| {
                    ui.set_max_width(width - 16.0);
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    section_label_row(ui, &skin, width - 16.0, "上下文预算（估算 token）");
                    for (key, value) in ["当前模型", "数据库", "字体"].iter().zip(&values) {
                        kv_row(ui, &skin, width - 16.0, key, value);
                    }
                    assert!(ui.min_rect().width() <= width);
                });
                output.textures_delta.clear();
                let mut bottom = 0.0;
                let mut found = 0;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        assert!(!text.galley.elided);
                        assert!(clipped.clip_rect.expand(1.0).contains_rect(rect));
                        assert!(rect.top() >= bottom - 1.0, "rows overlap {width}/{scale}");
                        bottom = rect.bottom();
                        if values.contains(&text.galley.job.text) { found += 1; }
                    }
                }
                assert_eq!(found, 3);
            }
        }
    }

    #[test]
    fn long_page_title_reserves_clickable_close_target() {
        for width in [240.0, 320.0, 720.0] {
            for scale in [1.0, 1.75, 2.8] {
                let ctx = context();
                let loaded = neo_theme::fonts::install(&ctx);
                let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
                    neo_theme::Metrics::from_scale(scale));
                theme.apply(&ctx);
                let whale = crate::brand::WhaleMark::cached(&ctx);
                let skin = Skin::new(theme, &whale);
                let size = egui::vec2(width, 420.0);
                let mut state = AppState::default();
                state.settings_tab = SettingsTab::WakeTest;
                state.preferences_unsaved = true;
                let mut closed = false;
                let mut render = |events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                        events, ..Default::default()
                    }, |ui| {
                        closed = panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                            &mut state, size.y, &loaded);
                    });
                    output.textures_delta.clear();
                    output
                };
                render(vec![]);
                let output = render(vec![]);
                let (title, close): (Rect, Rect) = probe(&ctx, "settings-title-probe");
                assert!(title.contains_rect(close));
                let mut found = false;
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        if text.galley.text() == "麦克风测试" && rect.intersects(title) {
                            assert!(rect.right() < close.left());
                            assert!(title.expand(1.0).contains_rect(rect));
                            found = true;
                        }
                    }
                }
                assert!(found);
                render(pointer(close.center(), true));
                render(pointer(close.center(), false));
                drop(render);
                assert!(closed, "close not operable {width}/{scale}");
            }
        }
    }

    #[test]
    fn low_height_large_scale_keeps_body_scrollable_and_close_clear() {
        for (width, height, scale) in [(240.0, 360.0, 2.8), (320.0, 360.0, 2.8), (720.0, 300.0, 1.75), (1000.0, 320.0, 1.0)] {
            let ctx = context();
            let theme = neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
                neo_theme::Metrics::from_scale(scale));
            theme.apply(&ctx);
            let whale = crate::brand::WhaleMark::cached(&ctx);
            let skin = Skin::new(theme, &whale);
            let size = egui::vec2(width, height);
            let mut state = AppState::default();
            state.preferences_unsaved = true;
            state.settings_tab = SettingsTab::About;
            state.db_path = Some("D:\\School\\database-long-path\\".repeat(8));
            let mut seen = std::collections::BTreeSet::new();
            let mut warning_rows = 0;
            let mut database_seen = false;
            for step in 0..140 {
                let events = if step < 2 { vec![] } else {
                    vec![egui::Event::PointerMoved(egui::pos2(width * 0.6, height - 50.0)),
                        egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -20.0), modifiers: egui::Modifiers::NONE, phase: egui::TouchPhase::Move }]
                };
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                    time: Some(step as f64 * 0.1), events, ..Default::default()
                }, |ui| {
                    panel(ui, &skin, Rect::from_min_size(egui::pos2(8.0, 8.0), size - egui::vec2(16.0, 16.0)),
                        &mut state, height, &LoadedFonts::default());
                });
                output.textures_delta.clear();
                let body: Rect = probe(&ctx, "settings-body-probe");
                assert!(body.height() >= skin.t().caption * 1.5, "body starved {width}/{height}/{scale}: {body:?}");
                let (title, close): (Rect, Rect) = probe(&ctx, "settings-title-probe");
                assert!(Rect::from_min_size(egui::Pos2::ZERO, size).contains_rect(close));
                for clipped in &output.shapes {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        let rect = Rect::from_min_size(text.pos, text.galley.size());
                        if text.galley.text() == page_name(SettingsTab::About) && rect.intersects(title) {
                            assert!(rect.right() < close.left());
                        }
                        if text.galley.text().starts_with("设置尚未保存") {
                            warning_rows = text.galley.rows.len();
                            for (i, row) in text.galley.rows.iter().enumerate() {
                                if clipped.clip_rect.contains_rect(row.rect().translate(text.pos.to_vec2())) { seen.insert(i); }
                            }
                        }
                        if text.galley.text() == "数据库" && clipped.clip_rect.contains_rect(rect) { database_seen = true; }
                    }
                }
            }
            assert!(warning_rows > 0);
            assert_eq!(seen.len(), warning_rows);
            assert!(database_seen, "body unreachable {width}/{height}/{scale}");
        }
    }

    #[test]
    fn failed_memory_save_keeps_draft_and_paints_error() {
        for missing in [false, true] {
            let ctx = context();
            let mut state = AppState::default();
            state.memories = vec![neo_tools::tools::memory::Memory {
                id: 7, content: "original".into(), updated_ms: 0,
            }];
            state.memory_editing = Some((7, "  unsaved draft  ".into()));
            let mut calls = 0;
            let size = egui::vec2(640.0, 600.0);
            let mut render = |ui: &mut Ui, skin: &Skin<'_>| {
                memory_tab_with_edit(ui, skin, 580.0, &mut state, |id, draft| {
                    calls += 1;
                    assert_eq!(id, 7);
                    assert_eq!(draft, "unsaved draft");
                    if missing { Ok(None) } else { Err(neo_tools::ToolError::io("test write denied")) }
                });
            };
            for _ in 0..2 { frame(&ctx, size, vec![], &mut render); }
            let save: Rect = probe(&ctx, "neo-memory-save-probe");
            frame(&ctx, size, pointer(save.center(), true), &mut render);
            frame(&ctx, size, pointer(save.center(), false), &mut render);
            let output = frame(&ctx, size, vec![], &mut render);
            assert_eq!(calls, 1);
            assert_eq!(state.memory_editing, Some((7, "  unsaved draft  ".into())));
            assert_eq!(state.memories[0].content, "original");
            let message: String = probe(&ctx, "neo-memory-write-error");
            assert!(message.contains("草稿已保留"));
            assert!(message.contains(if missing { "已不存在" } else { "test write denied" }));
            assert!(output.shapes.iter().any(|clipped| {
                if let egui::Shape::Text(text) = &clipped.shape {
                    text.galley.job.text == message
                        && clipped.clip_rect.contains_rect(Rect::from_min_size(text.pos, text.galley.size()))
                } else { false }
            }), "error label must be painted inside the visible clip");
        }
    }
}

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
    let title = ui.painter().layout(title.to_owned(), d.font_bold(d.t().label), p.label_primary, tw);
    let desc = ui.painter().layout(desc.to_owned(), skin.prop(skin.t().caption), p.label_tertiary, width);
    let title_h = title.size().y.max(sw.y);
    let h = title_h + desc.size().y + m.s(18.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());

    // 右侧开关：组件只负责画与报点击，取值翻转在这里做。
    let sw_rect = Rect::from_center_size(
        egui::pos2(rect.right() - sw.x * 0.5, rect.top() + m.s(6.0) + title_h * 0.5),
        sw,
    );
    if Switch::new(*on)
        .id_salt(salt)
        .show_at(ui, &d, sw_rect)
        .clicked()
    {
        *on = !*on;
    }

    ui.painter().galley(rect.min + egui::vec2(0.0, m.s(6.0)), title, p.label_primary);
    ui.painter().galley(rect.min + egui::vec2(0.0, title_h + m.s(12.0)), desc, p.label_tertiary);
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
    let galley = ui.painter().layout(label.to_owned(), skin.d().font_bold(skin.t().caption),
        skin.p().label_secondary, width.max(1.0));
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, galley.size().y), Sense::hover());
    ui.painter().galley(rect.min, galley, skin.p().label_secondary);
    ui.add_space(skin.m().s(8.0));
}

/// 能放下才均分；否则改为可换行的独立选项，不缩小字体或隐藏选项。
fn choice_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, labels: &[&str], selected: usize, salt: &str) -> Option<usize> {
    let d = skin.d();
    let m = skin.m();
    let widest = labels.iter().flat_map(|label| [d.font(d.t().caption), d.font_bold(d.t().caption)]
        .map(|font| ui.painter().layout_no_wrap((*label).to_owned(), font, skin.p().label_primary).size().x))
        .fold(0.0_f32, f32::max);
    if width >= (widest + m.s(20.0)) * labels.len() as f32 + m.s(4.0) {
        let top = ui.cursor().top();
        let result = ui.push_id(salt, |ui| segmented(ui, skin, width, labels, selected)).inner;
        #[cfg(test)]
        for i in 0..labels.len() {
            let segment_w = (width - m.s(4.0)) / labels.len() as f32;
            let rect = Rect::from_min_size(egui::pos2(ui.min_rect().left() + m.s(2.0) + segment_w * i as f32, top + m.s(2.0)),
                Vec2::new(segment_w, m.s(28.0)));
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new((salt, i)), rect));
        }
        let _ = top;
        result
    } else {
        let mut result = None;
        ui.push_id(salt, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(m.s(6.0));
                for (i, label) in labels.iter().enumerate() {
                    let response = ui.add(egui::Button::new(egui::RichText::new(*label)
                        .font(d.font(d.t().caption))).selected(i == selected).wrap());
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new((salt, i)), response.rect));
                    if response.clicked() { result = Some(i); }
                }
            });
        });
        result
    }
}

/// 单行输入框 —— [`neo_ui::TextField`]：容器、聚焦环、密码掩码都由组件库负责。
///
/// `salt` 必须是常量（不能随内容变），否则每次按键 id 变化、焦点丢失。
fn input_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, value: &mut String, secret: bool, salt: &str) {
    let d = skin.d();
    let mut tf = TextField::new(value).id_salt(salt);
    if secret {
        tf = tf.secret(true);
    }
    tf.show(ui, &d, width);
}

/// 安全与操作提示必须完整换行，不能依赖悬停才能读全。
fn hint_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, text: &str) {
    ui.scope(|ui| {
        ui.set_max_width(width);
        ui.add(egui::Label::new(egui::RichText::new(text)
            .font(skin.prop(skin.t().caption)).color(skin.p().label_caption)).wrap());
    });
}

/// 短值保留紧凑对齐；长值改为标签在上、全文在下，触屏也能读全。
pub(super) fn kv_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, key: &str, value: &str) {
    let d = skin.d();
    let key_size = ui.painter().layout_no_wrap(key.to_owned(), d.font(d.t().caption), skin.p().label_caption).size();
    let value_size = ui.painter().layout_no_wrap(value.to_owned(), d.font_mono(d.t().caption), skin.p().label_secondary).size();
    if !value.contains(['\r', '\n']) && key_size.x <= width * 0.4
        && key_size.x + skin.m().s(16.0) + value_size.x <= width
        && key_size.y.max(value_size.y) <= FieldRow::height(&d)
    {
        FieldRow::new(key, value).show(ui, &d, width);
    } else {
        ui.scope(|ui| {
            ui.set_max_width(width);
            ui.add(egui::Label::new(egui::RichText::new(key)
                .font(d.font(d.t().caption)).color(skin.p().label_caption)).wrap());
            ui.add_space(skin.m().s(4.0));
            ui.add(egui::Label::new(egui::RichText::new(value)
                .font(d.font_mono(d.t().caption)).color(skin.p().label_secondary)).wrap());
            ui.add_space(skin.m().s(10.0));
        });
    }
}
