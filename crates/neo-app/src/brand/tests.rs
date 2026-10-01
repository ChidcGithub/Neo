
    use super::*;

    #[test]
    fn parses_four_subpaths() {
        // 上游路径由 4 条子路径组成：身体 + 鳍挖空 + 眼睛 + 鳍/嘴挖空。
        // 数量由 `d` 字符串里的 M/Z 对决定，与 SVG 源一一对应。
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        assert_eq!(subpaths.len(), 4, "子路径数量与上游不符");
        for sp in &subpaths {
            assert!(sp.len() > 3, "子路径点数过少: {}", sp.len());
        }
    }

    #[test]
    fn all_points_inside_viewbox() {
        for sp in svg::parse_subpaths(FISH_LOGO_PATH) {
            for p in sp {
                assert!(p[0] >= -1.0 && p[0] <= VIEWBOX_W + 1.0, "x 越界: {}", p[0]);
                assert!(p[1] >= -1.0 && p[1] <= VIEWBOX_H + 1.0, "y 越界: {}", p[1]);
            }
        }
    }

    #[test]
    fn eye_and_fin_are_cut_out() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        let (w, h, ss) = (256usize, 188usize, 4usize);
        let cov = svg::rasterize(
            &subpaths,
            (VIEWBOX_W, VIEWBOX_H),
            w,
            h,
            ss,
            svg::FillRule::EvenOdd,
        );
        let at = |x: f32, y: f32| -> f32 {
            let px = ((x / VIEWBOX_W) * w as f32) as usize;
            let py = ((y / VIEWBOX_H) * h as f32) as usize;
            cov[py.min(h - 1) * w + px.min(w - 1)]
        };
        // 身体实心区（鲸鱼左侧下腹）。
        assert!(at(2.0, 10.0) > 0.9, "身体区域未被填充: {}", at(2.0, 10.0));
        assert!(at(10.0, 8.0) > 0.9, "身体区域未被填充: {}", at(10.0, 8.0));
        // 眼睛与鳍在上游是挖空 —— 必须透出背景。
        assert!(at(12.44, 8.26) < 0.2, "眼睛未成为镂空: {}", at(12.44, 8.26));
        assert!(at(14.0, 8.5) < 0.2, "鳍未成为镂空: {}", at(14.0, 8.5));
    }

    /// 上游 SVG 用默认的非零填充；这里用奇偶规则实现。
    /// 两者只有在子路径互相重叠时才会分歧，因此需要一条测试把「当前几何下等价」钉住 ——
    /// 一旦上游改了鲸鱼路径（比如子路径开始重叠），这条会先炸。
    #[test]
    fn even_odd_matches_nonzero_on_this_geometry() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);

        let even_odd = |x: f32, y: f32| -> bool {
            let mut crossings = Vec::new();
            for sp in &subpaths {
                for i in 0..sp.len() {
                    let a = sp[i];
                    let b = sp[(i + 1) % sp.len()];
                    if (a[1] <= y) != (b[1] <= y) {
                        let t = (y - a[1]) / (b[1] - a[1]);
                        crossings.push(a[0] + t * (b[0] - a[0]));
                    }
                }
            }
            crossings.sort_by(|p, q| p.partial_cmp(q).unwrap());
            crossings.chunks_exact(2).any(|c| x > c[0] && x < c[1])
        };

        let nonzero = |x: f32, y: f32| -> bool {
            let mut winding = 0i32;
            for sp in &subpaths {
                for i in 0..sp.len() {
                    let a = sp[i];
                    let b = sp[(i + 1) % sp.len()];
                    let crosses_up = a[1] <= y && b[1] > y;
                    let crosses_down = b[1] <= y && a[1] > y;
                    if !crosses_up && !crosses_down {
                        continue;
                    }
                    let t = (y - a[1]) / (b[1] - a[1]);
                    let x_at = a[0] + t * (b[0] - a[0]);
                    if x_at > x {
                        winding += if crosses_up { 1 } else { -1 };
                    }
                }
            }
            winding != 0
        };

        for iy in 0..40 {
            for ix in 0..54 {
                let x = (ix as f32 + 0.5) / 54.0 * VIEWBOX_W;
                let y = (iy as f32 + 0.5) / 40.0 * VIEWBOX_H;
                assert_eq!(
                    even_odd(x, y),
                    nonzero(x, y),
                    "填充规则在 ({x:.3}, {y:.3}) 处出现分歧 —— 光栅化需要改用非零规则"
                );
            }
        }
    }

    #[test]
    fn coverage_is_binary_inside_and_outside() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        let (w, h) = (128usize, 94usize);
        let cov = svg::rasterize(
            &subpaths,
            (VIEWBOX_W, VIEWBOX_H),
            w,
            h,
            2,
            svg::FillRule::EvenOdd,
        );
        let filled = cov.iter().filter(|c| **c > 0.9).count();
        assert!(filled > 0, "没有任何像素被填充");
        // 角落必须为空（鲸鱼轮廓不触及 viewBox 右上角）。
        assert!(cov[0] < 0.1, "左上角不应被填充");
        assert!(cov[w - 1] < 0.1, "右上角不应被填充");
    }
