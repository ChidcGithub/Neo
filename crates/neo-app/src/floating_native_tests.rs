//! Off-screen GDI only: no HWND, desktop capture, compositor flush or GUI.
use super::*;

#[test]
fn win10_gdi_labels_fit_circular_buttons_at_common_dpi_scales() {
    unsafe {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
            let mut dib = Dib::new((EXTENT * scale) as i32).unwrap();
            for amount in [0.01, 0.5, 1.0] {
                for language in [crate::i18n::Language::ZhCn, crate::i18n::Language::EnUs] {
                    crate::i18n::with_language(language, || {
                        dib.pixels().fill(0);
                        let mask = labels(&mut dib, scale, amount).unwrap();
                        assert_eq!(mask.len(), (dib.size * dib.size) as usize);
                        assert!(mask.iter().any(|alpha| *alpha != 0));
                    });
                }
                for circle in animated_circles(amount).skip(2) {
                    let bounds = label_bounds(circle, scale);
                    for text in [
                        "屏幕书写\n未实现",
                        "画板\n未实现",
                        "Ink\nNot yet",
                        "Board\nNot yet",
                    ] {
                        dib.pixels().fill(0);
                        let measured =
                            draw_label(dib.dc, text, bounds, (8.5 * scale).round() as i32).unwrap();
                        assert!(measured.cx > 0 && measured.cy > 0, "{text:?}");
                        assert!(
                            measured.cx <= bounds.right - bounds.left,
                            "width: {text:?}, {scale}, {amount}"
                        );
                        assert!(
                            measured.cy <= bounds.bottom - bounds.top,
                            "height: {text:?}, {scale}, {amount}"
                        );
                        assert_ne!(GdiFlush(), 0);
                        assert!(dib.pixels().iter().any(|pixel| pixel & 0x00ffffff != 0));
                    }
                }
            }
        }
    }
}
