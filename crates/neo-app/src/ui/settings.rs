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
use neo_theme::SquirclePaint;
use neo_ui::{field, FieldRow, Icon, IconButton, NavItem, Panel, Switch, TextField};

use super::{at, section_label, segmented, text_left, Skin};
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
    let p = skin.p();
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

    // 距离 chip
    let label = format!("观看距离 · {}", state.distance.label());
    let font = d.font_bold(d.t().caption);
    let tw = ui
        .painter()
        .layout_no_wrap(label.clone(), font.clone(), p.label_secondary)
        .size()
        .x;
    let chip_w = tw + m.s(22.0);
    let chip = Rect::from_min_size(
        egui::pos2(
            btn_center.x - btn_d * 0.5 - m.s(8.0) - chip_w,
            rect.center().y - h * 0.5,
        ),
        Vec2::new(chip_w, h),
    );
    let dist_resp = super::tap(ui, chip, ui.id().with("neo-distance-chip"));
    let st = super::State::of(&dist_resp);
    let fill = if st.hovered {
        p.hover_solid
    } else {
        p.bg_layer_2
    };
    ui.painter().squircle(
        chip,
        m.radius_chip(),
        fill,
        egui::Stroke::new(1.0, p.border_l1),
    );
    super::text_center(ui.painter(), chip, &label, font, p.label_secondary);
    out.distance = dist_resp.clicked();

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
    let inner = Panel::new().paint(ui, &d, rect, m.s(24.0));

    // 左导航 / 右内容，中间一道竖分隔线。
    let nav_w = m.s(176.0);
    let gap = m.s(24.0);
    let nav_rect = Rect::from_min_size(inner.min, Vec2::new(nav_w, inner.height()));
    let content_rect =
        Rect::from_min_max(egui::pos2(nav_rect.right() + gap, inner.top()), inner.max);
    ui.painter().vline(
        nav_rect.right() + gap * 0.5,
        inner.y_range(),
        egui::Stroke::new(1.0, p.border_l1),
    );

    // ---- 左：标题 + 导航 ----
    at(ui, nav_rect, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let (title_rect, _) =
            ui.allocate_exact_size(Vec2::new(nav_w, m.s(32.0)), Sense::hover());
        text_left(
            ui.painter(),
            title_rect,
            "设置",
            d.font_bold(d.t().label + m.s(6.0)),
            p.label_primary,
        );
        ui.add_space(m.s(16.0));
        for &(tab, name) in SettingsTab::ALL {
            let resp = NavItem::new(name, nav_icon(tab))
                .active(state.settings_tab == tab)
                .id_salt(("settings-nav", name))
                .show(ui, &d, nav_w);
            if resp.clicked() {
                state.settings_tab = tab;
            }
            ui.add_space(m.s(4.0));
        }
    });

    // ---- 右：页标题 + 描述 + 滚动内容 ----
    at(ui, content_rect, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let cw = content_rect.width();

        // 页标题行（右端是关闭钮）。
        let (title_rect, _) = ui.allocate_exact_size(Vec2::new(cw, m.s(28.0)), Sense::hover());
        text_left(
            ui.painter(),
            title_rect,
            page_name(state.settings_tab),
            d.font_bold(d.t().label + m.s(4.0)),
            p.label_primary,
        );
        let close_d = m.s(24.0);
        let close_center = egui::pos2(title_rect.right() - close_d * 0.5, title_rect.center().y);
        if IconButton::new(Icon::Close)
            .ghost()
            .id_salt("neo-settings-close")
            .show_at(ui, &d, close_center)
            .clicked()
        {
            closed = true;
        }
        ui.add_space(m.s(2.0));
        let (desc_rect, _) = ui.allocate_exact_size(Vec2::new(cw, m.s(16.0)), Sense::hover());
        text_left(
            ui.painter(),
            desc_rect,
            page_desc(state.settings_tab),
            skin.prop(skin.t().caption),
            p.label_tertiary,
        );
        ui.add_space(m.s(14.0));

        // 内容区滚动：行多也不顶破面板（小窗里调用方会把面板收窄）。
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                // 给滚动条让位。
                let w = (ui.available_width() - m.s(12.0)).max(0.0);
                match state.settings_tab {
                    SettingsTab::General => general_tab(ui, skin, w, state),
                    SettingsTab::Appearance => appearance_tab(ui, skin, w, state),
                    SettingsTab::Display => display_tab(ui, skin, w, state, viewport_h),
                    SettingsTab::Model => model_tab(ui, skin, w, state),
                    SettingsTab::About => about_tab(ui, skin, w, state, loaded),
                }
            });
    });

    closed
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
        SettingsTab::Appearance => "主题与回复展示",
        SettingsTab::Display => "观看距离与缩放链路",
        SettingsTab::Model => "接口、密钥与模型列表",
        SettingsTab::About => "版本、存储与字体装配",
    }
}

fn nav_icon(tab: SettingsTab) -> Icon {
    match tab {
        SettingsTab::General => Icon::Cog,
        SettingsTab::Appearance => Icon::Sun,
        SettingsTab::Display => Icon::Board,
        SettingsTab::Model => Icon::Sparkle,
        SettingsTab::About => Icon::Info,
    }
}

// ---------------------------------------------------------------------------
// 各页
// ---------------------------------------------------------------------------

/// 通用页：后台运行、启动即后台与语音唤醒三个开关。
fn general_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    switch_row(
        ui,
        skin,
        width,
        "关闭时最小化到托盘",
        "点关闭按钮后转入系统托盘继续运行，「Hi, Neo」唤醒会重新打开窗口",
        &mut state.minimize_to_tray,
        "neo-set-tray",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        "启动时最小化到托盘",
        "启动后不显示主界面，常驻后台等待「Hi, Neo」语音唤醒",
        &mut state.start_in_tray,
        "neo-set-start-tray",
    );
    row_divider(ui, skin, width);
    switch_row(
        ui,
        skin,
        width,
        "语音唤醒「Hi, Neo」",
        "常驻监听唤醒词，唤醒后全屏跑马灯亮起，直接说出指令即可",
        &mut state.wake_enabled,
        "neo-set-wake",
    );
}

fn appearance_tab(ui: &mut Ui, skin: &Skin<'_>, width: f32, state: &mut AppState) {
    let m = skin.m();
    section_label_row(ui, skin, width, "主题");
    let idx = match state.theme_mode {
        neo_theme::ThemeMode::Dark => 0,
        neo_theme::ThemeMode::Light => 1,
    };
    if let Some(sel) = segmented(ui, skin, width, &["暗色", "亮色"], idx) {
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
    if let Some(sel) = segmented(ui, skin, width, &["近距", "教室", "远距"], idx) {
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
    hint_row(ui, skin, width, "仅保存在本机数据库，不会上传");
    ui.add_space(m.s(16.0));

    section_label_row(ui, skin, width, "当前模型");
    let name = if state.has_models() {
        format!("{}（{}）", state.model_display(), state.model_id())
    } else {
        "未选择模型".to_owned()
    };
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, m.s(30.0)), Sense::hover());
    ui.painter().squircle(
        rect,
        m.radius_chip(),
        skin.p().bg_layer_2,
        egui::Stroke::new(1.0, skin.p().border_l1),
    );
    text_left(
        ui.painter(),
        neo_ui::inset_all(rect, 8.0),
        &name,
        skin.prop(skin.t().label),
        skin.p().label_secondary,
    );
    ui.add_space(m.s(6.0));
    let switch_hint = if state.model_def().is_some_and(|m| m.reasoning) {
        "在输入卡的模型选择器里切换（点一下循环）· 该模型默认开启思考"
    } else {
        "在输入卡的模型选择器里切换（点一下循环）"
    };
    hint_row(ui, skin, width, switch_hint);
    ui.add_space(m.s(16.0));

    // ---- 思考强度：对应请求体顶层的 thinking / reasoning_effort ----
    section_label_row(ui, skin, width, "思考强度");
    let labels: Vec<&str> = neo_llm::Thinking::ALL.iter().map(|t| t.label()).collect();
    let sel = neo_llm::Thinking::ALL
        .iter()
        .position(|t| *t == state.thinking)
        .unwrap_or(0);
    if let Some(i) = segmented(ui, skin, width, &labels, sel) {
        state.thinking = neo_llm::Thinking::ALL[i];
    }
    ui.add_space(m.s(6.0));
    hint_row(ui, skin, width, state.thinking.hint());
    ui.add_space(m.s(16.0));

    // ---- 可选模型：来自模型商的 /models，内置表只是兜底 ----
    section_label_row(ui, skin, width, "可选模型");
    // 左：数量；右：刷新键。交给布局系统排，别手量宽度（Button 会自己算内边距）。
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(if state.has_models() {
                format!("{} 个可用 · 已保存到本机", state.models.len())
            } else {
                "还没有模型".to_owned()
            })
            .font(skin.prop(skin.t().label))
            .color(skin.p().label_secondary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if neo_ui::Button::new("从模型商刷新")
                .elevated()
                .show(ui, &skin.d())
                .clicked()
            {
                state.start_model_fetch();
            }
        });
    });
    ui.add_space(m.s(6.0));
    let hint = match (&state.model_fetch, &state.model_fetch_error) {
        (Some(_), _) => "正在拉取…".to_owned(),
        (None, Some(e)) if !state.has_models() => {
            format!("拉取失败：{e} · 检查接口地址与密钥后重试")
        }
        (None, Some(e)) => format!("上次刷新失败：{e}（仍在用上次拉到的列表）"),
        (None, None) if state.has_models() => {
            "列表来自模型商，已保存到本机；每次启动会自动刷新".to_owned()
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
    let h = m.s(52.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());

    // 右侧开关：组件只负责画与报点击，取值翻转在这里做。
    let sw = Switch::size(&d);
    let sw_rect = Rect::from_center_size(
        egui::pos2(rect.right() - sw.x * 0.5, rect.center().y),
        sw,
    );
    if Switch::new(*on)
        .id_salt(salt)
        .show_at(ui, &d, sw_rect)
        .clicked()
    {
        *on = !*on;
    }

    // 左侧文字（给开关让位）。
    let tw = (sw_rect.left() - m.s(12.0) - rect.left()).max(0.0);
    let title_rect = Rect::from_min_size(
        rect.min + egui::vec2(0.0, m.s(7.0)),
        Vec2::new(tw, m.s(20.0)),
    );
    text_left(
        ui.painter(),
        title_rect,
        title,
        d.font_bold(d.t().label),
        p.label_primary,
    );
    let desc_rect = Rect::from_min_size(
        rect.min + egui::vec2(0.0, m.s(29.0)),
        Vec2::new(tw, m.s(16.0)),
    );
    text_left(
        ui.painter(),
        desc_rect,
        desc,
        skin.prop(skin.t().caption),
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

/// 小标题行。
fn section_label_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, label: &str) {
    let m = skin.m();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, m.s(18.0)), Sense::hover());
    section_label(ui.painter(), skin, rect, label);
    ui.add_space(m.s(6.0));
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

/// 灰色提示行 —— 转发组件库。
fn hint_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, text: &str) {
    field::hint_row(ui, &skin.d(), width, text);
}

/// 一行键值对（左键、右值）—— [`neo_ui::FieldRow`]。
fn kv_row(ui: &mut Ui, skin: &Skin<'_>, width: f32, key: &str, value: &str) {
    FieldRow::new(key, value).show(ui, &skin.d(), width);
}
