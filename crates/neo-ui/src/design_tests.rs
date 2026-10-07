//! Design 接线测试：token 入口的一致性与对比度 wiring。
//!
//! `Design` 是全组件库唯一的取色/取度量入口 —— 这里锁的是
//! 「快捷别名与正式访问器不漂移」「明暗判别与色板同源」这两件
//! 一旦跑偏就会让同帧控件用不同色板的事。

use super::*;

fn theme_of(mode: neo_theme::ThemeMode) -> Theme {
    Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0))
}

#[test]
fn aliases_never_drift_from_accessors() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let d = Design::new(theme_of(mode));
        assert_eq!(d.p(), d.palette(), "{mode:?}: p() 与 palette() 漂移");
        assert_eq!(
            d.m().scale(),
            d.metrics().scale(),
            "{mode:?}: m() 与 metrics() 漂移"
        );
        assert_eq!(d.t().body, d.typo().body, "{mode:?}: t() 与 typo() 漂移");
        assert_eq!(d.c(), d.comps(), "{mode:?}: c() 与 comps() 漂移");
    }
}

#[test]
fn is_dark_tracks_theme_mode() {
    assert!(!Design::new(theme_of(neo_theme::ThemeMode::Light)).is_dark());
    assert!(Design::new(theme_of(neo_theme::ThemeMode::Dark)).is_dark());
}

/// 组件 token 必须来自「当前色板」的明暗判别，而不是调用方的心情：
/// 同一个 `Design` 里 `comps()` 的取值集合由 `palette.bg_base` 的亮度唯一决定。
#[test]
fn comps_follow_the_palette_they_ship_with() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let d = Design::new(theme_of(mode));
        let sum = d.palette().bg_base.r() as u32
            + d.palette().bg_base.g() as u32
            + d.palette().bg_base.b() as u32;
        let want = if sum < { 3 * 128 } {
            neo_theme::palette::Components::DARK
        } else {
            neo_theme::palette::Components::LIGHT
        };
        assert_eq!(d.comps(), want, "{mode:?}: 组件 token 与色板明暗不一致");
    }
}

/// 字号入口：同一 `Design` 的三种字族在同磅值下尺寸一致
/// （族只改字面，不该改排版尺寸，否则混排时基线会跳）。
#[test]
fn font_families_share_the_same_size() {
    let d = Design::new(theme_of(neo_theme::ThemeMode::Light));
    let size = d.typo().body;
    assert_eq!(d.font(size).size, size);
    assert_eq!(d.font_bold(size).size, size);
    assert_eq!(d.font_mono(size).size, size);
}
