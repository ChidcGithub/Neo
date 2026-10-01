use super::*;

#[test]
fn disabled_and_loading_buttons_ignore_focused_keyboard_activation() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75, 2.8] {
            for key in [egui::Key::Enter, egui::Key::Space] {
                for kind in 0..3 {
                    let ctx = egui::Context::default();
                    neo_theme::fonts::install(&ctx);
                    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let d = Design::new(theme);
                    let mut id = None;
                    for step in 0..3 {
                        if let Some(id) = id { ctx.memory_mut(|m| m.request_focus(id)); }
                        let mut output = ctx.run_ui(egui::RawInput {
                            events: if step == 0 { vec![] } else { vec![egui::Event::Key {
                                key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE,
                            }] }, ..Default::default()
                        }, |ui| {
                            let response = match kind {
                                0 => Button::new("删除").id_salt("disabled-key").enabled(step == 0).show(ui, &d),
                                1 => Button::new("提交").id_salt("disabled-key").loading(step != 0).show(ui, &d),
                                _ => IconButton::new(Icon::Trash).id_salt("disabled-key").enabled(step == 0).show_touch(ui, &d),
                            };
                            id = Some(response.id);
                            if step > 0 {
                                assert!(!response.enabled());
                                assert!(!response.clicked(), "禁用键盘触发 {kind}/{key:?}/{scale}");
                                assert!(!response.has_focus());
                            }
                        });
                        output.textures_delta.clear();
                    }
                }
            }
        }
    }
}

#[test]
fn touch_buttons_stay_inside_narrow_parent_clip() {
    for scale in [0.85, 1.0, 1.75, 2.8] {
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        let d = Design::new(neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Dark,
            neo_theme::Metrics::from_scale(scale)));
        for width in [40.0, 96.0, 180.0] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let parent = Rect::from_min_size(egui::pos2(40.0, 40.0), Vec2::new(width, 1000.0));
                crate::at(ui, parent, |ui| {
                    ui.shrink_clip_rect(parent);
                    let mut rects = Vec::new();
                    ui.horizontal_wrapped(|ui| {
                        rects.push(IconButton::new(Icon::Pen).show_touch(ui, &d).rect);
                        rects.push(IconButton::new(Icon::Trash).show_touch(ui, &d).rect);
                        rects.push(Button::new("完整的长操作名称").icon(Icon::Trash).touch_layout().show(ui, &d).rect);
                    });
                    for (i, rect) in rects.iter().enumerate() {
                        assert!(parent.contains_rect(*rect), "热区越界 {width}/{scale}: {rect:?}");
                        for other in &rects[i + 1..] { assert!(!rect.shrink(0.1).intersects(other.shrink(0.1))); }
                    }
                });
            });
            output.textures_delta.clear();
        }
    }
}

#[test]
fn semantic_buttons_keep_full_labels_and_disabled_state() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    neo_theme::fonts::install(&ctx);
    let d = Design::new(neo_theme::Theme::from_metrics(neo_theme::ThemeMode::Light,
        neo_theme::Metrics::from_scale(1.0)));
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let response = Button::new("完整删除说明").enabled(false).touch_layout().show(ui, &d);
        assert!(!response.enabled());
        assert!(!response.clicked());
        let response = IconButton::new(Icon::Trash).label("删除记忆，需确认").enabled(false).show_touch(ui, &d);
        assert!(!response.enabled());
        assert!(!response.clicked());
    });
    output.textures_delta.clear();
    let tree = output.platform_output.accesskit_update.unwrap();
    for label in ["完整删除说明", "删除记忆，需确认"] {
        let (_, node) = tree.nodes.iter().find(|(_, node)| node.label() == Some(label)).unwrap();
        assert!(node.is_disabled());
    }
}

/// **回归测试**：同一面板里两组不同选项，在相同下标上不能拿到同一个 Id。
///
/// 这里的四组选项就是设置面板实际在用的四行（页签 / 主题 / 思考过程 / 观看距离）。
/// 页签第 1 项与思考过程第 1 项都是「显示」—— 正是线上撞车的那一对。
#[test]
fn segment_groups_in_settings_do_not_collide() {
    let base = egui::Id::new("neo-settings-panel");
    let groups: [&[&str]; 4] = [
        &["外观", "显示", "模型", "关于"],
        &["暗色", "亮色"],
        &["隐藏", "显示"],
        &["近距", "教室", "远距"],
    ];
    for (i, a) in groups.iter().enumerate() {
        for b in groups.iter().skip(i + 1) {
            for idx in 0..a.len().min(b.len()) {
                assert_ne!(
                    seg_id(base, None, a, idx),
                    seg_id(base, None, b, idx),
                    "「{}」与「{}」的第 {idx} 段撞了同一个 Id",
                    a.join("/"),
                    b.join("/")
                );
            }
        }
    }
}

/// 同一组选项、不同下标仍然是不同的 Id（否则段与段之间互相覆盖）。
#[test]
fn segments_within_one_group_are_distinct() {
    let base = egui::Id::new("panel");
    let opts = ["隐藏", "显示"];
    assert_ne!(seg_id(base, None, &opts, 0), seg_id(base, None, &opts, 1));
}

/// 两组一字不差的选项：默认会撞，给了 salt 就分开 —— 这就是 salt 存在的理由。
#[test]
fn identical_option_groups_need_an_explicit_salt() {
    let base = egui::Id::new("panel");
    let opts = ["隐藏", "显示"];
    assert_eq!(seg_id(base, None, &opts, 1), seg_id(base, None, &opts, 1));
    assert_ne!(
        seg_id(base, Some("a"), &opts, 1),
        seg_id(base, Some("b"), &opts, 1)
    );
}
