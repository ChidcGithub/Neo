//! 基础原语测试：几何稳定性（inset / 阴影 / 过渡）与状态语义。
//!
//! 这里的断言守一件事：**状态（hover / press / 主题）只改颜色，不改几何**。
//! 几何一变，布局就会抖 —— 教室大屏上两米外都看得见。

use super::*;

fn design_of(mode: neo_theme::ThemeMode) -> Design {
    Design::new(neo_theme::Theme::from_metrics(
        mode,
        neo_theme::Metrics::from_scale(1.0),
    ))
}

// ---------------------------------------------------------------------------
// inset / inset_all：精确的矩形代数
// ---------------------------------------------------------------------------

#[test]
fn inset_shrinks_each_side_exactly() {
    let rect = Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 220.0));
    let got = inset(rect, 1.0, 2.0, 3.0, 4.0);
    assert_eq!(
        got,
        Rect::from_min_max(egui::pos2(11.0, 22.0), egui::pos2(107.0, 216.0))
    );
}

#[test]
fn inset_all_equals_uniform_inset() {
    let rect = Rect::from_min_max(egui::pos2(-5.0, 7.5), egui::pos2(40.0, 90.0));
    assert_eq!(inset_all(rect, 6.0), inset(rect, 6.0, 6.0, 6.0, 6.0));
}

// ---------------------------------------------------------------------------
// translucent：压透明度只缩放通道，不改变色相
// ---------------------------------------------------------------------------

#[test]
fn translucent_clamps_and_scales() {
    let c = Color32::from_rgb(100, 150, 200);
    assert_eq!(translucent(c, 1.0), c);
    assert_eq!(translucent(c, 0.0).a(), 0);
    // 越界输入按 0..1 钳制。
    assert_eq!(translucent(c, -1.0).a(), 0);
    assert_eq!(translucent(c, 2.0), c);
    // 半透明：RGB 与 alpha 等比（预乘语义）。
    let half = translucent(c, 0.5);
    assert_eq!(half.a(), 128);
    assert_eq!(half.r(), 50);
}

// ---------------------------------------------------------------------------
// State::emphasis：只产出不透明度增益，与几何无关
// ---------------------------------------------------------------------------

#[test]
fn emphasis_depends_only_on_flags() {
    let mk = |hovered, pressed| State {
        hovered,
        pressed,
        focused: false,
        enabled: true,
    };
    assert_eq!(mk(false, false).emphasis(), 0.0);
    assert_eq!(mk(true, false).emphasis(), 0.6);
    assert_eq!(mk(false, true).emphasis(), 1.0);
    assert_eq!(mk(true, true).emphasis(), 1.0);
}

// ---------------------------------------------------------------------------
// elevation_soft：主题切换只改颜色，不改几何
// ---------------------------------------------------------------------------

#[test]
fn shadow_geometry_is_theme_invariant() {
    let light = elevation_soft(&design_of(neo_theme::ThemeMode::Light));
    let dark = elevation_soft(&design_of(neo_theme::ThemeMode::Dark));
    assert_eq!(light.offset, dark.offset, "主题切换不得改变阴影位移");
    assert_eq!(light.blur, dark.blur, "主题切换不得改变阴影模糊半径");
    assert_eq!(light.spread, dark.spread, "主题切换不得改变阴影扩散");
    // 颜色必须不同（暗色下投影更深才有存在感），且都不透明通道非零。
    assert_ne!(light.color, dark.color, "明暗投影应各自取色");
    assert!(light.color.a() > 0 && dark.color.a() > 0);
    // 亮色投影必须比暗色淡（白底灰边显脏的约束来自上游）。
    assert!(
        light.color.a() < dark.color.a(),
        "亮色投影应更淡: light={} dark={}",
        light.color.a(),
        dark.color.a()
    );
}

#[test]
fn shadow_matches_upstream_spec() {
    // `--dsw-elevation-soft`：位移 6px / 模糊 20px / 无扩散。
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let s = elevation_soft(&design_of(mode));
        assert_eq!(s.offset, [0, 6], "{mode:?}");
        assert_eq!(s.blur, 20, "{mode:?}");
        assert_eq!(s.spread, 0, "{mode:?}");
    }
}

// ---------------------------------------------------------------------------
// elide：省略结果恒在预算内（含测量容差边界）
// ---------------------------------------------------------------------------

#[test]
fn elide_respects_budget_and_epsilon_boundary() {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let font = FontId::proportional(14.0);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.painter();
        let long = "一段足够长的中文标题文本用来触发省略号截断逻辑";
        let full_w = painter
            .layout_no_wrap(long.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x;

        // 预算足够：原样返回（恰好等宽 + 容差内也算够）。
        assert_eq!(elide(painter, long, &font, full_w + MEASURE_EPSILON), long);
        // 预算为负 / 为零：空串（调用方不该画出半个省略号）。
        assert_eq!(elide(painter, long, &font, 0.0), "");
        assert_eq!(elide(painter, long, &font, -3.0), "");

        // 预算收紧：结果必须真的放得下。
        let budget = full_w * 0.4;
        let shown = elide(painter, long, &font, budget);
        assert!(shown.ends_with('…'), "截断后应以省略号结尾: {shown:?}");
        let shown_w = painter
            .layout_no_wrap(shown.clone(), font.clone(), Color32::WHITE)
            .size()
            .x;
        assert!(
            shown_w <= budget + MEASURE_EPSILON,
            "截断结果 {shown_w} 超出预算 {budget}"
        );
        assert!(shown.len() < long.len(), "应真的截短: {shown:?}");
    });
    output.textures_delta.clear();
}
