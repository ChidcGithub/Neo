
    use super::Geometry;
    use egui::{pos2, Rect, Vec2};

    #[test]
    fn fixed_regions_stay_inside_panel_and_do_not_overlap() {
        for scale in [1.0, 1.25, 1.6, 2.5] {
            for body_h in [80.0, 240.0, 600.0] {
                let pad = 22.0 * scale;
                let title = 34.0 * scale;
                let footer = 36.0 * scale;
                let gap = 12.0 * scale;
                let panel = Rect::from_min_size(
                    pos2(15.0, 25.0),
                    Vec2::new(
                        520.0 * scale,
                        2.0 * pad + title + footer + 2.0 * gap + body_h,
                    ),
                );
                let g = Geometry::new(panel, pad, title, footer, gap);
                assert!(panel.contains_rect(g.body));
                assert!(g.title.bottom() < g.body.top());
                assert!(g.body.bottom() < g.footer.top());
                assert!((g.body.height() - body_h).abs() < 0.01);
            }
        }
    }
