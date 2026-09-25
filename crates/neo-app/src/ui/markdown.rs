//! CommonMark/GFM message rendering, backed by egui_commonmark.
//!
//! The local upstream compatibility patch constrains tables, retains column
//! alignment and prevents model-authored links/images from invoking local files.
//! There is no lossy intermediate block AST or zero-width formula placeholder.

use std::sync::{Arc, Mutex};

use egui::{Id, RichText, TextStyle, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use super::{math, Skin};

/// Cache syntax definitions across messages/frames, but never cache static message
/// geometry: streamed text, width, theme and scale can all change independently.
pub fn render(ui: &mut Ui, skin: &Skin<'_>, text: &str, streaming: bool) {
    let cache = ui.ctx().data_mut(|data| {
        data.get_temp_mut_or_insert_with(Id::new("neo-commonmark-cache"), || {
            Arc::new(Mutex::new(CommonMarkCache::default()))
        })
        .clone()
    });
    // Do not hold a Context data lock while the renderer calls back into egui.
    let mut cache = cache.lock().unwrap_or_else(|poison| poison.into_inner());
    ui.scope(|ui| {
        configure(ui, skin);
        let body = skin.t().body;
        let color = skin.p().label_primary;
        let math_fn = move |ui: &mut Ui, latex: &str, inline: bool| {
            let size = body * if inline { 1.0 } else { 1.12 };
            if inline {
                draw_math(ui, latex, size, false, color);
            } else {
                // A display formula is a separate block, not an inline widget.
                ui.label("\n");
                ui.vertical(|ui| {
                    ui.add_space((body * 0.3).ceil());
                    draw_math(ui, latex, size, true, color);
                    ui.add_space((body * 0.3).ceil());
                    let mut aligned = ui.min_rect();
                    aligned.max.y = aligned.max.y.ceil();
                    ui.expand_to_include_rect(aligned);
                });
                ui.label("\n");
            }
        };
        CommonMarkViewer::new()
            .explicit_image_uri_scheme(true)
            .enable_scroll_to_heading(true)
            .render_math_fn(Some(&math_fn))
            .show(ui, &mut cache, text);
        if streaming {
            ui.label(RichText::new("▍").color(skin.p().accent));
        }
    });
}

fn configure(ui: &mut Ui, skin: &Skin<'_>) {
    let p = skin.p();
    let body = skin.t().body;
    let style = ui.style_mut();
    style.override_font_id = None;
    style.override_text_style = None;
    style.text_styles.insert(TextStyle::Body, skin.prop(body));
    style.text_styles.insert(TextStyle::Button, skin.prop(body));
    style
        .text_styles
        .insert(TextStyle::Heading, skin.bold(body * 1.45));
    style
        .text_styles
        .insert(TextStyle::Small, skin.prop(body * 0.82));
    style
        .text_styles
        .insert(TextStyle::Monospace, skin.mono(body * 0.93));
    style.visuals.override_text_color = Some(p.label_primary);
    style.visuals.weak_text_color = Some(p.label_secondary);
    style.visuals.hyperlink_color = p.accent;
    style.visuals.code_bg_color = p.components().code_inline;
    style.visuals.extreme_bg_color = p.components().code_block;
    style.visuals.widgets.noninteractive.fg_stroke.color = p.label_primary;
    style.spacing.item_spacing.y = body * 0.32;
    style.spacing.interact_size.y = body * 1.35;
    style.spacing.icon_width = body;
    style.spacing.icon_width_inner = body * 0.65;
    style.url_in_tooltip = true;
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);
}

fn draw_math(ui: &mut Ui, latex: &str, size: f32, display: bool, color: egui::Color32) {
    let Some(measured) = math::measure(latex, size, display) else {
        ui.label(
            RichText::new(if display {
                format!("$${latex}$$")
            } else {
                format!("${latex}$")
            })
            .code(),
        )
        .on_hover_text("公式尚未完整或语法不受支持，保留原文");
        return;
    };
    // Formula widgets reserve their actual width AND height. Oversized formulas
    // get a local horizontal viewport instead of covering following text.
    if measured.x > ui.available_width() {
        egui::ScrollArea::horizontal()
            .max_width(ui.available_width())
            .auto_shrink([false, true])
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                let _ = math::render(ui, color, latex, size, display);
            });
    } else {
        let _ = math::render(ui, color, latex, size, display);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Rect, Vec2};
    use neo_theme::{Distance, Theme, ThemeMode};

    pub const SAMPLE: &str = "## 目录与文件清单\n\n| 项目 | 类型 | 大小 | 说明 |\n| :--- | :---: | ---: | :--- |\n| `LCC_cleaned/` | 文件夹 | 空 | 里面没有任何文件 |\n| 屏幕截图 2026-07-28 132019.png | 图片 | 1.1 MB | YOLO 饮料检测结果截图 |\n| 屏幕截图 2026-07-28 132034.png | 图片 | 175 KB | 训练过程日志截图 |\n| 演示脚本.txt | 文本 | 683 B | 项目演示的台词脚本 |\n\n## 三张截图的内容\n\n1. **检测结果**：`best.pt` 检测两瓶饮料。\n2. **训练日志**：20 轮训练，10 个类别。\n   - 保留嵌套列表的层级\n   - 子项里的 **强调** 与 `代码`\n3. **验证结果**：Precision 0.773，Recall 0.809。\n\n### 公式与代码\n\n行内公式 $E=mc^2$ 与后面的文字应当分开。\n\n$$\\frac{-b\\pm\\sqrt{b^2-4ac}}{2a}$$\n\n```rust\nfn main() {\n    println!(\"Hello, Neo\");\n}\n```\n\n- [x] 文档解析已接入\n- [ ] 继续核对资料\n\n> 引用支持 **格式** 与 [网页链接](https://example.com)。\n\n脚注示例[^1]。\n\n[^1]: 这是脚注内容。";

    fn frame(
        ctx: &egui::Context,
        text: &str,
        width: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        frame_themed(
            ctx,
            text,
            width,
            events,
            Theme::new(ThemeMode::Light, 1080.0, Distance::Standard),
        )
    }

    fn frame_themed(
        ctx: &egui::Context,
        text: &str,
        width: f32,
        events: Vec<egui::Event>,
        theme: Theme,
    ) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(width, 2400.0),
            )),
            ..Default::default()
        };
        input.events = events;
        let mut output = ctx.run_ui(input, |ui| {
            let whale = crate::brand::WhaleMark::load(ui.ctx());
            let skin = Skin::new(theme, &whale);
            render(ui, &skin, text, false);
        });
        output.textures_delta.clear();
        output
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        ctx.set_theme(egui::Theme::Light);
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        ctx
    }

    fn text_shapes(output: &egui::FullOutput) -> Vec<(String, Rect, Color32)> {
        fn visit(shape: &egui::Shape, result: &mut Vec<(String, Rect, Color32)>) {
            match shape {
                egui::Shape::Text(t) => result.push((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.fallback_color,
                )),
                egui::Shape::Vec(shapes) => {
                    for s in shapes {
                        visit(s, result);
                    }
                }
                _ => {}
            }
        }
        let mut result = Vec::new();
        for shape in &output.shapes {
            visit(&shape.shape, &mut result);
        }
        result
    }

    #[test]
    fn commonmark_table_is_two_dimensional_and_heading_is_separate() {
        let ctx = context();
        let text =
            "| First | Second |\n|---|---|\n| alpha | beta |\n| gamma | delta |\n\n## After table";
        let _ = frame(&ctx, text, 640.0, vec![]);
        let out = frame(&ctx, text, 640.0, vec![]);
        let labels = text_shapes(&out);
        let rect = |s: &str| {
            labels
                .iter()
                .find(|(t, _, _)| t == s)
                .unwrap_or_else(|| panic!("missing {s}: {labels:?}"))
                .1
        };
        assert!(rect("beta").left() > rect("alpha").right());
        assert!((rect("beta").top() - rect("alpha").top()).abs() < 1.0);
        assert!(rect("gamma").top() > rect("alpha").bottom());
        assert!(rect("After table").top() > rect("delta").bottom());
        assert!(labels.iter().all(|(t, _, _)| !t.contains(" | ")));
    }

    #[test]
    fn commonmark_nested_lists_numbers_styles_and_streaming_update() {
        let ctx = context();
        let initial = "9. nine\n10. ten\n    - nested\n      - deep\n11. eleven\n\n**bold** *italic* ~~deleted~~ `code`";
        let out = frame(&ctx, initial, 420.0, vec![]);
        let labels = text_shapes(&out);
        for expected in [
            "9.", "10.", "11.", "nine", "ten", "nested", "deep", "eleven", "bold", "italic",
            "deleted", "code",
        ] {
            assert!(
                labels.iter().any(|(t, _, _)| t == expected),
                "missing {expected}: {labels:?}"
            );
        }
        let out = frame(&ctx, "### Updated\n\nNew streamed text", 420.0, vec![]);
        let labels = text_shapes(&out);
        assert!(labels.iter().any(|(t, _, _)| t == "Updated"));
        assert!(!labels.iter().any(|(t, _, _)| t == "eleven"));
    }

    #[test]
    fn commonmark_link_click_opens_web_not_local_or_script() {
        let ctx = context();
        for (text, should_open) in [
            ("[Open](https://example.com)", true),
            ("[Open](file:///C:/secret.txt)", false),
            ("[Open](javascript:alert%281%29)", false),
        ] {
            let _ = frame(&ctx, text, 500.0, vec![]);
            let out = frame(&ctx, text, 500.0, vec![]);
            let pos = text_shapes(&out)
                .iter()
                .find(|(t, _, _)| t == "Open")
                .unwrap()
                .1
                .center();
            let _ = frame(
                &ctx,
                text,
                500.0,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            let out = frame(
                &ctx,
                text,
                500.0,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            assert_eq!(
                out.platform_output
                    .commands
                    .iter()
                    .any(|c| matches!(c, egui::OutputCommand::OpenUrl(_))),
                should_open
            );
        }
    }

    #[test]
    fn commonmark_math_reserves_height_and_preserves_invalid_source() {
        let ctx = context();
        let out = frame(
            &ctx,
            "Before\n\n$$\\frac{1}{2}$$\n\nAfter\n\n$\\notARealCommand{x}$",
            640.0,
            vec![],
        );
        let labels = text_shapes(&out);
        let before = labels.iter().find(|(t, _, _)| t == "Before").unwrap().1;
        let after = labels.iter().find(|(t, _, _)| t == "After").unwrap().1;
        assert!(after.top() > before.bottom() + 24.0);
        assert!(labels.iter().any(|(t, _, _)| t.contains("notARealCommand")));
        assert!(
            labels.iter().all(|(t, _, _)| t != "Unaligned"),
            "{labels:?}"
        );
    }

    #[test]
    fn commonmark_table_alignment_and_wrapping_are_not_justified() {
        let ctx = context();
        let text = "| L | C | R |\n|:---|:---:|---:|\n| left | center | right |\n| longer label | longer center | longer right |";
        let _ = frame(&ctx, text, 680.0, vec![]);
        let out = frame(&ctx, text, 680.0, vec![]);
        let labels = text_shapes(&out);
        let rect = |s: &str| labels.iter().find(|(t, _, _)| t == s).unwrap().1;
        assert!((rect("left").left() - rect("longer label").left()).abs() < 1.0);
        assert!((rect("center").center().x - rect("longer center").center().x).abs() < 1.0);
        assert!((rect("right").right() - rect("longer right").right()).abs() < 1.0);
        assert!(labels.iter().all(|(t, _, _)| t != "Unaligned"));
    }

    fn table_stripes(output: &egui::FullOutput) -> Vec<Rect> {
        fn visit(shape: &egui::Shape, result: &mut Vec<Rect>) {
            match shape {
                egui::Shape::Rect(r)
                    if r.rect.width() > 200.0
                        && r.rect.height() > 4.0
                        && r.corner_radius.nw == 2
                        && r.stroke.width == 0.0 =>
                {
                    result.push(r.rect);
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        visit(shape, result);
                    }
                }
                _ => {}
            }
        }
        let mut rows = Vec::new();
        for shape in &output.shapes {
            visit(&shape.shape, &mut rows);
        }
        rows
    }

    #[test]
    fn commonmark_table_content_is_vertically_centered_in_striped_rows() {
        let ctx = context();
        let long = "Long content that wraps across several lines inside a narrow table cell";
        let text = format!(
            "| Left | Center | Right |\n|:---|:---:|---:|\n| short | {long} | `code` |\n| gap | gap | gap |\n| [Linked](https://example.com) | {long} | tail |\n| gap | gap | gap |\n| single | middle | end |\n\nAfter table"
        );
        // Reuse one context while resizing: remembered cell heights must also shrink.
        for (width, mode, distance) in [
            (540.0, ThemeMode::Light, Distance::Standard),
            (980.0, ThemeMode::Dark, Distance::Classroom),
            (1500.0, ThemeMode::Dark, Distance::Auditorium),
            (540.0, ThemeMode::Light, Distance::Standard),
        ] {
            ctx.set_theme(if mode == ThemeMode::Dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            let theme = Theme::new(mode, 1080.0, distance);
            for _ in 0..3 {
                let _ = frame_themed(&ctx, &text, width, vec![], theme);
            }
            let out = frame_themed(&ctx, &text, width, vec![], theme);
            let labels = text_shapes(&out);
            let rows = table_stripes(&out);
            assert_eq!(rows.len(), 3, "striped rows: {rows:?}");
            for (row, cells) in rows.iter().zip([
                ["short", long, "code"],
                ["Linked", long, "tail"],
                ["single", "middle", "end"],
            ]) {
                for cell in cells {
                    let rect = labels
                        .iter()
                        .filter(|(text, _, _)| text == cell)
                        .map(|(_, rect, _)| *rect)
                        .min_by(|a, b| {
                            (a.center().y - row.center().y)
                                .abs()
                                .total_cmp(&(b.center().y - row.center().y).abs())
                        })
                        .unwrap_or_else(|| panic!("missing {cell}: {labels:?}"));
                    assert!(
                        (rect.center().y - row.center().y).abs() <= 1.0,
                        "{cell} not vertically centered at width {width}: text={rect:?}, row={row:?}"
                    );
                    assert!(
                        row.contains_rect(rect),
                        "text overflows row: {rect:?} {row:?}"
                    );
                }
            }
            assert!(labels.iter().all(|(text, _, _)| text != "Unaligned"));
        }
    }

    #[test]
    fn commonmark_table_complex_cell_height_shrinks_and_link_hitbox_follows() {
        let ctx = context();
        let marker = Color32::from_rgb(17, 193, 97);
        let source = "| Formula | Link |\n|---|---|\n| $x$ | [Open](https://example.com) |";
        let render_frame = |height: f32, events: Vec<egui::Event>| {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(640.0, 500.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let whale = crate::brand::WhaleMark::load(ui.ctx());
                    configure(
                        ui,
                        &Skin::new(
                            Theme::new(ThemeMode::Light, 1080.0, Distance::Standard),
                            &whale,
                        ),
                    );
                    let math = move |ui: &mut Ui, _: &str, _: bool| {
                        // Known-size stand-in checks the arbitrary callback path, not font metrics.
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(24.0, height), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 0.0, marker);
                    };
                    CommonMarkViewer::new().render_math_fn(Some(&math)).show(
                        ui,
                        &mut CommonMarkCache::default(),
                        source,
                    );
                },
            );
            out.textures_delta.clear();
            out
        };
        for height in [64.0, 18.0, 96.0, 18.0] {
            for _ in 0..3 {
                let _ = render_frame(height, vec![]);
            }
            let out = render_frame(height, vec![]);
            let row = table_stripes(&out)[0];
            let formula = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == marker => Some(r.rect),
                    _ => None,
                })
                .unwrap();
            let link = text_shapes(&out)
                .into_iter()
                .find(|(text, _, _)| text == "Open")
                .unwrap()
                .1;
            for rect in [formula, link] {
                assert!(
                    (rect.center().y - row.center().y).abs() <= 1.0,
                    "cell={rect:?} row={row:?}"
                );
                assert!(row.contains_rect(rect));
            }
            let pos = link.center();
            let _ = render_frame(
                height,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            let clicked = render_frame(
                height,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                }],
            );
            assert!(clicked
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(command,
                    egui::OutputCommand::OpenUrl(url) if url.url == "https://example.com"
                )));
        }
    }

    #[test]
    fn commonmark_six_headings_escaped_pipe_and_reference_links() {
        let ctx = context();
        let text = "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\n\n| A | B |\n|---|---|\n| a\\|b | [Reference][target] |\n\n[target]: https://example.com\n\n    indented code\n";
        let out = frame(&ctx, text, 680.0, vec![]);
        let labels = text_shapes(&out);
        for expected in [
            "One",
            "Two",
            "Three",
            "Four",
            "Five",
            "Six",
            "a|b",
            "Reference",
            "indented code",
        ] {
            assert!(
                labels.iter().any(|(t, _, _)| t.contains(expected)),
                "missing {expected}: {labels:?}"
            );
        }
        let heights: Vec<_> = ["One", "Two", "Three", "Four", "Five", "Six"]
            .iter()
            .map(|s| labels.iter().find(|(t, _, _)| t == s).unwrap().1.height())
            .collect();
        assert!(heights.windows(2).all(|pair| pair[0] >= pair[1]));
        assert!(heights[0] > heights[5]);
    }

    #[test]
    fn commonmark_code_copy_uses_actual_clipboard_output() {
        let ctx = context();
        let text = "```rust\nlet answer = 42;\n```";
        let _ = frame(&ctx, text, 640.0, vec![]);
        let out = frame(&ctx, text, 640.0, vec![]);
        let pos = text_shapes(&out)
            .iter()
            .find(|(t, _, _)| t == "\u{1f5d0}")
            .unwrap()
            .1
            .center();
        let _ = frame(
            &ctx,
            text,
            640.0,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        let out = frame(
            &ctx,
            text,
            640.0,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert!(out
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "let answer = 42;")));
    }

    #[test]
    fn commonmark_incomplete_stream_syntax_never_panics_or_leaks_cache() {
        let ctx = context();
        for text in [
            "**incomplete",
            "```rust\nlet a =",
            "$\\frac{",
            "| A | B |\n|---|---|\n|partial",
            "[link](https://exa",
            "> 1. nested\n>    - unfinished",
        ] {
            let out = frame(&ctx, text, 260.0, vec![]);
            assert!(!text_shapes(&out).is_empty());
        }
    }

    #[test]
    fn commonmark_quote_keeps_nested_list_table_and_following_block() {
        let ctx = context();
        let text = "> Outer\n>\n> > Nested\n>\n> 1. First\n> 2. Second\n>\n> | A | B |\n> |---|---|\n> | alpha | beta |\n\n## Following";
        let _ = frame(&ctx, text, 640.0, vec![]);
        let out = frame(&ctx, text, 640.0, vec![]);
        let labels = text_shapes(&out);
        for expected in [
            "Outer",
            "Nested",
            "First",
            "Second",
            "alpha",
            "beta",
            "Following",
        ] {
            assert!(
                labels.iter().any(|(t, _, _)| t == expected),
                "missing {expected}: {labels:?}"
            );
        }
        let rect = |s: &str| labels.iter().find(|(t, _, _)| t == s).unwrap().1;
        assert!(rect("beta").left() > rect("alpha").right());
        assert!(rect("Following").top() > rect("beta").bottom());
    }

    #[test]
    fn commonmark_snapshot_matrix() {
        for (name, width, mode, distance) in [
            (
                "22-commonmark-dark",
                1200.0,
                ThemeMode::Dark,
                Distance::Classroom,
            ),
            (
                "23-commonmark-light-narrow",
                520.0,
                ThemeMode::Light,
                Distance::Standard,
            ),
            (
                "24-commonmark-large",
                1920.0,
                ThemeMode::Dark,
                Distance::Auditorium,
            ),
        ] {
            let mut initialized = false;
            let theme = Theme::new(mode, 1080.0, distance);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(Vec2::new(width, 1700.0))
                .wgpu()
                .build_ui(|ui| {
                    if !initialized {
                        neo_theme::fonts::install(ui.ctx());
                        ui.ctx().set_theme(if mode == ThemeMode::Dark {
                            egui::Theme::Dark
                        } else {
                            egui::Theme::Light
                        });
                        initialized = true;
                        return;
                    }
                    ui.painter()
                        .rect_filled(ui.ctx().content_rect(), 0.0, theme.palette.bg_base);
                    egui::Frame::NONE.inner_margin(24.0).show(ui, |ui| {
                        let whale = crate::brand::WhaleMark::load(ui.ctx());
                        render(ui, &Skin::new(theme, &whale), SAMPLE, false);
                        assert!(
                            ui.min_rect().right() <= width - 12.0,
                            "Markdown expanded beyond viewport: {:?}",
                            ui.min_rect()
                        );
                    });
                });
            harness.run_steps(4);
            let image = harness.render().unwrap();
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens");
            image.save(dir.join(format!("{name}.png"))).unwrap();
        }
    }

    #[test]
    fn commonmark_images_have_explicit_non_loading_fallback() {
        let ctx = context();
        let out = frame(
            &ctx,
            "![private](file:///C:/private.png) ![remote](https://example.com/image.png)",
            640.0,
            vec![],
        );
        let labels = text_shapes(&out);
        assert!(labels.iter().any(|(t, _, _)| t.contains("private")));
        assert!(labels.iter().any(|(t, _, _)| t.contains("remote")));
        assert!(out.platform_output.commands.is_empty());
    }
}
