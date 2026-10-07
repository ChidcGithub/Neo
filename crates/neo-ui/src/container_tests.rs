//! Panel 表面层级测试：默认底是 `surface_2`、描边是 `muted`，`fill` 覆盖 honored。
//! 外加几何稳定（内容区 = 外框 − pad，逐像素相等）与描边对比度底线。

use super::*;

/// 从一整帧的输出里收集所有填充色（Path / Rect 形状的 fill）。
fn fills(output: &egui::FullOutput) -> Vec<Color32> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Path(path) if path.fill != Color32::TRANSPARENT => Some(path.fill),
            egui::Shape::Rect(r) if r.fill != Color32::TRANSPARENT => Some(r.fill),
            _ => None,
        })
        .collect()
}

/// 从一整帧的输出里收集所有路径描边色（squircle 描边是 Path；阴影描边为 NONE）。
fn strokes(output: &egui::FullOutput) -> Vec<Color32> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Path(path) if !path.stroke.is_empty() => match path.stroke.color {
                egui::epaint::ColorMode::Solid(c) => Some(c),
                egui::epaint::ColorMode::UV(_) => None,
            },
            _ => None,
        })
        .collect()
}

fn setup(mode: neo_theme::ThemeMode) -> (egui::Context, Design) {
    let ctx = egui::Context::default();
    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0));
    theme.apply(&ctx);
    (ctx, Design::new(theme))
}

fn paint_panel(ctx: &egui::Context, d: &Design, panel: &Panel) -> egui::FullOutput {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let rect = Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(240.0, 160.0));
        panel.paint(ui, d, rect, 16.0);
    });
    output.textures_delta.clear();
    output
}

#[test]
fn panel_default_fill_is_surface_2() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (ctx, d) = setup(mode);
        let output = paint_panel(&ctx, &d, &Panel::new());
        let want = d.p().surface_2;
        assert!(
            fills(&output).contains(&want),
            "{mode:?}: Panel 默认底应为 surface_2 {want:?}"
        );
    }
}

#[test]
fn panel_fill_override_is_honored() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (ctx, d) = setup(mode);
        let output = paint_panel(&ctx, &d, &Panel::new().fill(d.p().surface_3));
        let (want, default) = (d.p().surface_3, d.p().surface_2);
        let fills = fills(&output);
        assert!(
            fills.contains(&want),
            "{mode:?}: fill(surface_3) 未生效 {want:?}"
        );
        assert!(
            !fills.contains(&default),
            "{mode:?}: 覆盖后不应再出现默认底 surface_2 {default:?}"
        );
    }
}

/// 描边 token：层级契约里的 `muted`（实色弱描边），不是 alpha 叠加的 border_l2。
#[test]
fn panel_border_is_muted() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let (ctx, d) = setup(mode);
        let output = paint_panel(&ctx, &d, &Panel::new());
        let want = d.p().muted;
        assert!(
            strokes(&output).contains(&want),
            "{mode:?}: Panel 描边应为 muted {want:?}"
        );
        // muted 是实色（层级 token 不走 alpha），画出来的也必须是不透明的。
        assert_eq!(want.a(), 255, "{mode:?}: muted 应为实色");
    }
}

/// 几何稳定：内容区 = 外框四边各收 `pad`，逐像素相等；
/// 圆角覆盖 / 缩放 / 主题都不动这个式子。Panel 无交互态，天然不该有尺寸抖动。
#[test]
fn panel_content_rect_is_exact_inset() {
    let outer = Rect::from_min_size(egui::pos2(40.0, 40.0), egui::vec2(240.0, 160.0));
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let ctx = egui::Context::default();
            let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
            theme.apply(&ctx);
            let d = Design::new(theme);
            for pad in [0.0, 8.0, 16.0] {
                for panel in [Panel::new(), Panel::new().radius(4.0)] {
                    let mut got = None;
                    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                        got = Some(panel.paint(ui, &d, outer, pad));
                    });
                    output.textures_delta.clear();
                    let want = crate::base::inset_all(outer, pad);
                    assert_eq!(
                        got.unwrap(),
                        want,
                        "{mode:?} scale={scale} pad={pad}: 内容区不等于 inset_all(外框, pad)"
                    );
                }
            }
        }
    }
}

/// 组件级对比度：Panel 实际画出的描边对其默认底（surface_2）与
/// 弹层底（surface_3）都要 ≥ 4.5；高对比色板下 ≥ 7（WCAG AAA）。
///
/// 这条守的是「接线」而非 token 本身 —— token 的配对底线在 neo-theme
/// 已经测过；这里防的是哪天 Panel 被改回 border_l2 或某个无契约的颜色。
#[test]
fn panel_border_meets_contrast_floor_on_its_surfaces() {
    fn luminance(c: Color32) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    }
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    let sets: [(&str, neo_theme::Palette, f32); 3] = [
        ("light", neo_theme::Palette::LIGHT, 4.5),
        ("dark", neo_theme::Palette::DARK, 4.5),
        ("hc", neo_theme::Palette::HIGH_CONTRAST, 7.0),
    ];
    for (name, palette, floor) in sets {
        // 走 Design 的真实路径取色，而不是直接读 token —— 测的是组件用色。
        let theme = neo_theme::Theme {
            // HC 尚未接入 ThemeMode；它亮度上是暗底，用 Dark 占位
            // （Palette::components() 同样按亮度判别，取值一致）。
            mode: neo_theme::ThemeMode::Dark,
            palette,
            metrics: neo_theme::Metrics::from_scale(1.0),
            typo: neo_theme::Typography::new(&neo_theme::Metrics::from_scale(1.0)),
        };
        let ctx = egui::Context::default();
        theme.apply(&ctx);
        let d = Design::new(theme);

        for fill in [None, Some(palette.surface_3)] {
            let panel = match fill {
                Some(f) => Panel::new().fill(f),
                None => Panel::new(),
            };
            let output = paint_panel(&ctx, &d, &panel);
            let border = strokes(&output)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("{name}: Panel 没有画出描边"));
            let surface = fill.unwrap_or(palette.surface_2);
            let ratio = contrast(border, surface);
            assert!(
                ratio >= floor,
                "{name}: Panel 描边 {border:?} 对底 {surface:?} 对比度 {ratio:.2} < {floor}"
            );
        }
    }
}
