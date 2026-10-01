use super::*;

#[test]
fn arcs_consume_seven_parameters_and_preserve_following_commands() {
    assert_eq!(
        parse_subpaths("M1 2 A3 4 45 0 1 10 20 L30 40"),
        vec![vec![[1.0, 2.0], [10.0, 20.0], [30.0, 40.0]]],
    );
    assert_eq!(
        parse_subpaths("M1 2 a3 4 0 01-5 6 3 4 0 10 2 3 l1 1"),
        vec![vec![[1.0, 2.0], [-4.0, 8.0], [-2.0, 11.0], [-1.0, 12.0]]],
    );
    assert_eq!(
        parse_subpaths("M0 0A3 4 0 0110 20L30 40"),
        parse_subpaths("M0 0L10 20L30 40"),
    );
}

#[test]
fn smooth_curves_reflect_only_the_immediately_preceding_curve_kind() {
    for separator in ["L20 20", "H20", "V20", "A1 1 0 0 1 20 20", "M20 20", "ZM20 20"] {
        let prefix = format!("M0 0C0 12 8 12 10 10{separator}");
        let pos = if separator == "H20" { "20 10" } else if separator == "V20" { "10 20" } else { "20 20" };
        assert_eq!(parse_subpaths(&format!("{prefix}S30 40 40 20")),
            parse_subpaths(&format!("{prefix}C{pos} 30 40 40 20")), "{separator}");
        let prefix = format!("M0 0Q8 12 10 10{separator}");
        assert_eq!(parse_subpaths(&format!("{prefix}T40 20")),
            parse_subpaths(&format!("{prefix}Q{pos} 40 20")), "{separator}");
    }
    assert_eq!(
        parse_subpaths("M0 0C0 10 5 10 10 0Q15 10 20 0S25 10 30 0"),
        parse_subpaths("M0 0C0 10 5 10 10 0Q15 10 20 0C20 0 25 10 30 0"),
    );
    assert_eq!(
        parse_subpaths("M0 0Q5 10 10 0C15 10 15 10 20 0T30 0"),
        parse_subpaths("M0 0Q5 10 10 0C15 10 15 10 20 0Q20 0 30 0"),
    );
}

#[test]
fn smooth_curve_chains_keep_reflection_for_relative_and_implicit_segments() {
    assert_eq!(
        parse_subpaths("M0 0C0 10 5 10 10 0s5 -10 10 0 5 10 10 0"),
        parse_subpaths("M0 0C0 10 5 10 10 0C15 -10 15 -10 20 0C25 10 25 10 30 0"),
    );
    assert_eq!(
        parse_subpaths("M0 0Q5 10 10 0t10 0 10 0"),
        parse_subpaths("M0 0Q5 10 10 0Q15 -10 20 0Q25 10 30 0"),
    );
}

#[test]
fn current_whale_geometry_and_cutouts_are_preserved() {
    use super::whale_fixture::*;
    let paths = parse_subpaths(FISH_LOGO_PATH);
    assert_eq!(paths.len(), 4);
    for point in paths.iter().flatten() {
        assert!((-1.0..=VIEWBOX_W + 1.0).contains(&point[0]));
        assert!((-1.0..=VIEWBOX_H + 1.0).contains(&point[1]));
    }
    let coverage = rasterize(&paths, (VIEWBOX_W, VIEWBOX_H), 256, 188, 4, FillRule::EvenOdd);
    let at = |x: f32, y: f32| coverage[(y / VIEWBOX_H * 188.0) as usize * 256 + (x / VIEWBOX_W * 256.0) as usize];
    assert!(at(2.0, 10.0) > 0.9);
    assert!(at(10.0, 8.0) > 0.9);
    assert!(at(12.44, 8.26) < 0.2);
    assert!(at(14.0, 8.5) < 0.2);
}

#[test]
fn parses_absolute_square() {
    let sp = parse_subpaths("M0 0L10 0L10 10L0 10Z");
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0].len(), 4);
    assert_eq!(sp[0][0], [0.0, 0.0]);
    assert_eq!(sp[0][2], [10.0, 10.0]);
}

#[test]
fn supports_relative_and_implicit_repeat() {
    // 隐含重复：M 之后的两组坐标按 L 处理。
    let sp = parse_subpaths("M0 0 5 0 5 5Z");
    assert_eq!(sp[0].len(), 3);
    assert_eq!(sp[0][2], [5.0, 5.0]);
    // 相对命令
    let sp = parse_subpaths("m1 1l2 0l0 2z");
    assert_eq!(sp[0][0], [1.0, 1.0]);
    assert_eq!(sp[0][2], [3.0, 3.0]);
}

#[test]
fn supports_h_v_and_scientific_notation() {
    let sp = parse_subpaths("M1 1H9V9H1Z");
    assert_eq!(sp[0][1], [9.0, 1.0]);
    assert_eq!(sp[0][2], [9.0, 9.0]);

    // 上游 IconFolderClose16 里的写法
    let sp = parse_subpaths("M1.2e-05 3L4 3Z");
    assert_eq!(sp[0][0][0], 1.2e-05);
}

#[test]
fn supports_quadratic_via_elevation() {
    let sp = parse_subpaths("M0 0Q5 10 10 0Z");
    // 曲线被展平：点数明显多于 3，且中段向上凸（y 大于 0）
    assert!(sp[0].len() > 5, "二次曲线未展平");
    let max_y = sp[0].iter().map(|p| p[1]).fold(f32::MIN, f32::max);
    assert!(max_y > 4.0, "曲线没有拱起来: {max_y}");
}

#[test]
fn multiple_subpaths_are_split() {
    let sp = parse_subpaths("M0 0L4 0L4 4ZM6 6L8 6L8 8Z");
    assert_eq!(sp.len(), 2);
}

#[test]
fn nonzero_cuts_a_hole() {
    // 外框顺时针、内框逆时针：nonzero 下中间必须空。
    let outer = "M0 0L16 0L16 16L0 16Z";
    let inner = "M4 4L4 12L12 12L12 4Z"; // 反向绕行
    let sp = parse_subpaths(&format!("{outer}{inner}"));
    let cov = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::NonZero);
    // 像素 (2,2) → viewBox ≈(1,1)：在外框上、洞外
    assert!(cov[2 * 32 + 2] > 0.9, "外框没被填");
    // 像素 (16,16) → viewBox ≈(8.25,8.25)：洞中心
    assert!(cov[16 * 32 + 16] < 0.1, "内框没有成为洞");
}

#[test]
fn even_odd_differs_from_nonzero_on_overlap() {
    // 同向绕行的两个重叠矩形：nonzero 全填，even-odd 中间留洞。
    let a = "M0 0L10 0L10 10L0 10Z";
    let b = "M5 5L15 5L15 15L5 15Z";
    let sp = parse_subpaths(&format!("{a}{b}"));
    let nz = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::NonZero);
    let eo = rasterize(&sp, (16.0, 16.0), 32, 32, 2, FillRule::EvenOdd);
    // 重叠区（约 (7.5, 7.5) → 像素 15,15）nonzero 实心、even-odd 空
    assert!(nz[15 * 32 + 15] > 0.9, "nonzero 重叠区应为实心");
    assert!(eo[15 * 32 + 15] < 0.1, "even-odd 重叠区应为空");
}

#[test]
fn degenerate_input_does_not_panic() {
    for d in ["", "M", "M0", "L3 3", "M0 0A1 1 0 0 1 2 2", "M0 0Q1"] {
        let _ = parse_subpaths(d);
    }
}

#[test]
fn bounds_are_computed() {
    let sp = parse_subpaths("M1 2L5 2L5 9L1 9Z");
    assert_eq!(bounds(&sp), Some([1.0, 2.0, 5.0, 9.0]));
    assert_eq!(bounds(&[]), None);
}
