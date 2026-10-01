
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
