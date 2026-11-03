use super::*;

#[test]
fn embedded_mascot_textures_load_with_the_expected_aspect() {
    let ctx = egui::Context::default();
    let mark = MascotMark::load(&ctx);
    assert!((mark.aspect() - 1024.0 / 523.0).abs() < 0.001);
}

#[test]
fn tray_window_and_app_icons_are_square_and_have_alpha() {
    for (name, (rgba, w, h)) in [
        ("tray black", tray_rgba(64, false)),
        ("tray white", tray_rgba(64, true)),
        ("tray spark black", tray_spark_rgba(64, false)),
        ("tray spark white", tray_spark_rgba(64, true)),
        ("window black", window_rgba(64, false)),
        ("window white", window_rgba(64, true)),
        ("app black", app_rgba(96, false)),
        ("app white", app_rgba(96, true)),
    ] {
        assert_eq!(w, h, "{name}");
        assert_eq!(rgba.len(), (w * h * 4) as usize, "{name}");
        assert!(
            rgba.chunks_exact(4).any(|p| p[3] > 0),
            "{name} has no opaque pixels"
        );
    }
}

#[test]
fn tray_wordmark_fills_the_icon_width() {
    for white in [false, true] {
        let (rgba, w, _) = tray_rgba(64, white);
        let mut min_x = w;
        let mut max_x = 0;
        for (i, pixel) in rgba.chunks_exact(4).enumerate() {
            if pixel[3] > 0 {
                let x = (i as u32) % w;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
        assert!(
            max_x - min_x + 1 >= w - 2,
            "tray wordmark leaves {min_x} px on the left and {} px on the right",
            w - 1 - max_x
        );
    }
}

#[test]
fn tray_size_is_a_plausible_system_icon_size() {
    let size = tray_size();
    assert!((16..=128).contains(&size), "unexpected tray size {size}");
}
