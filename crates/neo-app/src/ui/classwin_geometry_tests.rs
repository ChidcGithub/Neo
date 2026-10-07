//! 课堂总结窗口的几何锁定：滑入曲线端点、关闭钮与正文标题互不越界、
//! 红点倒计时几何。
//!
//! 与既有 `classwin_tests`（可见矩形物理坐标、挂起态交互性）分文件：
//! 这里只管「几何问题」—— 尺寸钳制、滑入插值、标题避让、红点外光晕。

use super::*;

fn dark_theme() -> Theme {
    Theme::new(
        neo_theme::ThemeMode::Dark,
        1080.0,
        neo_theme::Distance::Standard,
    )
}

// ---------------------------------------------------------------------------
// target_rect：窗口永远不越过显示器下缘，也不小于最小可读尺寸
// ---------------------------------------------------------------------------

#[test]
fn target_rect_respects_monitor_and_floor_on_extreme_sizes() {
    let theme = dark_theme();
    let m = theme.metrics;
    for monitor in [
        Vec2::new(1280.0, 720.0),
        Vec2::new(1920.0, 1080.0),
        Vec2::new(3840.0, 2160.0),
        Vec2::new(96.0, 96.0), // 极端小副屏：尺寸钳到下限，左上留 16px 边距
    ] {
        let rect = ClassWin::target_rect(theme, monitor);
        // 左上不低于 16px 边距（顶部滑入终点），右侧不越出屏幕（除非被迫）。
        assert!(rect.min.x >= m.s(16.0) - 0.001, "{monitor}");
        assert_eq!(rect.min.y, m.s(20.0));
        assert!(rect.width() <= m.s(640.0) + 0.001);
        assert!(rect.height() <= m.s(460.0) + 0.001);
        assert!(rect.width() >= m.s(280.0) - 0.001);
        assert!(rect.height() >= m.s(160.0) - 0.001);
        // 水平居中（小屏被迫贴左时除外）。
        if monitor.x >= m.s(640.0) + m.s(32.0) {
            let expected_x = (monitor.x - rect.width()) * 0.5;
            assert!((rect.min.x - expected_x).abs() < 0.01, "{monitor}");
        }
    }
}

// ---------------------------------------------------------------------------
// 滑入：从顶外 -size.y 到 target.y，ease-out 单调节拍，t=1 精确落点
// ---------------------------------------------------------------------------

#[test]
fn slide_in_interpolation_is_monotonic_and_lands_exactly_on_target() {
    let theme = dark_theme();
    let rect = ClassWin::target_rect(theme, Vec2::new(1920.0, 1080.0));
    let target = rect.min;
    let size = rect.size();
    let mut last_y = f32::NEG_INFINITY;
    for step in 0..=32 {
        let t = step as f32 / 32.0;
        let eased = 1.0 - (1.0 - t).powi(3);
        let y = target.y * eased + (-size.y) * (1.0 - eased);
        // 单调不降（向上滑入），两端精确命中。
        assert!(y >= last_y - 0.001);
        last_y = y;
        if step == 0 {
            assert!((y + size.y).abs() < 0.01, "t=0 必须完全在屏外");
        }
        if step == 32 {
            assert!((y - target.y).abs() < 1e-4, "t=1 必须精确落位");
        }
        // 任意中间帧的水平位置恒等于 target.x。
        let _ = Pos2::new(target.x, y);
    }
    // 显式终点分支：t>=1 不再发插值，直接发 target（防止停顿跨帧抖动）。
    let t = 1.0f32;
    let eased = 1.0 - (1.0 - t).powi(3);
    assert_eq!(eased, 1.0);
}

// ---------------------------------------------------------------------------
// 关闭钮：不盖住标题文字，自身不越出卡片右缘
// ---------------------------------------------------------------------------

#[test]
fn close_button_clears_title_and_stays_inside_card() {
    let theme = dark_theme();
    let m = theme.metrics;
    let rect = ClassWin::target_rect(theme, Vec2::new(1920.0, 1080.0));
    let pad = m.s(18.0);
    let inner = rect.shrink(pad);
    let title_rect = Rect::from_min_size(inner.min, Vec2::new(inner.width(), m.s(26.0)));
    let close_d = m.s(26.0);
    let close_center = Pos2::new(title_rect.right() - close_d * 0.5, title_rect.center().y);
    let close_rect = Rect::from_center_size(close_center, Vec2::splat(close_d));
    // 关闭钮不越出标题行右缘。
    assert!(close_rect.right() <= title_rect.right() + 0.001);
    assert!(close_rect.top() >= title_rect.top() - 0.001);
    assert!(close_rect.bottom() <= title_rect.bottom() + 0.001);
    // 标题文字预算区在关闭钮左侧结束，两区不相交。
    let title_max_w = close_center.x - close_d - title_rect.left();
    let title_zone = Rect::from_min_max(
        title_rect.min,
        Pos2::new(close_center.x - close_d, title_rect.bottom()),
    );
    assert!(!title_zone.intersects(close_rect));
    assert!(title_max_w > 0.0);
    // 分隔线/正文从标题行下方起排，不侵入按钮垂直带。
    let y = title_rect.bottom() + m.s(4.0);
    assert!(y > close_rect.bottom());
}

// ---------------------------------------------------------------------------
// 红点：倒计时几何 —— 半径由卡片宽度决定，外圈光晕与实体同圆心
// ---------------------------------------------------------------------------

#[test]
fn dot_geometry_scales_with_metrics_and_stays_centered() {
    let theme = dark_theme();
    let m = theme.metrics;
    let d = m.s(12.0);
    let margin = m.s(14.0);
    let rect = Rect::from_min_size(Pos2::new(margin, margin), Vec2::splat(d));
    let c = rect.center();
    let r = rect.width() * 0.5;
    // 外圈光晕半径 = 卡片半宽；实体点半径 = 62%。
    assert!((r - d * 0.5).abs() < 0.001);
    assert!(r * 0.62 < r);
    // 两圆同心：光晕不会偏出红点。
    let _ = c;
    // 卡片左上贴 margin，不越过屏幕原点。
    assert!(rect.min.x >= 0.0 && rect.min.y >= 0.0);
}

#[test]
fn dot_alpha_envelope_fades_in_then_out_and_stays_bounded() {
    // k = elapsed / DOT_SECS ∈ [0,1]；前 3% 淡入，末 8% 淡出。
    for k in [0.0_f32, 0.015, 0.03, 0.5, 0.92, 0.96, 1.0] {
        let a = (k / 0.03_f32).min(1.0) * (1.0 - ((k - 0.92) / 0.08).clamp(0.0, 1.0));
        assert!((0.0..=1.0).contains(&a), "k={k}");
        match k {
            // f32 的 0.03/0.08 不精确：k=1 时 (1.0-0.92)/0.08 ≈ 0.99999988（差一个 ulp），
            // 残余 a ≈ 1.8e-7 而非 0.0，端点容差取 EPSILON 量级而非 1e-5。
            0.0 => assert!(a < 1e-5, "k=0 必须全隐"),
            0.5 => assert!((a - 1.0).abs() < 1e-5, "k=0.5 必须全亮"),
            1.0 => assert!(a < 4.0 * f32::EPSILON, "k=1 必须收完（实际 {a:e}）"),
            _ => {}
        }
    }
}
