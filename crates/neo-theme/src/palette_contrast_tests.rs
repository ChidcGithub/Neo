use super::*;

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
