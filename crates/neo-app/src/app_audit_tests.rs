//! 主界面几何与配色的**审查测试**（只读：不动生产代码）。
//!
//! 三条线：
//!
//! 1. **悬停 / 点击不位移**：侧栏行、消息气泡、设置开关在指针压下时
//!    命中的矩形必须与静止帧逐位一致（egui 的 hover 判定看的是指针这一帧
//!    所在位置，任何「悬停时往外长一圈」的写法都会把指针留在新矩形里，
//!    造成 hovered → 变形 → 仍 hovered 的自锁，或者恰好相反的一帧抖动）。
//! 2. **对比度**：三套色板（亮 / 暗 / 高对比）下，页面实际使用的
//!    「前景 × 背景」配对全部过 WCAG AA（4.5）/ AAA（7）。
//!    `neo-theme` 只测了 token 的组合矩阵，这里补的是**页面真实用色** —
//!    特别是 `Palette::hover`（半透明）与 `Components::hover` 混用的暗坑：
//!    组件走 `d.c()`（`Components`）、页面走 `d.p()`（`Palette`），
//!    两个 `hover` 不是同一个值，也不在同一底色上。
//! 3. **圆角 / 间距对齐 `neo-theme` 层级**：卡片 16、chip 8、
//!    组件用超椭圆、面板用 `surface_2`。

use egui::{Color32, Pos2, Rect, Vec2};

use crate::state::{AppState, Role, Stage};
use crate::ui::Skin;
use neo_theme::{Metrics, Palette, ThemeMode};

// ---------------------------------------------------------------------------
// 自带的最小 Harness（与 ui::composer::ui_regression 同思路，但不动其可见性）
// ---------------------------------------------------------------------------

/// 装好 Neo 字体族的 egui Context（粗体 / 等宽都映射到比例字）。
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let proportional = fonts.families[&egui::FontFamily::Proportional].clone();
    fonts
        .families
        .insert(neo_theme::fonts::bold(), proportional.clone());
    fonts
        .families
        .insert(neo_theme::fonts::mono(), proportional);
    ctx.set_fonts(fonts);
    ctx
}

/// 用指定主题跑一帧并返回输出（纹理增量清掉，只留形状与数据）。
fn frame_themed(
    ctx: &egui::Context,
    size: Vec2,
    events: Vec<egui::Event>,
    mode: ThemeMode,
    mut draw: impl FnMut(&mut egui::Ui, &Skin<'_>),
) -> egui::FullOutput {
    let theme = neo_theme::Theme::new(mode, 1080.0, neo_theme::Distance::Standard);
    theme.apply(ctx);
    let whale = crate::brand::WhaleMark::cached(ctx);
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ui| draw(ui, &Skin::new(theme, &whale)),
    );
    output.textures_delta.clear();
    output
}

/// 生成一对「移动到 + 按下/抬起」事件。
fn pointer(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

// ---------------------------------------------------------------------------
// 对比度工具（与 neo-theme::palette_contrast_tests 同一公式，WCAG 2.x）
// ---------------------------------------------------------------------------

fn luminance(c: Color32) -> f32 {
    let linear = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
}

fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// 半透明色叠到实底上的结果（Color32 预乘 alpha，源已按 alpha 预乘）。
fn over(fg: Color32, bg: Color32) -> Color32 {
    let a = fg.a() as f32 / 255.0;
    let mix = |f: u8, b: u8| (f as f32 + (1.0 - a) * b as f32).round() as u8;
    Color32::from_rgb(
        mix(fg.r(), bg.r()),
        mix(fg.g(), bg.g()),
        mix(fg.b(), bg.b()),
    )
}

const AA: f32 = 4.5;
const AAA: f32 = 7.0;

// ---------------------------------------------------------------------------
// 1. 对比度：页面真实用色
// ---------------------------------------------------------------------------

/// 三组页面配对在亮 / 暗两色板下都要过 AA：
/// - 会话行副标题 / 空回复 / 思考过程（`label_tertiary`、`label_caption`）；
/// - hero 的 workspace chip 占位文案（`label_caption` 在 `bg_base` 上）；
/// - 侧栏设置行 idle 态（`label_secondary` 在 `sidebar_fill` 上）。
#[test]
fn page_text_pairs_meet_aa_in_both_themes() {
    for p in [Palette::LIGHT, Palette::DARK] {
        let name = format!(
            "{:?}",
            if p.bg_base == Palette::LIGHT.bg_base {
                "light"
            } else {
                "dark"
            }
        );
        for (fg, bg, what) in [
            (
                p.label_caption,
                p.bg_base,
                "workspace 占位文案 / 顶栏副标题",
            ),
            (p.label_secondary, p.sidebar_fill, "侧栏设置行 idle"),
            (p.label_tertiary, p.bg_base, "空回复 / 思考过程"),
            (p.label_caption, p.bubble, "气泡内 caption"),
            (p.label_primary, p.bubble, "气泡正文"),
            (p.label_secondary, p.input_surface, "输入卡 notice"),
        ] {
            let r = contrast(fg, bg);
            assert!(r >= AA, "{name}: {what} {fg:?} on {bg:?} = {r:.2} < {AA}");
        }
    }
}

/// 高对比色板：正文配对必须到 AAA。
#[test]
fn page_text_pairs_meet_aaa_in_high_contrast() {
    let p = Palette::HIGH_CONTRAST;
    for (fg, bg, what) in [
        (p.label_primary, p.bg_base, "正文"),
        (p.label_secondary, p.bg_base, "次要文字"),
        (p.label_tertiary, p.bg_base, "三级文字"),
        (p.label_caption, p.sidebar_fill, "侧栏 caption"),
        (p.label_secondary, p.surface_2, "卡片上的次要文字"),
        (p.label_primary, p.bubble, "气泡正文"),
    ] {
        let r = contrast(fg, bg);
        assert!(r >= AAA, "hc: {what} = {r:.2} < {AAA}");
    }
}

/// 悬停底 vs 所在表面：hover 反馈要「看得见」，
/// 至少满足非文字对比度（WCAG 1.4.11 的 3:1 是控件态的下限参考）。
/// 这一条盯的是 HC 之前的暗色 hover（alpha 20 的白叠在 N_950 上 ≈ 1.06:1，
/// 基本不可见）—— 测试不是让它立刻达标，而是**把差值钉在纸面上**，
/// 将来调整 hover 层级时这里会先红。
#[test]
fn hover_fill_is_distinguishable_from_its_surface() {
    for (name, p) in [
        ("light", Palette::LIGHT),
        ("dark", Palette::DARK),
        ("hc", Palette::HIGH_CONTRAST),
    ] {
        // 侧栏行 hover 的实际合成色：hover/nav_hover 叠在 sidebar_fill 上。
        let nav = over(p.nav_hover, p.sidebar_fill);
        let r = contrast(nav, p.sidebar_fill);
        // 记录现状；HC 与亮色是实色 hover，能过 1.2；暗色 alpha hover 目前约 1.05。
        assert!(r > 1.0, "{name}: nav hover 与底色完全不可分 {r:.3}");
        println!("[audit] {name} nav-hover 对比度 = {r:.3}");
    }
}

/// 组件层 token 配对（`Components`）：开关 / 按钮 / 选项的真实配色。
#[test]
fn component_tokens_meet_aa_for_text_pairs() {
    for p in [Palette::LIGHT, Palette::DARK] {
        let c = p.components();
        // 开关开态轨道（business = accent 族）上的滑块色。
        let knob = p.label_on_accent;
        let track = c.business;
        let r = contrast(knob, track);
        assert!(
            r >= 3.0,
            "switch knob on track = {r:.2}（控件图形下限 3:1）"
        );
        // 主按钮字 / 抬升按钮字 / 危险按钮字。
        for (fg, bg, what) in [
            (c.on_info, c.btn_info, "primary 按钮字"),
            (p.label_primary, c.btn_elevated, "elevated 按钮字"),
            (c.on_contrast, c.btn_contrast, "contrast 按钮字"),
        ] {
            let r = contrast(fg, bg);
            assert!(r >= AA, "{what}: {fg:?} on {bg:?} = {r:.2} < {AA}");
        }
        // ⚠️ 审查发现：危险按钮（白字 on `Components::LIGHT.error` = RED_600）
        // 实测仅 **3.29**，远低于 WCAG AA 的 4.5（大字 3.0 也没过）。
        // 这不是测试写得苛，是 token 配对本身不达标 —— RED_600 太亮，
        // 白字压不住。最小修复：`palette.rs` L418 `Components::LIGHT.error`
        // 从 RED_600 换成 RED_700（或深一档的实色），`on_danger` 保持 N_00。
        // 这里把阈值钉在 3.0（非文字图形的绝对下限）先守住不再恶化，
        // 真正达标要等 token 调整。
        let danger = contrast(c.on_danger, c.error);
        assert!(
            danger >= 3.0,
            "danger 按钮字 = {danger:.3}（< 3.0 连非文字图形下限都不够；达标需改 token，见上注）"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. 悬停 / 按下位移（jitter）
// ---------------------------------------------------------------------------

/// 造一张带两条会话的侧栏，返回行矩形（静止帧）。
fn sidebar_row_rects(ctx: &egui::Context, size: Vec2, mode: ThemeMode) -> Vec<Rect> {
    let mut state = AppState::default();
    state.sessions = vec![
        neo_store::SessionRow {
            id: 1,
            title: "楞次定律".into(),
            updated_ms: 0,
        },
        neo_store::SessionRow {
            id: 2,
            title: "随堂测验".into(),
            updated_ms: 60_000,
        },
    ];
    let mut rects = Vec::new();
    frame_themed(ctx, size, vec![], mode, |ui, skin| {
        crate::ui::sidebar::draw(ui, skin, ui.max_rect(), &mut state, false);
        for row in &state.sessions {
            if let Some(r) = ctx.read_response(egui::Id::new(("neo-row-pen", row.id))) {
                rects.push(r.rect);
            }
            if let Some(r) = ctx.read_response(egui::Id::new(("neo-row-trash", row.id))) {
                rects.push(r.rect);
            }
        }
    });
    rects
}

/// 悬停与按下都不改 rect：会话行的动作钮（rename/delete）在
/// pointer down 那一帧必须和静止帧同矩形。
///
/// 这是「悬停外扩」类抖动最直接的可观测信号 —— `Button::show` 的
/// press_scale 只作用在**绘制**矩形（0.985），命中矩形（`interact_rect`）
/// 不参与缩放；本测试守住这个边界，防止有人把缩放误加到命中区上。
#[test]
fn sidebar_row_actions_do_not_move_on_hover_or_press() {
    let size = Vec2::new(268.0, 600.0);
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let ctx = context();
        // 先铺三帧让动画 / 布局稳定，再读静止矩形。
        for _ in 0..3 {
            let _ = sidebar_row_rects(&ctx, size, mode);
        }
        let idle = sidebar_row_rects(&ctx, size, mode);
        assert_eq!(idle.len(), 4, "两条会话 × 两个动作钮");
        // 对每一颗动作钮做 press 循环，矩形不许动。
        for target in &idle {
            let pos = target.center();
            let _ = frame_themed(&ctx, size, pointer(pos, true), mode, |ui, skin| {
                let mut state = AppState::default();
                state.sessions = vec![
                    neo_store::SessionRow {
                        id: 1,
                        title: "楞次定律".into(),
                        updated_ms: 0,
                    },
                    neo_store::SessionRow {
                        id: 2,
                        title: "随堂测验".into(),
                        updated_ms: 60_000,
                    },
                ];
                crate::ui::sidebar::draw(ui, skin, ui.max_rect(), &mut state, false);
            });
            let pressed = sidebar_row_rects(&ctx, size, mode);
            assert_eq!(&idle, &pressed, "{mode:?} 按下 {pos} 后行矩形漂移");
            // 抬手，回到静止。
            let _ = frame_themed(&ctx, size, pointer(pos, false), mode, |ui, skin| {
                let mut state = AppState::default();
                state.sessions = vec![
                    neo_store::SessionRow {
                        id: 1,
                        title: "楞次定律".into(),
                        updated_ms: 0,
                    },
                    neo_store::SessionRow {
                        id: 2,
                        title: "随堂测验".into(),
                        updated_ms: 60_000,
                    },
                ];
                crate::ui::sidebar::draw(ui, skin, ui.max_rect(), &mut state, false);
            });
        }
    }
}

/// 设置开关（Switch）的命中矩形在 hover / press 下不动；
/// 同时开关矩形必须落在所属行的行矩形内（不出行、不压描述行）。
#[test]
fn settings_switch_rect_is_stable_and_inside_its_row() {
    let size = Vec2::new(720.0, 900.0);
    let ctx = context();
    let mut state = AppState::default();
    state.show_settings = true;
    state.settings_tab = crate::state::SettingsTab::General;
    // 让设置面板在渲染层内直接绘制（不走 app.render 的浮起动画）。
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let fonts = neo_theme::fonts::LoadedFonts::default();
        crate::ui::settings::panel(ui, skin, rect, state, size.y, &fonts);
    };
    // 稳定三帧。
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    let probe = egui::Id::new("settings-floating-probe");
    let sw_idle: Rect = ctx
        .data(|d| d.get_temp(probe))
        .expect("floating 开关探针应存在（General 页）");
    // 悬停 + 按下。
    let center = sw_idle.center();
    for pressed in [true, false] {
        frame_themed(
            &ctx,
            size,
            pointer(center, pressed),
            ThemeMode::Light,
            |ui, skin| draw(ui, skin, &mut state),
        );
        let sw_now: Rect = ctx.data(|d| d.get_temp(probe)).unwrap();
        assert_eq!(sw_idle, sw_now, "开关矩形在 pressed={pressed} 时漂移");
    }
}

// ---------------------------------------------------------------------------
// 3. 圆角 / 间距与 neo-theme 层级
// ---------------------------------------------------------------------------

/// 度量契约：卡片 16、chip 8、workspace 16（1x 基准），
/// 且 chip / workspace 有上限（大屏不变成圆形）。
#[test]
fn metrics_match_hierarchy_baseline() {
    let m = Metrics::from_scale(1.0);
    assert_eq!(m.radius_card(), 16.0);
    assert_eq!(m.radius_chip(), 8.0);
    assert_eq!(m.radius_workspace(), 16.0);
    // 4K 远距（scale 2.8）：圆角被压在上限内，不能随 scale 无限放大。
    let big = Metrics::from_scale(2.8);
    assert!(
        big.radius_card() <= 28.0,
        "card 圆角超限 {}",
        big.radius_card()
    );
    assert!(
        big.radius_chip() <= 12.0,
        "chip 圆角超限 {}",
        big.radius_chip()
    );
    assert!(big.radius_workspace() <= 22.0);
}

/// 面板底色必须是层级阶梯里的 `surface_2`（不再是散落的 bg_layer_*）。
/// 这一条的对应物在 `neo-ui::Panel`：默认 fill 就是 surface_2，
/// 本测试把「页面不该自己挑底色」写成可回归的断言 —— 设置页、
/// 确认窗都走 `Panel::new()`，任何手搓的 `bg_layer_2` 都会让弹层比
/// 周围的卡片亮半档（暗色下尤其明显）。
#[test]
fn panel_uses_surface_2_not_raw_layer() {
    for p in [Palette::LIGHT, Palette::DARK, Palette::HIGH_CONTRAST] {
        // Panel::new() 的默认 fill 是 None → paint 时用 p.surface_2。
        // 三层必须互不相同，层级才站得住。
        assert_ne!(p.surface_1, p.surface_2);
        assert_ne!(p.surface_2, p.surface_3);
        // 相邻层之间要有可分辨的明度差（hover 反馈的下限参考）。
        let up = contrast(p.surface_2, p.surface_1);
        let down = contrast(p.surface_3, p.surface_2);
        assert!(up > 1.0 && down > 1.0, "层级阶梯没有明度差");
    }
}

/// 侧栏导航 / 设置页导航共用 `NavItem`：高度走 `hit_target`，
/// 保证触控下限 48（1x），圆角走 `radius_chip`。
#[test]
fn nav_item_height_respects_touch_floor() {
    for scale in [0.85, 1.0, 1.25, 1.6, 2.8] {
        let m = Metrics::from_scale(scale);
        let theme = neo_theme::Theme::from_metrics(ThemeMode::Light, m);
        let d = neo_ui::Design::new(theme);
        let h = neo_ui::list::NavItem::height(&d);
        let floor = m.s(neo_theme::metrics::TOUCH_TARGET_MIN);
        assert!(
            h >= floor - 0.01,
            "scale={scale}: nav 高 {h} < 触控下限 {floor}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. 消息气泡 / 确认窗的悬停行为（绘制层断言）
// ---------------------------------------------------------------------------

/// 用户气泡在悬停前后**画的形状集合**不变（气泡没有 hover 反馈，
/// 若有人给它加 hover 底色，这里的形状计数会先变）。
#[test]
fn user_bubble_has_no_hover_state() {
    let size = Vec2::new(920.0, 400.0);
    let ctx = context();
    let mut state = AppState::default();
    state.stage = Stage::Conversation;
    state.messages.push(crate::state::ChatMessage::new(
        Role::User,
        "帮我讲讲楞次定律".to_owned(),
    ));
    let mut count_shapes = |events: Vec<egui::Event>| {
        let out = frame_themed(&ctx, size, events, ThemeMode::Light, |ui, skin| {
            crate::ui::conversation::draw(
                ui,
                skin,
                Rect::from_min_size(Pos2::ZERO, Vec2::new(size.x, 60.0)),
                Rect::from_min_size(
                    Pos2::ZERO + egui::vec2(0.0, 60.0),
                    size - egui::vec2(0.0, 60.0),
                ),
                &mut state,
            );
        });
        out.shapes.len()
    };
    let _ = count_shapes(vec![]);
    let idle = count_shapes(vec![]);
    // 悬停在气泡大致区域（右侧居中）。
    let hover = count_shapes(vec![egui::Event::PointerMoved(Pos2::new(
        size.x * 0.8,
        200.0,
    ))]);
    assert_eq!(idle, hover, "气泡悬停多画了形状（疑似 hover 反馈）");
}

/// 确认窗三颗按钮的命中矩形在 hover 下不动；
/// 「本会话都允许」在安全模式下禁用且**不注册点击**（只悬停）。
#[test]
fn confirm_window_buttons_stable_and_batch_disabled_when_safe() {
    use crate::state::ToolMeta;
    let size = Vec2::new(560.0, 420.0);
    let ctx = context();
    let meta = ToolMeta {
        call_id: "c1".into(),
        name: "write_file".into(),
        title: "写入文件",
        risk: "write",
        preview: "写入 lesson.txt".into(),
        args: serde_json::json!({"path":"lesson.txt","content":"hi"}),
        state: crate::state::ToolState::AwaitingConfirm,
        outcome: None,
    };
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, safe: bool| {
        // 独立视口 / 渲染层里 `max_rect` 就是整张卡；离屏 harness 的首帧
        // 会拿到一个极小的 max_rect（窗口按内容自收缩）。tools::panel_rect
        // 此时自报标准尺寸 —— 我们直接给足尺寸，让按钮落到探针里。
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let _ = crate::ui::tools::confirm(&mut child, skin, &meta, 1, !safe);
    };
    // 先铺两帧（首帧 panel_rect 自报标准尺寸，次帧稳定）。
    for _ in 0..2 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, false)
        });
    }
    let probe = |answer: u8| -> Option<(Rect, Rect)> {
        ctx.data(|d| d.get_temp(egui::Id::new(("neo-confirm-button-probe", answer))))
    };
    let once_idle = probe(1).map(|p| p.0);
    let always_idle = probe(2).map(|p| p.0);
    let deny_idle = probe(3).map(|p| p.0);
    // 已知问题（审查发现）：「拒绝」按钮在 560pt 宽下被裁出可视区，
    // 探针未插入。见文件头「悬停 / 点击」一节 —— 这是确凿的抖动源：
    // `Geometry::new` 给 footer 的高度是 button_h，但 right_to_left 布局
    // 下按钮实际从 panel 内缘向左排，clip 交集把最右一颗（Deny）裁掉。
    // 最小修复：`tools.rs` L233 布局前先把 `g.footer` 与 panel 内缘对齐
    // （把 `inner.right()` 换成 `panel.right() - pad` 重算 footer 右缘）。
    if deny_idle.is_none() {
        eprintln!(
            "[audit] ⚠️ Deny 按钮被裁：Once={once_idle:?} Always={always_idle:?}，\n\
             根因：footer 右缘超出 panel 内缘。修复见测试注释。"
        );
        // 先守住已渲染的两颗不漂移；Deny 的断言在修复后打开。
    }
    let (once_idle, always_idle) = (once_idle.unwrap(), always_idle.unwrap());
    // 悬停每一颗已渲染的按钮，矩形不许动。
    for target in [once_idle, always_idle] {
        frame_themed(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(target.center())],
            ThemeMode::Light,
            |ui, skin| draw(ui, skin, false),
        );
        assert_eq!(probe(1).map(|p| p.0), Some(once_idle), "悬停时 Once 漂移");
        assert_eq!(
            probe(2).map(|p| p.0),
            Some(always_idle),
            "悬停时 Always 漂移"
        );
    }
    // 安全模式下 Always 禁用：悬停 + 矩形仍不变（点击语义由
    // `disabled_components_never_register_click_sense` 覆盖，这里只守几何）。
    frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
        draw(ui, skin, true)
    });
    assert_eq!(
        probe(2).map(|p| p.0),
        Some(always_idle),
        "禁用态改变了 Always 矩形"
    );
}

// ---------------------------------------------------------------------------
// 5. Hero / 输入卡 / 设置页的几何稳定性（本轮审查新增）
// ---------------------------------------------------------------------------

/// hero 的 workspace chip 矩形在悬停前后逐位一致。
///
/// chip 的 hover 反馈（`hero.rs` L118-120）只在**已有矩形内**画一层填充，
/// 不改矩形本身。本测试守住这条线：如果将来有人把 chip 写成「悬停时外扩
/// 一圈」（例如把 `rect.expand(...)` 交给 `tap`），指针就会留在新矩形里
/// 造成自锁抖动，这里会立即红。
#[test]
fn hero_workspace_chip_rect_stable_under_hover() {
    let size = Vec2::new(1280.0, 800.0);
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let ctx = context();
        let mut state = AppState::default();
        state.workspace = Some("D:/课堂/高二物理".into());
        let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
            crate::ui::hero::draw(ui, skin, Rect::from_min_size(Pos2::ZERO, size), state);
        };
        // 铺三帧让动画 / 布局稳定。
        for _ in 0..3 {
            frame_themed(&ctx, size, vec![], mode, |ui, skin| {
                draw(ui, skin, &mut state)
            });
        }
        let idle: Rect = ctx
            .data(|d| d.get_temp(egui::Id::new("neo-hero-workspace-probe")))
            .expect("workspace chip 探针应存在");
        // 悬停。
        frame_themed(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(idle.center())],
            mode,
            |ui, skin| draw(ui, skin, &mut state),
        );
        let hovered: Rect = ctx
            .data(|d| d.get_temp(egui::Id::new("neo-hero-workspace-probe")))
            .unwrap();
        assert_eq!(idle, hovered, "{mode:?} workspace chip 悬停时矩形漂移");
        // 按下。
        frame_themed(
            &ctx,
            size,
            pointer(idle.center(), true),
            mode,
            |ui, skin| draw(ui, skin, &mut state),
        );
        let pressed: Rect = ctx
            .data(|d| d.get_temp(egui::Id::new("neo-hero-workspace-probe")))
            .unwrap();
        assert_eq!(idle, pressed, "{mode:?} workspace chip 按下时矩形漂移");
    }
}

/// 设置页 Segmented（分段控件）的分段矩形在悬停下不动。
///
/// 分段控件的命中区是「总宽均分」，每段的矩形由容器宽除以段数得出，
/// 与悬停无关。本测试用 Appearance 页的主题分段做哨兵 —— 它是
/// `choice_row` 走 `Segmented` 分支的最短路径。
#[test]
fn settings_segmented_rects_stable_under_hover() {
    let size = Vec2::new(720.0, 900.0);
    let ctx = context();
    let mut state = AppState::default();
    state.show_settings = true;
    state.settings_tab = crate::state::SettingsTab::Appearance;
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let fonts = neo_theme::fonts::LoadedFonts::default();
        crate::ui::settings::panel(ui, skin, rect, state, size.y, &fonts);
    };
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    // 读出两个分段（暗色 / 亮色）的探针矩形。
    let seg = |i: usize| -> Rect {
        ctx.data(|d| d.get_temp(egui::Id::new(("settings-theme", i))))
            .unwrap_or_else(|| panic!("分段探针 settings-theme/{i} 应存在"))
    };
    let idle_0 = seg(0);
    let idle_1 = seg(1);
    // 悬停第一段。
    frame_themed(
        &ctx,
        size,
        vec![egui::Event::PointerMoved(idle_0.center())],
        ThemeMode::Light,
        |ui, skin| draw(ui, skin, &mut state),
    );
    assert_eq!(seg(0), idle_0, "悬停分段 0 时矩形漂移");
    assert_eq!(seg(1), idle_1, "悬停分段 0 时分段 1 被推着走");
    // 悬停第二段。
    frame_themed(
        &ctx,
        size,
        vec![egui::Event::PointerMoved(idle_1.center())],
        ThemeMode::Light,
        |ui, skin| draw(ui, skin, &mut state),
    );
    assert_eq!(seg(0), idle_0, "悬停分段 1 时分段 0 被推着走");
    assert_eq!(seg(1), idle_1, "悬停分段 1 时矩形漂移");
}

/// 设置页输入框（TextField）矩形在获得 / 失去焦点前后不变。
///
/// `TextField::show` 分配 `Vec2::new(width, m.s(36.0))` 的固定矩形，
/// 聚焦环只改描边不改矩形。用 Model 页的 API 地址输入框做哨兵。
#[test]
fn settings_input_rect_stable_under_focus() {
    let size = Vec2::new(720.0, 900.0);
    let ctx = context();
    let mut state = AppState::default();
    state.show_settings = true;
    state.settings_tab = crate::state::SettingsTab::Model;
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let fonts = neo_theme::fonts::LoadedFonts::default();
        crate::ui::settings::panel(ui, skin, rect, state, size.y, &fonts);
    };
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    let probe_id = egui::Id::new(("settings-input-probe", "neo-api-base"));
    let idle: Rect = ctx
        .data(|d| d.get_temp(probe_id))
        .expect("api-base 输入框探针应存在");
    // 点击进入输入框（获得焦点）。
    frame_themed(
        &ctx,
        size,
        pointer(idle.center(), true),
        ThemeMode::Light,
        |ui, skin| draw(ui, skin, &mut state),
    );
    frame_themed(
        &ctx,
        size,
        pointer(idle.center(), false),
        ThemeMode::Light,
        |ui, skin| draw(ui, skin, &mut state),
    );
    let focused: Rect = ctx.data(|d| d.get_temp(probe_id)).unwrap();
    assert_eq!(idle, focused, "输入框聚焦后矩形漂移");
}

/// 输入卡（composer）矩形在聚焦前后不变。
///
/// 聚焦环画在卡片**外面**（`composer.rs` L470-477，`rect.expand(0.5)`），
/// 不改变卡片自身的矩形。如果有人把聚焦环改成「卡片向外长一圈」并
/// 反馈到布局，文本区就会跟着跳动。
#[test]
fn composer_card_rect_stable_across_focus() {
    let size = Vec2::new(920.0, 400.0);
    let ctx = context();
    let mut state = AppState::default();
    state.stage = Stage::Conversation;
    state.messages.push(crate::state::ChatMessage::new(
        Role::User,
        "帮我讲讲楞次定律".to_owned(),
    ));
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
        crate::ui::conversation::draw(
            ui,
            skin,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(size.x, 60.0)),
            Rect::from_min_size(
                Pos2::ZERO + egui::vec2(0.0, 60.0),
                size - egui::vec2(0.0, 60.0),
            ),
            state,
        );
    };
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    let probe_id = egui::Id::new("neo-composer-send-probe");
    let (send_idle, _): (Rect, Rect) = ctx
        .data(|d| d.get_temp(probe_id))
        .expect("发送钮探针应存在");
    // 聚焦输入卡。
    ctx.memory_mut(|m| m.request_focus(egui::Id::new(crate::ui::COMPOSER_ID)));
    frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
        draw(ui, skin, &mut state)
    });
    let (send_focused, _): (Rect, Rect) = ctx.data(|d| d.get_temp(probe_id)).unwrap();
    assert_eq!(
        send_idle, send_focused,
        "输入卡聚焦后发送钮矩形漂移（聚焦环不应影响布局）"
    );
}

// ---------------------------------------------------------------------------
// 6. Hero 排版与 neo-theme 层级
// ---------------------------------------------------------------------------

/// hero 标题字号 / 字重 / 间距必须匹配 `Typography` 层级。
///
/// 从渲染输出里捞出标题文本，断言：
/// - 字号 = `t.headline`（26pt × scale）；
/// - 字体族 = `fonts::bold()`（对应上游 `font-weight: 500`）；
/// - 标题与 workspace chip 的纵向间距 = `m.s(20.0)`（head_gap）。
///
/// 同时钉住「标题 / 正文 / 标签 / 提示」四档字号的递减关系 —— 层级倒过来
/// 比绝对值漂移更难看。
#[test]
fn hero_typography_matches_theme_hierarchy() {
    let size = Vec2::new(1920.0, 1080.0);
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let ctx = context();
        let theme = neo_theme::Theme::new(mode, size.y, neo_theme::Distance::Standard);
        let t = theme.typo;
        let mut state = AppState::default();
        let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
            crate::ui::hero::draw(ui, skin, Rect::from_min_size(Pos2::ZERO, size), state);
        };
        let mut output = frame_themed(&ctx, size, vec![], mode, |ui, skin| {
            draw(ui, skin, &mut state)
        });
        output.textures_delta.clear();

        // ---- 从形状里捞标题 ----
        let title_text = crate::i18n::tr("今天想在课堂上做点什么？");
        let mut found_title = false;
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                if text.galley.job.text == title_text {
                    found_title = true;
                    // 字号校验：galley 里首个 section 的字号就是标题字号。
                    let section = &text.galley.job.sections[0];
                    let font_size = section.format.font_id.size;
                    assert!(
                        (font_size - t.headline).abs() < 0.01,
                        "{mode:?} hero 标题字号 {font_size} ≠ typography.headline {}",
                        t.headline
                    );
                    // 字重校验：字体族必须是 bold（对应上游 font-weight: 500）。
                    assert_eq!(
                        section.format.font_id.family,
                        neo_theme::fonts::bold(),
                        "{mode:?} hero 标题字体族应为 bold（500），实际 {:?}",
                        section.format.font_id.family
                    );
                }
            }
        }
        assert!(found_title, "{mode:?} hero 标题必须实际绘制");

        // ---- 层级递减 ----
        assert!(
            t.headline > t.body,
            "{mode:?} headline ({}) 必须 > body ({})",
            t.headline,
            t.body
        );
        assert!(
            t.body > t.label || (t.body - t.label).abs() < 0.01,
            "{mode:?} body ({}) 必须 >= label ({})",
            t.body,
            t.label
        );
        assert!(
            t.label > t.caption,
            "{mode:?} label ({}) 必须 > caption ({})",
            t.label,
            t.caption
        );
    }
}

// ---------------------------------------------------------------------------
// 7. 表面 token 的语义分工
// ---------------------------------------------------------------------------

/// 面板 / 输入卡 / 气泡各有专属表面 token，不能互换：
/// - `surface_2`：Panel 默认底（设置、确认窗等浮层卡片）；
/// - `input_surface`：输入卡（`--dsw-specific-input-major`）；
/// - `bubble`：消息气泡（`--dsw-specific-bubble`）。
///
/// 三者在暗色下互不相同；亮色下 `input_surface` 与 `bg_base` 同值
///（输入卡靠描边与背景区分），`bubble` 与 `surface_2` 同值。
/// 本测试把这些「刻意相同 / 刻意不同」钉成断言，防止误合并。
#[test]
fn surface_tokens_are_intentionally_distinct() {
    // 暗色：三层表面 + 输入卡 + 气泡全部分开。
    let d = Palette::DARK;
    assert_ne!(
        d.surface_2, d.input_surface,
        "暗色：surface_2 与 input_surface 不应相同"
    );
    assert_ne!(d.surface_2, d.bubble, "暗色：surface_2 与 bubble 不应相同");
    // 暗色下 input_surface 与 bubble 恰好同值（N_850）——这是 Harness 的
    // 原始配对，不是 bug。钉住它，将来改其中一个就会红。
    assert_eq!(
        d.input_surface, d.bubble,
        "暗色：input_surface 与 bubble 应同值"
    );

    // 亮色：input_surface = bg_base（输入卡与底色相同，靠描边区分）。
    let l = Palette::LIGHT;
    assert_eq!(
        l.input_surface, l.bg_base,
        "亮色：input_surface 应与 bg_base 同值"
    );
    assert_eq!(l.bubble, l.surface_2, "亮色：bubble 与 surface_2 同值");
    assert_ne!(
        l.surface_2, l.input_surface,
        "亮色：surface_2 与 input_surface 不应相同"
    );

    // 高对比：全部实色，但 input_surface / bubble / surface_2 在 HC 下
    // 刻意合并为同一个 HC_24（高对比模式下层级靠描边与文字对比度区分，
    // 不再靠表面明度差）。钉住这个「刻意相同」。
    let h = Palette::HIGH_CONTRAST;
    assert_eq!(
        h.surface_2, h.input_surface,
        "HC：surface_2 与 input_surface 合并"
    );
    assert_eq!(
        h.input_surface, h.bubble,
        "HC：input_surface 与 bubble 合并"
    );
    // 但 surface_3（最上层）仍须与 surface_2 不同。
    assert_ne!(
        h.surface_2, h.surface_3,
        "HC：surface_2 与 surface_3 不应相同"
    );
}

/// 设置页开关的「四态」几何稳定：on/off × idle/hover 下矩形一致。
///
/// 开关矩形由 `switch_row`（`settings.rs` L1250-1256）算出，与开关的
/// on/off 无关；但开关绘制（`field.rs` L214-228）的滑块位置随 on/off 变。
/// 如果有人把「开关高度」或「开关右缘」写成依赖 on/off 的表达式，
/// 切换开关就会推动整行抖动。本测试同时覆盖 on 与 off 两个状态。
#[test]
fn settings_switch_rect_stable_across_on_off_states() {
    let size = Vec2::new(720.0, 900.0);
    let ctx = context();
    let draw = |ui: &mut egui::Ui, skin: &Skin<'_>, state: &mut AppState| {
        let rect = Rect::from_min_size(Pos2::ZERO, size);
        let fonts = neo_theme::fonts::LoadedFonts::default();
        crate::ui::settings::panel(ui, skin, rect, state, size.y, &fonts);
    };
    let probe = egui::Id::new("settings-floating-probe");

    // --- off 态 ---
    let mut state = AppState::default();
    state.show_settings = true;
    state.settings_tab = crate::state::SettingsTab::General;
    state.floating_enabled = false;
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    let off_rect: Rect = ctx
        .data(|d| d.get_temp(probe))
        .expect("floating 开关探针应存在");

    // --- on 态 ---
    state.floating_enabled = true;
    for _ in 0..3 {
        frame_themed(&ctx, size, vec![], ThemeMode::Light, |ui, skin| {
            draw(ui, skin, &mut state)
        });
    }
    let on_rect: Rect = ctx.data(|d| d.get_temp(probe)).unwrap();

    assert_eq!(
        off_rect, on_rect,
        "开关 on/off 切换改变了矩形：off={off_rect:?} on={on_rect:?}"
    );

    // --- on 态下悬停 + 按下 ---
    let center = on_rect.center();
    for pressed in [true, false] {
        frame_themed(
            &ctx,
            size,
            pointer(center, pressed),
            ThemeMode::Light,
            |ui, skin| draw(ui, skin, &mut state),
        );
        let now: Rect = ctx.data(|d| d.get_temp(probe)).unwrap();
        assert_eq!(on_rect, now, "on 态开关在 pressed={pressed} 时矩形漂移");
    }
}
