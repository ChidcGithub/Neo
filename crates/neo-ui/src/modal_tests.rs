//! Modal 表面层级测试：模态卡片用最上层表面 `surface_3`。

use super::*;

/// 从一整帧的输出里收集所有填充色（Path / Rect 形状的 fill）。
fn fills(output: &egui::FullOutput) -> Vec<egui::Color32> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Path(path) if path.fill != egui::Color32::TRANSPARENT => Some(path.fill),
            egui::Shape::Rect(r) if r.fill != egui::Color32::TRANSPARENT => Some(r.fill),
            _ => None,
        })
        .collect()
}

#[test]
fn modal_paints_surface_3() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(1.0));
        theme.apply(&ctx);
        let d = Design::new(theme);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let body = Modal::new("层级测试", ModalSize::Sm).begin(ui, &d, 200.0);
            assert!(body.is_some());
        });
        output.textures_delta.clear();

        let (want, panel_default) = (d.p().surface_3, d.p().surface_2);
        let fills = fills(&output);
        assert!(
            fills.contains(&want),
            "{mode:?}: 模态卡片应为 surface_3 {want:?}"
        );
        assert!(
            !fills.contains(&panel_default),
            "{mode:?}: 模态不该落回 Panel 默认的 surface_2 {panel_default:?}"
        );
    }
}
