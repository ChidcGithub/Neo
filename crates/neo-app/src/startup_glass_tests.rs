use super::*;

#[test]
fn render_is_bounded_and_rejects_invalid_shapes() {
    assert!(render(&[], 0, 1, 360, 120).is_none());
    assert!(render(&[0], 2, 1, 360, 120).is_none());
    assert!(render(&[0], 1, 1, -1, 120).is_none());
    assert!(render(&[0], 1, 1, i32::MAX, i32::MAX).is_none());
    assert!(render(&[], 513, 1, 360, 120).is_none());
}

#[test]
fn blur_removes_fine_detail_and_retains_a_light_gray_plate() {
    let source: Vec<_> = (0..30).flat_map(|y| (0..90).map(move |x| {
        if (x + y) % 2 == 0 { 0 } else { 0xffffff }
    })).collect();
    let output = render(&source, 90, 30, 360, 120).unwrap();
    assert_eq!(output.len(), 360 * 120);
    for pixel in &output {
        assert_eq!(pixel >> 24, 255);
        for shift in [0, 8, 16] {
            let value = (pixel >> shift) & 255;
            // Edge extension affects checker corners more with the lighter tint.
            assert!((154..=219).contains(&value), "tinted channel={value}");
        }
    }
    // Original black/white checker contrast is gone, but subtle grain survives.
    let row = &output[60 * 360..61 * 360];
    let min = row.iter().map(|p| p & 255).min().unwrap();
    let max = row.iter().map(|p| p & 255).max().unwrap();
    assert!(max - min <= 4);
    assert_eq!(output, render(&source, 90, 30, 360, 120).unwrap());
}

#[test]
fn backdrop_color_survives_blur_without_exposing_sharp_content() {
    let dark = render(&[0; 16], 4, 4, 12, 4).unwrap();
    let light = render(&[0xffffff; 16], 4, 4, 12, 4).unwrap();
    // Tint is brighter, but still contributes exactly half of each channel.
    assert_eq!(dark[0], 0xff7b7c7e);
    for (a, b) in dark.iter().zip(light) {
        for shift in [0, 8, 16] {
            let difference = ((b >> shift) & 255) - ((a >> shift) & 255);
            assert!((127..=128).contains(&difference), "backdrop weight must remain 50%");
        }
    }
}
