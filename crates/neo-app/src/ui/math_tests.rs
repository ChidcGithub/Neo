
    use super::*;

    fn glyphs(dl: &DisplayList) -> Vec<(u32, f64, f64, f64)> {
        dl.items
            .iter()
            .filter_map(|i| match i {
                DisplayItem::GlyphPath {
                    char_code,
                    x,
                    y,
                    scale,
                    ..
                } => Some((*char_code, *x, *y, *scale)),
                _ => None,
            })
            .collect()
    }

    fn assert_same_layout(actual: &DisplayList, expected: &DisplayList) {
        assert_eq!(actual.width, expected.width);
        assert_eq!(actual.height, expected.height);
        assert_eq!(actual.depth, expected.depth);
        assert_eq!(actual.items, expected.items);
    }

    #[test]
    fn cached_layout_reuses_parse_and_matches_uncached_results() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DisplayList>();
        assert_send_sync::<Arc<DisplayList>>();
        reset_layout_cache();
        for latex in [
            r"\frac{a}{b}",
            r"\sum_{i=1}^n i",
            r"\sqrt{x} + \widehat{ab}",
            r"\color{red}{x} + \colorbox{blue}{y}",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        ] {
            for display in [false, true] {
                let expected = layout(latex, display).unwrap();
                let before = parse_calls();
                let first = cached_layout(latex, display).unwrap();
                assert_eq!(parse_calls(), before + 1);
                assert_same_layout(&first, &expected);
                for _ in 0..4 {
                    let hit = cached_layout(latex, display).unwrap();
                    assert!(Arc::ptr_eq(&first, &hit));
                    assert_same_layout(&hit, &expected);
                }
                assert_eq!(parse_calls(), before + 1);
            }
        }
    }

    #[test]
    fn cache_keys_include_full_source_and_display_but_not_size() {
        reset_layout_cache();
        let latex = r"\sum_{i=1}^n i";
        let first = cached_layout(latex, false).unwrap();
        assert_eq!(
            measure(latex, 12.0, false).unwrap() * 2.0,
            measure(latex, 24.0, false).unwrap()
        );
        assert_eq!(parse_calls(), 1);
        let display = cached_layout(latex, true).unwrap();
        assert!(display.total_height() > first.total_height());
        assert!(!Arc::ptr_eq(&first, &display));
        let changed = cached_layout(r"\sum_{i=1}^n j", false).unwrap();
        assert_ne!(first.items, changed.items);
        let whitespace = cached_layout(&format!("{latex} "), false).unwrap();
        assert!(!Arc::ptr_eq(&first, &whitespace));
        assert_eq!(parse_calls(), 4);
        assert!(Arc::ptr_eq(&first, &cached_layout(latex, false).unwrap()));
        assert_eq!(parse_calls(), 4);
    }

    #[test]
    fn invalid_latex_is_cached_without_poisoning_streamed_corrections() {
        reset_layout_cache();
        for _ in 0..5 {
            assert!(cached_layout(r"\frac{a}", false).is_none());
            assert!(measure(r"\frac{a}", 20.0, false).is_none());
        }
        assert_eq!(parse_calls(), 1);
        assert!(cached_layout(r"\frac{a}", true).is_none());
        assert_eq!(parse_calls(), 2);
        assert!(cached_layout(r"\frac{a}{b}", false).is_some());
        assert_eq!(parse_calls(), 3);
    }

    #[test]
    fn cache_capacity_is_lru_and_evicted_arcs_remain_valid() {
        reset_layout_cache();
        let first = cached_layout("x_0", false).unwrap();
        let evicted = cached_layout("x_1", false).unwrap();
        for i in 2..MAX_CACHE_ENTRIES {
            cached_layout(&format!("x_{i}"), false).unwrap();
        }
        assert_eq!(parse_calls(), MAX_CACHE_ENTRIES);
        assert!(Arc::ptr_eq(&first, &cached_layout("x_0", false).unwrap()));
        cached_layout("y", false).unwrap();
        assert_eq!(parse_calls(), MAX_CACHE_ENTRIES + 1);
        assert!(Arc::ptr_eq(&first, &cached_layout("x_0", false).unwrap()));
        let reloaded = cached_layout("x_1", false).unwrap();
        assert!(!Arc::ptr_eq(&evicted, &reloaded));
        assert_same_layout(&evicted, &reloaded);
        assert_eq!(parse_calls(), MAX_CACHE_ENTRIES + 2);
        LAYOUT_CACHE.with(|cache| {
            let cache = cache.borrow();
            assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
            assert!(cache.bytes <= MAX_CACHE_BYTES);
        });
    }

    #[test]
    fn source_limit_bypasses_retention_without_rejecting_layout() {
        reset_layout_cache();
        let at_limit = format!("x{}", " ".repeat(MAX_CACHE_SOURCE_BYTES - 1));
        let first = cached_layout(&at_limit, false).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &cached_layout(&at_limit, false).unwrap()
        ));
        assert_eq!(parse_calls(), 1);
        let over_limit = format!("{at_limit} ");
        let a = cached_layout(&over_limit, false).unwrap();
        let b = cached_layout(&over_limit, false).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_same_layout(&a, &b);
        assert_eq!(parse_calls(), 3);
        let invalid = format!("\\unknownmacro{}", " ".repeat(MAX_CACHE_SOURCE_BYTES));
        assert!(cached_layout(&invalid, false).is_none());
        assert!(cached_layout(&invalid, false).is_none());
        assert_eq!(parse_calls(), 5);
        LAYOUT_CACHE.with(|cache| assert_eq!(cache.borrow().entries.len(), 1));
    }

    #[test]
    fn complex_output_bypasses_retention_without_rejecting_layout() {
        reset_layout_cache();
        let latex = "x".repeat(4096);
        assert!(latex.len() <= MAX_CACHE_SOURCE_BYTES);
        let a = cached_layout(&latex, false).unwrap();
        assert!(display_list_bytes(&a) > MAX_CACHE_ENTRY_BYTES);
        let b = cached_layout(&latex, false).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_same_layout(&a, &b);
        assert_eq!(parse_calls(), 2);
        LAYOUT_CACHE.with(|cache| assert!(cache.borrow().entries.is_empty()));
    }

    #[test]
    fn output_budget_counts_spare_capacity_fonts_and_path_commands() {
        let mut dl = DisplayList::new();
        let empty_bytes = display_list_bytes(&dl);
        dl.items.reserve(7);
        assert_eq!(
            display_list_bytes(&dl),
            empty_bytes + dl.items.capacity() * size_of::<DisplayItem>()
        );
        let mut font = String::with_capacity(4096);
        font.push_str("Main-Regular");
        let font_bytes = font.capacity();
        dl.items.push(DisplayItem::GlyphPath {
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            font,
            char_code: 120,
            color: MathColor::BLACK,
        });
        let before_path = display_list_bytes(&dl);
        assert_eq!(
            before_path,
            empty_bytes + dl.items.capacity() * size_of::<DisplayItem>() + font_bytes
        );
        let commands = Vec::with_capacity(MAX_CACHE_ENTRY_BYTES / size_of::<PathCommand>());
        let path_bytes = commands.capacity() * size_of::<PathCommand>();
        dl.items.push(DisplayItem::Path {
            x: 0.0,
            y: 0.0,
            commands,
            fill: true,
            color: MathColor::BLACK,
        });
        assert_eq!(display_list_bytes(&dl), before_path + path_bytes);
        let mut cache = LayoutCache::default();
        cache.insert("x", false, Some(Arc::new(dl)));
        assert!(cache.entries.is_empty());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn total_byte_budget_and_failure_entry_limit_are_enforced() {
        let mut cache = LayoutCache::default();
        for i in 0..MAX_CACHE_ENTRIES {
            let mut dl = DisplayList::new();
            dl.items
                .reserve(MAX_CACHE_ENTRY_BYTES / 2 / size_of::<DisplayItem>());
            cache.insert(&format!("x_{i}"), false, Some(Arc::new(dl)));
            assert!(cache.bytes <= MAX_CACHE_BYTES);
            assert_eq!(
                cache.bytes,
                cache.entries.iter().map(|entry| entry.bytes).sum::<usize>()
            );
        }
        assert!(cache.entries.len() < MAX_CACHE_ENTRIES);
        assert!(cache.entries.len() > 1);
        assert_eq!(
            cache.entries.back().unwrap().latex.as_ref(),
            format!("x_{}", MAX_CACHE_ENTRIES - 1)
        );
        for i in 0..MAX_CACHE_ENTRIES * 2 {
            cache.insert(&format!("\\unknown{i}"), false, None);
            assert!(cache.entries.len() <= MAX_CACHE_ENTRIES);
            assert!(cache.bytes <= MAX_CACHE_BYTES);
        }
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        assert!(cache.entries.iter().all(|entry| entry.result.is_none()));
    }

    #[test]
    fn measure_and_render_keep_size_validation_and_share_parse_offscreen() {
        reset_layout_cache();
        let ctx = egui::Context::default();
        neo_theme::fonts::install(&ctx);
        let latex = r"\frac{a}{b}";
        for size in [12.0_f32, 24.0] {
            let measured = measure(latex, size, false).unwrap();
            let mut output = ctx.run_ui(Default::default(), |ui| {
                for color in [Color32::RED, Color32::BLUE] {
                    let rect = render(ui, color, latex, size, false).unwrap();
                    assert_eq!(rect.size(), Vec2::new(measured.x.ceil(), measured.y.ceil()));
                }
                assert!(render(ui, Color32::WHITE, "", size, false).is_none());
                assert!(render(ui, Color32::WHITE, r"\frac{a}", size, false).is_none());
            });
            output.textures_delta.clear();
        }
        assert_eq!(parse_calls(), 3);
        for size in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert!(measure(latex, size, false).is_some());
            let mut output = ctx.run_ui(Default::default(), |ui| {
                assert!(render(ui, Color32::WHITE, latex, size, false).is_none());
            });
            output.textures_delta.clear();
        }
        assert_eq!(parse_calls(), 3);
    }

    #[test]
    fn lays_out_a_fraction() {
        let dl = layout(r"\frac{a}{b}", false).expect("应能排版");
        let lines: Vec<&DisplayItem> = dl
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Line { .. }))
            .collect();
        assert_eq!(lines.len(), 1, "一个分数只有一条分数线");
        let g = glyphs(&dl);
        assert_eq!(g.len(), 2, "分子分母各一个字");
        let ly = match lines[0] {
            DisplayItem::Line { y, .. } => *y,
            _ => unreachable!(),
        };
        assert!(g.iter().any(|(_, _, y, _)| *y < ly), "分子应在分数线之上");
        assert!(g.iter().any(|(_, _, y, _)| *y > ly), "分母应在分数线之下");
        assert!(dl.width > 0.0 && dl.total_height() > 0.0);
    }

    #[test]
    fn superscript_is_smaller() {
        let dl = layout(r"x^2 + \alpha", false).expect("应能排版");
        let g = glyphs(&dl);
        assert!(
            g.iter().any(|(_, _, _, s)| *s < 0.95),
            "上标没有缩小：{g:?}"
        );
        assert!(
            g.iter().any(|(_, _, _, s)| (*s - 1.0).abs() < 1e-6),
            "正文未按 1.0 排"
        );
    }

    #[test]
    fn mathbf_uses_a_bold_face() {
        let dl = layout(r"\mathbf{A}", false).unwrap();
        let fonts: Vec<String> = dl
            .items
            .iter()
            .filter_map(|i| match i {
                DisplayItem::GlyphPath { font, .. } => Some(font.clone()),
                _ => None,
            })
            .collect();
        assert!(
            fonts.iter().any(|f| f.contains("Bold")),
            "\\mathbf 没切到粗体面：{fonts:?}"
        );
    }

    #[test]
    fn display_style_is_taller_than_text_style() {
        let text = layout(r"\sum_{i=1}^n i", false).unwrap();
        let disp = layout(r"\sum_{i=1}^n i", true).unwrap();
        assert!(
            disp.total_height() > text.total_height(),
            "行间公式应更高：{:.3} vs {:.3}",
            disp.total_height(),
            text.total_height()
        );
    }

    /// **关键回归**：排版器给出的族名必须全都内嵌了 ——
    /// 缺一个就会静默退回正文字体（字形错、还难查）。
    #[test]
    fn every_font_face_asked_for_is_embedded() {
        for tex in [
            r"\frac{a}{b}",
            r"\mathbf{x} \mathit{y} \mathcal{L} \mathrm{z}",
            r"\sum_{i=1}^{n} \int_0^1 \sqrt{2} \left( \frac{1}{2} \right)",
            r"\alpha\beta\gamma\Delta\Omega \pm \times \div \leq \geq \neq",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            r"\widehat{ab} \overrightarrow{AB} \underbrace{x+y}",
        ] {
            let dl = layout(tex, true).unwrap_or_else(|| panic!("排版失败：{tex}"));
            for item in &dl.items {
                if let DisplayItem::GlyphPath { font, .. } = item {
                    assert!(
                        neo_theme::fonts::has_katex_face(font),
                        "`{tex}` 用到未内嵌的字体面 `{font}`"
                    );
                }
            }
        }
    }

    /// 语法错误必须**安静地失败**（返回 `None`），不能 panic、更不能崩界面。
    ///
    /// 空输入是另一回事：它能排版，只是尺寸为 0 —— 由 `render` 判为「不可画」。
    #[test]
    fn bad_latex_fails_quietly() {
        for bad in [
            r"\frac{a}",
            r"\unknownmacro{x}",
            r"\left(",
            r"\begin{pmatrix} a",
        ] {
            assert!(layout(bad, false).is_none(), "`{bad}` 应当排版失败");
        }
        if let Some(dl) = layout("", false) {
            assert!(dl.width <= 0.0 || dl.items.is_empty(), "空公式不该排出内容");
        }
    }

    #[test]
    fn path_commands_flatten_by_absolute_origin() {
        let cmds = vec![
            PathCommand::MoveTo { x: 0.0, y: 0.0 },
            PathCommand::LineTo { x: 1.0, y: 0.0 },
            PathCommand::LineTo { x: 1.0, y: 1.0 },
            PathCommand::Close,
        ];
        let out = flatten(&cmds, 2.0, 3.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0], [2.0, 3.0]);
        assert_eq!(out[0][2], [3.0, 4.0]);
    }
