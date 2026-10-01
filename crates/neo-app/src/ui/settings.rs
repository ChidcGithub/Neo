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
        let nav_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(32.0)));
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
        let close_d = m.hit_target(m.s(28.0)).min(cw * 0.5);
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
        let close_rect = Rect::from_min_max(
            egui::pos2(title_rect.right() - close_d, title_rect.top()), title_rect.max);
        let close = ui.scope(|ui| {
            ui.shrink_clip_rect(close_rect);
            IconButton::new(Icon::Close).ghost().label("关闭设置")
                .id_salt("neo-settings-close").show_at(ui, &d, close_center)
        }).inner;
        if close.clicked() {
            closed = true;
        }
        #[cfg(test)]
        ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("settings-title-probe"),
            (title_rect, close.rect)));
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
    switch_row(ui, skin, width, "致命启动错误静默退出", "仅课堂安全模式开启时生效，默认关闭",
        &mut state.silent_startup_errors, "neo-set-silent-startup");
    hint_row(ui, skin, width, "致命启动错误静默退出，仍记录本地日志；普通工具/网络故障不自动退出。关闭此项时致命故障显示错误弹窗。");
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

fn about_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState, loaded: &LoadedFonts) {
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

    section_label_row(ui, skin, width, "软件更新");
    switch_row(ui, skin, width, "自动检查更新", "启动后联网 GitHub 检查一次发布版本；不自动下载或安装",
        &mut state.auto_check_updates, "neo-set-auto-updates");
    let checking = matches!(state.update_status, crate::updates::Status::Checking);
    if ui.add_enabled(!checking, egui::Button::new("立即检查")).clicked() {
        state.update_check_requested = true;
    }
    ui.add_space(m.s(8.0));
    match &state.update_status {
        crate::updates::Status::Idle => hint_row(ui, skin, width, "尚未检查更新；手动检查间隔至少 10 秒"),
        crate::updates::Status::Checking => hint_row(ui, skin, width, "正在连接 GitHub 检查更新…"),
        crate::updates::Status::UpToDate => hint_row(ui, skin, width, "当前已是最新发布版本"),
        crate::updates::Status::Available { version, url } => {
            hint_row(ui, skin, width, &format!("发现新版本 {version}，请自行查看发布说明并下载"));
            ui.hyperlink_to("查看 GitHub 发布页面", url);
        }
        crate::updates::Status::Failed(_) => hint_row(ui, skin, width, "检查失败，请确认网络后稍后重试；不影响继续使用"),
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
    edit: impl FnMut(u64, &str) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
) {
    memory_tab_with_actions(ui, skin, width, state, edit,
        |id| neo_tools::tools::memory::forget_memory(&format!("#{id}")));
}

fn memory_tab_with_actions(
    ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState,
    mut edit: impl FnMut(u64, &str) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
    mut delete: impl FnMut(u64) -> Result<Option<neo_tools::tools::memory::Memory>, neo_tools::ToolError>,
) {
    use neo_tools::tools::memory as mem;

    let d = skin.d();
    let p = skin.p();
    let m = skin.m();

    let error_id = egui::Id::new("neo-memory-write-error");
    let mut error = ui.ctx().data(|data| data.get_temp::<String>(error_id));
    let owner_id = error_id.with("row");
    let mut error_owner = ui.ctx().data(|data| data.get_temp::<u64>(owner_id));
    let confirm_id = egui::Id::new("neo-memory-delete-confirm");
    let mut confirming = ui.ctx().data(|data| data.get_temp::<(u64, bool)>(confirm_id));
    let scroll_id = error_id.with("scroll");
    let scroll_error = ui.ctx().data_mut(|data| data.remove_temp::<bool>(scroll_id)).unwrap_or(false);

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
                    .tone(neo_ui::NoticeTone::Error).show(ui, &d);
                if scroll_error { notice.scroll_to_me(Some(egui::Align::Min)); }
                #[cfg(test)]
                ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-memory-error-probe"), (notice.rect, ui.clip_rect())));
                ui.add_space(m.s(6.0));
            }
        }
        if let Some((id, opening)) = confirming.filter(|(id, _)| *id == item.id) {
            let question = format!("删除记忆 #{}？此操作无法撤销。\n{}", id, item.content);
            let result = neo_ui::list::ConfirmBar::new(&question)
                .show(ui, &d, ("memory-confirm", id), opening);
            #[cfg(test)]
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-memory-confirm-probe"), (result.cancel.rect, result.confirm.rect)));
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
                    .label("保存记忆")
                    .id_salt(("neo-mem-save", item.id))
                    .show_touch(ui, &d);
                #[cfg(test)]
                ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("neo-memory-save-probe"), save.rect));
                if save.clicked() {
                    act = Some(Act::Save);
                }
                if IconButton::new(Icon::Close)
                    .ghost()
                    .label("取消编辑记忆")
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

        ui.add(egui::Label::new(egui::RichText::new(format!("#{} · {}", item.id, item.content))
            .font(d.font(d.t().label)).color(p.label_primary)).wrap());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(m.s(6.0));
            let pen = IconButton::new(Icon::Pen).ghost().label("编辑记忆")
                .id_salt(("neo-mem-pen", item.id)).show_touch(ui, &d);
            let trash = IconButton::new(Icon::Trash).danger().label("删除记忆")
                .id_salt(("neo-mem-trash", item.id)).show_touch(ui, &d);
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new(("neo-memory-actions-probe", item.id)), (pen.rect, trash.rect));
            });
            if pen.clicked() { act = Some(Act::Edit(item.id, item.content.clone())); }
            if trash.clicked() { act = Some(Act::RequestDelete(item.id)); }
        });
        ui.add_space(m.s(8.0));
    }

    let acted = act.is_some();
    match act {
        Some(Act::Edit(id, content)) => {
            if let Some((editing_id, _)) = state.memory_editing.as_ref() {
                error_owner = Some(*editing_id);
                error = Some("请先保存或取消当前编辑，草稿已保留".to_owned());
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
                Ok(None) => error = Some("删除失败：该记忆已不存在".to_owned()),
                Err(e) => error = Some(format!("删除失败：{}", e.message)),
            }
        },
        Some(Act::Save) => {
            if let Some((id, draft)) = &state.memory_editing {
                error_owner = Some(*id);
                let trimmed = draft.trim();
                if trimmed.is_empty() {
                    error = Some("记忆内容不能为空；如需删除请取消编辑后使用删除按钮".to_owned());
                } else {
                    match edit(*id, trimmed) {
                        Ok(Some(updated)) => {
                            if let Some(item) = state.memories.iter_mut().find(|item| item.id == updated.id) { *item = updated; }
                            state.memory_editing = None;
                            error = None;
                            state.memories_file_ms = None;
                        }
                        Ok(None) => error = Some("保存失败：该记忆已不存在，草稿已保留".to_owned()),
                        Err(e) => error = Some(format!("保存失败：{}（草稿已保留）", e.message)),
                    }
                }
            }
        }
        Some(Act::Cancel) => { state.memory_editing = None; error = None; }
        None => {}
    }
    ui.ctx().data_mut(|data| {
        if let Some(value) = confirming { data.insert_temp(confirm_id, value); }
        else { data.remove::<(u64, bool)>(confirm_id); }
        if acted && error.is_some() && error_owner.is_some() {
            data.insert_temp(scroll_id, true);
        }
    });

    // 添加与备份。
    ui.add_space(m.s(10.0));
    section_label_row(ui, skin, width, "添加与备份");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(m.s(8.0), m.s(8.0));
        let input_w = width;
        input_row(ui, skin, input_w, &mut state.memory_draft, false, "neo-mem-new");
        if neo_ui::Button::new("记下")
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
                    Err(e) => { error_owner = None; error = Some(format!("添加失败：{}（草稿已保留）", e.message)); },
                }
            }
        }
    });
    if error_owner.is_none() {
        if let Some(message) = &error {
            neo_ui::InlineNotice::new("memory-add-error", message).tone(neo_ui::NoticeTone::Error).show(ui, &d);
        }
    }
    ui.ctx().data_mut(|data| {
        if let Some(owner) = error_owner.filter(|_| error.is_some()) { data.insert_temp(owner_id, owner); }
        else { data.remove::<u64>(owner_id); }
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
            .touch_layout()
            .ghost()
            .show(ui, &d)
            .clicked()
        {
            state.import_memories_dialog(ui.ctx());
        }
        if neo_ui::Button::new("导出…")
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
        &format!("记忆保存在 {}", mem::memories_path().display()),
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
