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

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::mem::size_of;
use std::sync::Arc;

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
#[cfg(test)]
pub fn render(
    ui: &mut egui::Ui,
    color: Color32,
    latex: &str,
    size: f32,
    display: bool,
) -> Option<Rect> {
    let dl = cached_layout(latex, display)?;
    render_layout(ui, color, &dl, size)
}

/// 绘制已排版的公式，与 `render` 使用相同的尺寸校验和取整规则。
pub fn render_layout(
    ui: &mut egui::Ui,
    color: Color32,
    dl: &DisplayList,
    size: f32,
) -> Option<Rect> {
    let Vec2 { x: w, y: h } = measure_layout(dl, size);
    if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w.ceil(), h.ceil()), egui::Sense::hover());
    paint(ui.painter(), dl, rect, size, color);
    // 返回占用的矩形。基线是 `rect.top() + height × size` ——
    // 需要行内对齐时用 `layout()` 自己拿显示列表算，不必在这里多存一份。
    Some(rect)
}

/// 只要尺寸、不绘制（判断某个公式放不放得下时用）。
#[cfg(test)]
pub fn measure(latex: &str, size: f32, display: bool) -> Option<Vec2> {
    let dl = cached_layout(latex, display)?;
    Some(measure_layout(&dl, size))
}

/// em 显示列表不依赖字号；保留 `measure` 的非取整、非校验语义。
pub fn measure_layout(dl: &DisplayList, size: f32) -> Vec2 {
    Vec2::new((dl.width as f32) * size, (dl.total_height() as f32) * size)
}

const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CACHE_SOURCE_BYTES: usize = 8 * 1024;
const MAX_CACHE_ENTRY_BYTES: usize = 256 * 1024;
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

struct LayoutEntry {
    latex: Box<str>,
    display: bool,
    result: Option<Arc<DisplayList>>,
    bytes: usize,
}

#[derive(Default)]
struct LayoutCache {
    // 最旧的在前。最多 128 项，直接比较完整 key，避免哈希碰撞及命中时分配。
    entries: VecDeque<LayoutEntry>,
    bytes: usize,
}

impl LayoutCache {
    fn get_or_layout(&mut self, latex: &str, display: bool) -> Option<Arc<DisplayList>> {
        if latex.len() <= MAX_CACHE_SOURCE_BYTES {
            if let Some(index) = self
                .entries
                .iter()
                .position(|entry| entry.display == display && entry.latex.as_ref() == latex)
            {
                let entry = self.entries.remove(index).unwrap();
                let result = entry.result.clone();
                self.entries.push_back(entry);
                return result;
            }
        }

        let result = layout(latex, display).map(Arc::new);
        self.insert(latex, display, result.clone());
        result
    }

    fn insert(&mut self, latex: &str, display: bool, result: Option<Arc<DisplayList>>) {
        if latex.len() > MAX_CACHE_SOURCE_BYTES {
            return;
        }
        let bytes = size_of::<LayoutEntry>()
            .saturating_add(latex.len())
            .saturating_add(result.as_deref().map_or(0, display_list_bytes));
        if bytes > MAX_CACHE_ENTRY_BYTES {
            return;
        }
        while self.entries.len() >= MAX_CACHE_ENTRIES || self.bytes + bytes > MAX_CACHE_BYTES {
            let oldest = self.entries.pop_front().unwrap();
            self.bytes -= oldest.bytes;
        }
        self.entries.push_back(LayoutEntry {
            latex: latex.into(),
            display,
            result,
            bytes,
        });
        self.bytes += bytes;
    }
}

// 计入 Vec/String 的 capacity（不是 len），路径命令也可能远多于显示项。
// 容器槽位另由 entry 上限约束；预算只约束缓存持有量，不限制临时排版内存。
fn display_list_bytes(dl: &DisplayList) -> usize {
    let base = size_of::<DisplayList>() + 2 * size_of::<usize>(); // Arc 计数
    dl.items.iter().fold(
        base.saturating_add(dl.items.capacity().saturating_mul(size_of::<DisplayItem>())),
        |bytes, item| {
            bytes.saturating_add(match item {
                DisplayItem::GlyphPath { font, .. } => font.capacity(),
                DisplayItem::Path { commands, .. } => {
                    commands.capacity().saturating_mul(size_of::<PathCommand>())
                }
                DisplayItem::Line { .. } | DisplayItem::Rect { .. } => 0,
            })
        },
    )
}

thread_local! {
    // measure 没有 Context 参数。每个 UI 线程共享有限缓存，线程退出即释放；
    // 不向 egui data 塞入可能不满足 Send/Sync 的排版器内部状态。
    static LAYOUT_CACHE: RefCell<LayoutCache> = RefCell::new(LayoutCache::default());
    #[cfg(test)]
    static PARSE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// 共享 em 排版结果（含语法失败）。字号、颜色、视口宽度均不属于 key。
/// 超长源码或复杂输出仍正常返回，但不驻留缓存。调用方持有 Arc 可安全跨越淘汰。
pub fn cached_layout(latex: &str, display: bool) -> Option<Arc<DisplayList>> {
    LAYOUT_CACHE.with(|cache| cache.borrow_mut().get_or_layout(latex, display))
}

#[cfg(test)]
pub(super) fn reset_layout_cache() {
    LAYOUT_CACHE.with(|cache| *cache.borrow_mut() = LayoutCache::default());
    PARSE_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(super) fn parse_calls() -> usize {
    PARSE_CALLS.with(|calls| calls.get())
}

/// 无缓存排版成独立显示列表。保留测试与显式重新排版的接口。
pub fn layout(latex: &str, display: bool) -> Option<DisplayList> {
    #[cfg(test)]
    PARSE_CALLS.with(|calls| calls.set(calls.get() + 1));
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
                    painter,
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
///
/// 画在调用方的 `painter` 上（继承滚动区/单元格的裁剪与图层）——
/// 曾经用 `debug_painter()`：那是 Debug 层、裁剪是整个视口，公式滚出
/// 视野后括号残影还漂在顶栏与输入卡之上。纹理缓存才需要 `Context`。
#[allow(clippy::too_many_arguments)]
fn paint_path(
    painter: &egui::Painter,
    ox: f64,
    oy: f64,
    commands: &[PathCommand],
    fill: bool,
    color: MathColor,
    rect: Rect,
    size: f32,
    text_color: Color32,
) {
    let ctx = painter.ctx();
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
                painter.add(egui::Shape::line(pts, stroke));
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
    painter.image(
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
#[path = "math_tests.rs"]
mod tests;
