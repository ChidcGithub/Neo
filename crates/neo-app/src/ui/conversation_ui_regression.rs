
use super::*;
use crate::ui::composer::ui_regression::{context, frame_themed, probe};

#[test]
fn english_i18n_tool_status_preserves_raw_errors() {
    crate::i18n::with_language(crate::i18n::Language::EnUs, || {
        assert_eq!(tr("执行成功"), "Succeeded");
        tool_states_are_exclusive_and_errors_remain_accessible();
    });
}

#[test]
fn running_tool_repaints_but_done_tool_settles() {
    use crate::state::{ToolMeta, ToolState};
    let ctx = context();
    let mut msg = crate::state::ChatMessage::new(Role::Tool, String::new());
    msg.tool = Some(ToolMeta {
        call_id: "repaint".into(),
        name: "bash".into(),
        title: "执行",
        risk: "exec",
        preview: "preview".into(),
        args: serde_json::Value::Null,
        state: ToolState::Running,
        outcome: None,
    });
    for state in [ToolState::Running, ToolState::Done] {
        msg.tool.as_mut().unwrap().state = state;
        let mut delay = std::time::Duration::ZERO;
        for _ in 0..8 {
            let output = frame_themed(
                &ctx,
                Vec2::new(320.0, 600.0),
                vec![],
                neo_theme::ThemeMode::Dark,
                |ui, skin| draw_tool_card(ui, skin, &msg, 280.0),
            );
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        }
        if state == ToolState::Running {
            assert!(delay <= std::time::Duration::from_millis(60));
        } else {
            assert_eq!(
                delay,
                std::time::Duration::MAX,
                "完成态不得继续请求动画重绘"
            );
        }
    }
}

#[test]
fn tool_states_are_exclusive_and_errors_remain_accessible() {
    use crate::state::{ToolMeta, ToolState};
    let successful = ToolMeta {
        call_id: "success".into(),
        name: "bash".into(),
        title: "执行",
        risk: "exec",
        preview: "preview".into(),
        args: serde_json::Value::Null,
        state: ToolState::Done,
        outcome: Some(neo_tools::Outcome::ok(
            "bash",
            "done",
            serde_json::Value::Null,
        )),
    };
    assert_eq!(
        tool_status(&successful),
        (tr("执行成功"), neo_ui::NoticeTone::Success)
    );
    let message = "执行失败原因 very_long_token_".repeat(8);
    let hint = "检查路径及权限后手动决定下一步".repeat(4);
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for width in [240.0, 320.0, 560.0] {
            for scale in [0.85, 1.0, 1.75, 2.8] {
                for state in [
                    ToolState::AwaitingConfirm,
                    ToolState::Running,
                    ToolState::Done,
                    ToolState::Denied,
                    ToolState::Cancelled,
                ] {
                    let ctx = context();
                    ctx.enable_accesskit();
                    let theme =
                        neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
                    theme.apply(&ctx);
                    let whale = crate::brand::WhaleMark::cached(&ctx);
                    let skin = Skin::new(theme, &whale);
                    let mut outcome = neo_tools::Outcome::fail(
                        "bash",
                        neo_tools::ToolError::io(&message).with_hint(&hint),
                    );
                    outcome.data = serde_json::json!({"exit_code": -1073741819_i64});
                    let mut msg = crate::state::ChatMessage::new(Role::Assistant, String::new());
                    msg.tool = Some(ToolMeta {
                        call_id: "stable-tool".into(),
                        name: "bash".into(),
                        title: "执行",
                        risk: "exec",
                        preview: "preview".into(),
                        args: serde_json::json!({"command": "echo summary"}),
                        state,
                        outcome: Some(outcome),
                    });
                    let (status, tone) = tool_status(msg.tool.as_ref().unwrap());
                    if state == ToolState::Cancelled {
                        assert_eq!(tone, neo_ui::NoticeTone::Neutral);
                    }
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                egui::Pos2::ZERO,
                                Vec2::new(width, 6000.0),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            ui.set_max_width(width - 16.0);
                            draw_tool_card(ui, &skin, &msg, width - 16.0);
                        },
                    );
                    output.textures_delta.clear();
                    let texts: Vec<_> = output
                        .shapes
                        .iter()
                        .filter_map(|s| match &s.shape {
                            egui::Shape::Text(t) => Some(t),
                            _ => None,
                        })
                        .collect();
                    assert!(texts.iter().any(|t| t.galley.text() == status));
                    if tone == neo_ui::NoticeTone::Error {
                        let full = tf(
                            "{error}（建议：{hint}）",
                            &[("error", message.clone()), ("hint", hint.clone())],
                        );
                        let text = texts.iter().find(|t| t.galley.text() == full).unwrap();
                        assert!(!text.galley.elided);
                        assert!(text.pos.x + text.galley.size().x <= width);
                        let tree = output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .expect("AccessKit enabled by eframe");
                        assert!(tree
                            .nodes
                            .iter()
                            .any(|(_, node)| node.value() == Some(full.as_str())));
                    } else {
                        assert!(!texts.iter().any(|t| t.galley.text().contains(&message)));
                    }
                    if state == ToolState::Cancelled {
                        assert!(!texts.iter().any(|t| t.fallback_color == skin.p().error));
                    }
                }
            }
        }
    }
}

#[test]
fn long_code_and_wide_table_stay_readable_inside_message_column() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for width in [320.0, 768.0, 1920.0] {
            let ctx = context();
            let code = format!("CODE_START{}CODE_END", "long_token_".repeat(50));
            let table = format!(
                "| {} |\n| {} |\n| {} |\n",
                (0..12)
                    .map(|i| format!("Header{i}"))
                    .collect::<Vec<_>>()
                    .join(" | "),
                vec!["---"; 12].join(" | "),
                (0..12)
                    .map(|i| format!("Cell{i} {}", "longword".repeat(10)))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            let mut state = AppState::default();
            state.messages.push(crate::state::ChatMessage::new(
                Role::Assistant,
                format!("```text\n{code}\n```\n\n{table}\n\nAFTER_TABLE"),
            ));
            let size = Vec2::new(width, 1800.0);
            let area = Rect::from_min_size(egui::pos2(8.0, 8.0), size - Vec2::splat(16.0));
            let mut render =
                |ui: &mut Ui, skin: &Skin<'_>| draw_messages(ui, skin, area, &mut state);
            for _ in 0..4 {
                frame_themed(&ctx, size, vec![], mode, &mut render);
            }
            let output = frame_themed(&ctx, size, vec![], mode, &mut render);
            let (viewport, content, _): (Rect, Vec2, Vec2) =
                probe(&ctx, "neo-test-thread-geometry");
            assert!(
                content.x <= viewport.width() + 1.0,
                "width={width}: {content:?}, {viewport:?}"
            );
            let mut found_code = false;
            let mut found_tail = false;
            let mut cells = 0;
            let mut table_pos = None;
            for clipped in &output.shapes {
                if let egui::Shape::Text(text) = &clipped.shape {
                    let source = &text.galley.job.text;
                    if source.contains("CODE_START") {
                        found_code = true;
                        assert!(source.contains("CODE_END"), "代码不能丢失尾部");
                        assert!(text.galley.rows.len() > 1, "长代码应按消息列换行");
                        assert!(text.pos.x + text.galley.size().x <= viewport.right() + 1.0);
                    }
                    if source.starts_with("Cell") {
                        cells += 1;
                        let visible = clipped
                            .clip_rect
                            .intersect(Rect::from_min_size(text.pos, text.galley.size()));
                        if visible.is_positive() {
                            table_pos = Some(visible.center());
                        }
                        assert!(clipped.clip_rect.right() <= viewport.right() + 1.0);
                        assert!(clipped.clip_rect.left() >= viewport.left() - 1.0);
                    }
                    if source == "AFTER_TABLE" {
                        found_tail = true;
                    }
                }
            }
            assert!(
                found_code && found_tail && cells > 0,
                "代码、表格及后续正文都必须绘制"
            );
            // 横向滚到表尾，最后一列必须能真正进入局部裁剪区。
            for _ in 0..8 {
                frame_themed(
                    &ctx,
                    size,
                    vec![
                        egui::Event::PointerMoved(table_pos.unwrap()),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: Vec2::new(-2000.0, 0.0),
                            phase: egui::TouchPhase::Move,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    mode,
                    &mut render,
                );
            }
            let output = frame_themed(&ctx, size, vec![], mode, &mut render);
            assert!(
                output.shapes.iter().any(|clipped| {
                    if let egui::Shape::Text(text) = &clipped.shape {
                        text.galley.job.text.starts_with("Cell11")
                            && clipped
                                .clip_rect
                                .expand(1.0)
                                .contains_rect(Rect::from_min_size(text.pos, text.galley.size()))
                    } else {
                        false
                    }
                }),
                "width={width}: 最后一列必须能横向滚动至完整可见"
            );
        }
    }
}
