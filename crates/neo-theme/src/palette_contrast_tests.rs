use super::*;
use crate::{Metrics, ThemeMode};

/// 三套实色：亮色 / 暗色 / 高对比，连同各自的文字对比度底线（WCAG AA / AAA）。
const SETS: [(&str, Palette, f32); 3] = [
    ("light", Palette::LIGHT, 4.5),
    ("dark", Palette::DARK, 4.5),
    ("hc", Palette::HIGH_CONTRAST, 7.0),
];

fn luminance(color: Color32) -> f32 {
    let linear = |channel: u8| {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

fn contrast(a: Color32, b: Color32) -> f32 {
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn supporting_text_and_primary_actions_remain_readable_in_both_themes() {
    for p in [Palette::LIGHT, Palette::DARK] {
        for bg in [
            p.bg_base,
            p.input_surface,
            p.sidebar_fill,
            p.bubble,
            p.nav_active,
        ] {
            for fg in [
                p.label_primary,
                p.label_secondary,
                p.label_tertiary,
                p.label_caption,
            ] {
                assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");
            }
        }
        let c = p.components();
        for bg in [c.btn_info, c.btn_info_hover] {
            assert!(contrast(c.on_info, bg) >= 4.5);
        }
    }
}

/// 语义层级 token 的配对对比度：
/// - 文字（primary / secondary / tertiary / caption）在三層表面上 ≥ 4.5（HC ≥ 7）；
/// - `accent_on_soft` 在 `accent_soft` 上 ≥ 4.5（HC ≥ 7）—— container + on_* 配对；
/// - `muted` 弱描边对其所在的卡片 / 弹窗表面 ≥ 4.5（HC ≥ 7）。
#[test]
fn hierarchy_tokens_meet_contrast_floor_in_all_three_sets() {
    for (name, p, floor) in SETS {
        let surfaces = [
            ("surface_1", p.surface_1),
            ("surface_2", p.surface_2),
            ("surface_3", p.surface_3),
        ];
        let labels = [
            ("label_primary", p.label_primary),
            ("label_secondary", p.label_secondary),
            ("label_tertiary", p.label_tertiary),
            ("label_caption", p.label_caption),
        ];
        for (sname, bg) in surfaces {
            for (lname, fg) in labels {
                assert!(
                    contrast(fg, bg) >= floor,
                    "{name}: {lname} on {sname} = {:.2} < {floor}",
                    contrast(fg, bg)
                );
            }
        }
        let soft = contrast(p.accent_on_soft, p.accent_soft);
        assert!(
            soft >= floor,
            "{name}: accent_on_soft on accent_soft = {soft:.2} < {floor}"
        );
        for (sname, bg) in [("surface_2", p.surface_2), ("surface_3", p.surface_3)] {
            let m = contrast(p.muted, bg);
            assert!(m >= floor, "{name}: muted on {sname} = {m:.2} < {floor}");
        }
    }
}

/// 层级 token 必须是实色：层级关系靠明度差表达，不准用 alpha 叠加
/// （叠两层会串色，且对比度随底色漂移）。
#[test]
fn hierarchy_tokens_are_solid_in_all_three_sets() {
    for (name, p, _) in SETS {
        for (token, c) in [
            ("surface_1", p.surface_1),
            ("surface_2", p.surface_2),
            ("surface_3", p.surface_3),
            ("accent_soft", p.accent_soft),
            ("accent_on_soft", p.accent_on_soft),
            ("muted", p.muted),
        ] {
            assert_eq!(c.a(), 255, "{name}: {token} 不是实色: {c:?}");
        }
    }
}

/// 幂等 / 纯度：token 解析不依赖调用次数与顺序，重复解析逐字节相等。
#[test]
fn palette_resolution_is_pure_and_idempotent() {
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        assert_eq!(mode.palette(), mode.palette());
        assert_eq!(mode, mode.toggled().toggled());
        let p = mode.palette();
        assert_eq!(p.components(), p.components());
        // 从 Theme 再取一次，与直接从模式解析一致。
        let theme = crate::Theme::from_metrics(mode, Metrics::from_scale(1.0));
        assert_eq!(theme.palette, p);
        // 层级 token 不走 alpha 通道，gamma_multiply(1.0) 必须是恒等变换。
        for c in [
            p.surface_1,
            p.surface_2,
            p.surface_3,
            p.accent_soft,
            p.accent_on_soft,
            p.muted,
        ] {
            assert_eq!(c, c.gamma_multiply(1.0));
        }
    }
    // HC 是暗底：组件 token 判别（bg_base 亮度）必须落到暗色那套。
    assert_eq!(Palette::HIGH_CONTRAST.components(), Components::DARK);
    // 新字段有默认值：缺省色板即亮色。
    assert_eq!(Palette::default(), Palette::LIGHT);
    // surface_1 是 bg_base 的语义别名，两边不允许漂移。
    for (name, p, _) in SETS {
        assert_eq!(
            p.surface_1, p.bg_base,
            "{name}: surface_1 与 bg_base 不一致"
        );
    }
}

/// `Theme::apply` 幂等：同一主题重复注入 egui，样式状态不漂移。
#[test]
fn theme_apply_is_idempotent() {
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        let ctx = egui::Context::default();
        let theme = crate::Theme::from_metrics(mode, Metrics::from_scale(1.0));
        let egui_theme = match mode {
            ThemeMode::Dark => egui::Theme::Dark,
            ThemeMode::Light => egui::Theme::Light,
        };
        theme.apply(&ctx);
        let first = ctx.style_of(egui_theme).as_ref().clone();
        theme.apply(&ctx);
        let second = ctx.style_of(egui_theme).as_ref().clone();
        assert_eq!(first, second, "{mode:?}: 重复 apply 后样式漂移");
    }
}
