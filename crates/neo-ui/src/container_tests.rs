//! Panel 表面层级测试：默认底是 `surface_2`，`fill` 覆盖 honored。

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
