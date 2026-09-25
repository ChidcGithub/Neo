//! LaTeX 公式渲染。
//!
//! ## 分工
//!
//! **排版交给库**：[`ratex`] 是 KaTeX 的纯 Rust 移植，而上游 Harness 渲染公式
//! 用的正是 KaTeX —— 度量、间距、伸缩括号的取整规则天然一致。
//! 我们只负责把它的显示列表画到 egui 上，不自己算任何位置。
//!
//! 显示列表的坐标是 **em 单位**（y 向下、原点在包围盒左上角、基线在 `y = height`），
//! 乘上字号即为点。四类指令都有对应画法：
//!
//! | 指令 | 含义 | 画法 |
//! |---|---|---|
//! | `GlyphPath` | 字形（族名 + 码点 + 缩放） | 按**基线**定位画一个字 |
//! | `Line` | 分数线 / 根号线 / 上下划线 | 填充矩形（支持虚线） |
//! | `Rect` | `\colorbox` 底色 | 填充矩形 |
//! | `Path` | 伸缩括号、`\widehat` 等 | `fill` 走带洞光栅化，否则描边 |
//!
//! 字体由 `neo-theme` 内嵌的 20 个 KaTeX 字体面提供，族名与 `ratex` 的
//! `FontId::as_str()` 一一对应（`Main-Regular`、`Size2-Regular` …）。
//!
//! ## 两个实现要点
//!
//! 1. **按基线定位**：egui 的 `Align2` 没有基线档，所以先用一次
//!    `layout_no_wrap` 量出「行顶 → 基线」的距离，再以 `LEFT_TOP` 落笔时把它减掉。
//!    同一族同一字号只量一次（显示列表里通常就两三种）。
//! 2. **`Path` 走纹理**：伸缩括号之类的轮廓每帧重算既慢又浪费，按
//!    「指令序列 + 像素尺寸」缓存成白色 alpha 纹理，绘制时 `tint` 上色 ——
//!    与图标同一条路子。

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use egui::{
    Align2, Color32, FontId, Id, Painter, Pos2, Rect, Stroke, TextureHandle, TextureOptions, Vec2,
};
use ratex_layout::{to_display_list, LayoutOptions};
use ratex_parser::parser::parse;
use ratex_types::color::Color as MathColor;
use ratex_types::display_item::{DisplayItem, DisplayList};
use ratex_types::path_command::PathCommand;

/// 按给定字号排版并绘制。返回 `None` 表示 **LaTeX 有语法错误**（不画、不弹窗）。
///
/// `display` 为真走行间公式（`$$…$$`）：大运算符上下限、分数更舒展。
pub fn render(
    ui: &mut egui::Ui,
    color: Color32,
    latex: &str,
    size: f32,
    display: bool,
) -> Option<Rect> {
    let dl = layout(latex, display)?;
    let w = (dl.width as f32) * size;
    let h = (dl.total_height() as f32) * size;
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w.ceil(), h.ceil()), egui::Sense::hover());
    paint(ui.painter(), &dl, rect, size, color);
    // 返回占用的矩形。基线是 `rect.top() + height × size` ——
    // 需要行内对齐时用 `layout()` 自己拿显示列表算，不必在这里多存一份。
    Some(rect)
}

/// 只要尺寸、不绘制（判断某个公式放不放得下时用）。
pub fn measure(latex: &str, size: f32, display: bool) -> Option<Vec2> {
    let dl = layout(latex, display)?;
    Some(Vec2::new(
        (dl.width as f32) * size,
        (dl.total_height() as f32) * size,
    ))
}

/// 排版成显示列表。测试与「排一次画多次」用。
pub fn layout(latex: &str, display: bool) -> Option<DisplayList> {
    let nodes = parse(latex).ok()?;
    let opts = LayoutOptions {
        style: if display {
            ratex_types::math_style::MathStyle::Display
        } else {
            ratex_types::math_style::MathStyle::Text
        },
        ..LayoutOptions::default()
    };
    Some(to_display_list(&ratex_layout::layout(&nodes, &opts)))
}

/// 在既定矩形里画一段已排版好的公式。
pub fn paint(painter: &Painter, dl: &DisplayList, rect: Rect, size: f32, text_color: Color32) {
    let ox = rect.left();
    let oy = rect.top();
    let at = |x: f64, y: f64| Pos2::new(ox + x as f32 * size, oy + y as f32 * size);

    // 每族每字号只量一次「行顶 → 基线」。
    let mut baseline: HashMap<(String, u32), f32> = HashMap::new();

    for item in &dl.items {
        match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                color,
            } => {
                let Some(ch) = char::from_u32(*char_code) else {
                    continue;
                };
                let px = size * (*scale as f32);
                if px <= 0.5 {
                    continue;
                }
                let font_id = FontId::new(px, neo_theme::fonts::katex_family(font));
                let color = tint(*color, text_color);
                let key = (font.clone(), px.to_bits());
                let off = *baseline
                    .entry(key)
                    .or_insert_with(|| baseline_offset(painter, &font_id, text_color));
                painter.text(
                    Pos2::new(at(*x, *y).x, at(*x, *y).y - off),
                    Align2::LEFT_TOP,
                    ch,
                    font_id,
                    color,
                );
            }
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                color,
                dashed,
            } => {
                let c = tint(*color, text_color);
                let t = ((*thickness as f32) * size).max(0.6);
                let top = at(*x, *y).y;
                if *dashed {
                    // 虚线：3 倍线宽为一段，实空交替（`\hdashline`）。
                    let seg = t * 3.0;
                    let mut cx = *x as f32 * size;
                    let span = *width as f32 * size;
                    while cx < span {
                        let end = (cx + seg).min(span);
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(ox + cx, top),
                                Vec2::new((end - cx).max(0.5), t),
                            ),
                            0.0,
                            c,
                        );
                        cx += seg * 2.0;
                    }
                } else {
                    painter.rect_filled(
                        Rect::from_min_size(
                            Pos2::new(ox + *x as f32 * size, top),
                            Vec2::new((*width as f32 * size).max(0.5), t),
                        ),
                        0.0,
                        c,
                    );
                }
            }
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => {
                painter.rect_filled(
                    Rect::from_min_max(at(*x, *y), at(*x + *width, *y + *height)),
                    0.0,
                    tint(*color, text_color),
                );
            }
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                color,
            } => {
                paint_path(
                    painter.ctx(),
                    *x,
                    *y,
                    commands,
                    *fill,
                    *color,
                    rect,
                    size,
                    text_color,
                );
            }
        }
    }
}

/// 「行顶 → 基线」的距离。同一族同一字号对所有字符都相同（epaint 保证）。
fn baseline_offset(painter: &Painter, font: &FontId, color: Color32) -> f32 {
    let galley = painter.layout_no_wrap("x".to_owned(), font.clone(), color);
    match galley.rows.first() {
        Some(row) => row.pos.y + row.row.glyphs.first().map(|g| g.pos.y).unwrap_or(0.0),
        None => 0.0,
    }
}

/// `Path` 指令：填充走带洞光栅化（缓存成纹理），描边走折线。
#[allow(clippy::too_many_arguments)]
fn paint_path(
    ctx: &egui::Context,
    ox: f64,
    oy: f64,
    commands: &[PathCommand],
    fill: bool,
    color: MathColor,
    rect: Rect,
    size: f32,
    text_color: Color32,
) {
    let contours = flatten(commands, ox, oy);
    if contours.is_empty() {
        return;
    }
    let c = tint(color, text_color);

    if !fill {
        let stroke = Stroke::new((size * 0.04).max(0.6), c);
        for sp in &contours {
            let pts: Vec<Pos2> = sp
                .iter()
                .map(|p| Pos2::new(rect.left() + p[0] * size, rect.top() + p[1] * size))
                .collect();
            if pts.len() > 1 {
                ctx.debug_painter().add(egui::Shape::line(pts, stroke));
            }
        }
        return;
    }

    let Some(bounds) = neo_theme::svg::bounds(&contours) else {
        return;
    };
    let vw = (bounds[2] - bounds[0]).max(1e-3);
    let vh = (bounds[3] - bounds[1]).max(1e-3);
    let px_w = ((vw * size).ceil() as usize).clamp(1, 512);
    let px_h = ((vh * size).ceil() as usize).clamp(1, 512);

    let mut hasher = DefaultHasher::new();
    for cmd in commands {
        format!("{cmd:?}").hash(&mut hasher);
    }
    px_w.hash(&mut hasher);
    px_h.hash(&mut hasher);
    let id = Id::new(("neo-math-path", hasher.finish()));

    let tex = match ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        Some(t) => t,
        None => {
            let local: Vec<Vec<[f32; 2]>> = contours
                .iter()
                .map(|sp| {
                    sp.iter()
                        .map(|p| [p[0] - bounds[0], p[1] - bounds[1]])
                        .collect()
                })
                .collect();
            let cov = neo_theme::svg::rasterize(
                &local,
                (vw, vh),
                px_w,
                px_h,
                3,
                neo_theme::svg::FillRule::NonZero,
            );
            let pixels: Vec<Color32> = cov
                .iter()
                .map(|a| Color32::from_white_alpha((a.clamp(0.0, 1.0) * 255.0) as u8))
                .collect();
            let t = ctx.load_texture(
                "neo-math-path",
                egui::ColorImage::new([px_w, px_h], pixels),
                TextureOptions::LINEAR,
            );
            ctx.data_mut(|d| d.insert_temp(id, t.clone()));
            t
        }
    };

    let target = Rect::from_min_size(
        Pos2::new(
            rect.left() + bounds[0] * size,
            rect.top() + bounds[1] * size,
        ),
        Vec2::new(vw * size, vh * size),
    );
    ctx.debug_painter().image(
        tex.id(),
        target,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        c,
    );
}

/// 把 `ratex` 的颜色映射到主题。
///
/// 公式里的纯黑/纯白一律当作「跟随正文色」—— 公式应当随主题走，
/// 只有 `\color{red}` 这种真彩色才照实映射。
fn tint(c: MathColor, fallback: Color32) -> Color32 {
    let is_black = c.r < 0.01 && c.g < 0.01 && c.b < 0.01;
    let is_white = c.r > 0.99 && c.g > 0.99 && c.b > 0.99;
    if is_black || is_white {
        return fallback;
    }
    Color32::from_rgba_unmultiplied(
        (c.r.clamp(0.0, 1.0) * 255.0) as u8,
        (c.g.clamp(0.0, 1.0) * 255.0) as u8,
        (c.b.clamp(0.0, 1.0) * 255.0) as u8,
        (c.a.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// 展平 `PathCommand` 序列（相对坐标 → 以 `(ox, oy)` 为原点的 em 坐标）。
fn flatten(commands: &[PathCommand], ox: f64, oy: f64) -> Vec<Vec<[f32; 2]>> {
    const SEG: usize = 12;
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut last = [ox as f32, oy as f32];
    let mut start = last;

    for cmd in commands {
        match cmd {
            PathCommand::MoveTo { x, y } => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
                last = [(ox + x) as f32, (oy + y) as f32];
                start = last;
                cur.push(last);
            }
            PathCommand::LineTo { x, y } => {
                last = [(ox + x) as f32, (oy + y) as f32];
                cur.push(last);
            }
            PathCommand::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let (c1, c2, p) = (
                    [(ox + x1) as f32, (oy + y1) as f32],
                    [(ox + x2) as f32, (oy + y2) as f32],
                    [(ox + x) as f32, (oy + y) as f32],
                );
                for i in 1..=SEG {
                    let t = i as f32 / SEG as f32;
                    let u = 1.0 - t;
                    cur.push([
                        u * u * u * last[0]
                            + 3.0 * u * u * t * c1[0]
                            + 3.0 * u * t * t * c2[0]
                            + t * t * t * p[0],
                        u * u * u * last[1]
                            + 3.0 * u * u * t * c1[1]
                            + 3.0 * u * t * t * c2[1]
                            + t * t * t * p[1],
                    ]);
                }
                last = p;
            }
            PathCommand::QuadTo { x1, y1, x, y } => {
                let c = [(ox + x1) as f32, (oy + y1) as f32];
                let p = [(ox + x) as f32, (oy + y) as f32];
                for i in 1..=SEG {
                    let t = i as f32 / SEG as f32;
                    let u = 1.0 - t;
                    cur.push([
                        u * u * last[0] + 2.0 * u * t * c[0] + t * t * p[0],
                        u * u * last[1] + 2.0 * u * t * c[1] + t * t * p[1],
                    ]);
                }
                last = p;
            }
            PathCommand::Close => {
                if cur.len() > 1 {
                    out.push(std::mem::take(&mut cur));
                }
                last = start;
            }
        }
    }
    if cur.len() > 1 {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
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
}
