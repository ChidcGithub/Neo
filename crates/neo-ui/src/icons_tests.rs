use super::*;

/// 每个图标都映射到恰好一个私有使用区（PUA）字符 —— 守住码表别打错。
#[test]
fn every_icon_maps_to_a_single_pua_char() {
    for &icon in ALL {
        let mut chars = icon.glyph().chars();
        let Some(c) = chars.next() else {
            panic!("{icon:?} 的字形是空串");
        };
        assert!(
            ('\u{E000}'..='\u{F8FF}').contains(&c),
            "{icon:?} 的码位 {c:?} 不在私有使用区"
        );
        assert!(chars.next().is_none(), "{icon:?} 的字形不止一个字符");
    }
}

/// `ALL` 必须不重不漏：长度等于变体数，且没有重复项。
/// （新增变体忘了登记进 `ALL` 时，遍历测试会静默漏掉它。）
#[test]
fn all_covers_every_variant_exactly_once() {
    const VARIANTS: usize = 23; // 与枚举变体数同步；加图标时一起改。
    assert_eq!(ALL.len(), VARIANTS, "ALL 漏登或多登了图标");
    for (i, a) in ALL.iter().enumerate() {
        assert!(!ALL[..i].contains(a), "{a:?} 在 ALL 中重复出现");
    }
}

/// 尺寸网格：字号恒为「短边 × 1.25」。
///
/// 这条乘数是全库唯一的图标缩放规则 —— 按钮 16px、chip 箭头 10px、
/// 行内动作 15px 都是「给 paint 的矩形边长」，真正喂给字体光栅器的
/// 磅值必须逐字节等于这里算出来的值，否则同档图标会大小不一。
#[test]
fn glyph_size_is_short_edge_times_grid_factor() {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    for (w, h) in [(16.0, 16.0), (10.0, 14.0), (15.0, 12.0), (24.0, 24.0)] {
        let rect = Rect::from_min_size(egui::pos2(3.0, 5.0), egui::vec2(w, h));
        let want = w.min(h) * 1.25;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            Icon::Check.paint(ui.painter(), rect, Color32::WHITE);
        });
        output.textures_delta.clear();
        let galley = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(t) => Some(t),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{rect:?}: 没有画出文本形状"));
        assert_eq!(
            galley.galley.job.sections[0].format.font_id.size, want,
            "{rect:?}: 图标字号不在网格上（want {want}）"
        );
    }
}

/// 几何稳定：同一图标同一矩形，连续两帧画出的形状逐字节相等
/// （字号、中心点都不允许随帧漂移）。
#[test]
fn paint_is_deterministic_frame_over_frame() {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let rect = Rect::from_center_size(egui::pos2(100.0, 60.0), egui::vec2(16.0, 16.0));
    let mut frames = Vec::new();
    for _ in 0..2 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            Icon::Search.paint(ui.painter(), rect, Color32::WHITE);
        });
        output.textures_delta.clear();
        frames.push(format!("{:?}", output.shapes));
    }
    assert_eq!(frames[0], frames[1], "同一输入两帧画法不一致");
}

/// 退化输入必须静默跳过：零面积矩形 / 全透明颜色都不该产生任何形状。
#[test]
fn degenerate_input_paints_nothing() {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let p = ui.painter();
        let zero = Rect::from_center_size(egui::pos2(10.0, 10.0), egui::vec2(0.0, 16.0));
        Icon::Plus.paint(p, zero, Color32::WHITE);
        Icon::Plus.paint(
            p,
            Rect::from_center_size(egui::pos2(10.0, 10.0), egui::vec2(16.0, 16.0)),
            Color32::TRANSPARENT,
        );
    });
    output.textures_delta.clear();
    assert!(
        output.shapes.is_empty(),
        "退化输入不应产生形状: {:?}",
        output.shapes
    );
}
