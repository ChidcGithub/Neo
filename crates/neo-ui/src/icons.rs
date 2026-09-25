//! 图标集。
//!
//! ## 几何直接取自上游
//!
//! 22 个图标的路径数据来自 `dsh-client-ui-primitives`（见 [`paths`]，
//! 自动生成、与上游逐字节一致），与 `brand/whale_path.rs` 同一套做法。
//!
//! 这一点很关键：上游图标是 **16/14 栅格上的填充路径**（`viewBox="0 0 16 16"`，
//! `fill: currentColor`），**不是一笔描边**：看起来是线稿，其实由两条反向
//! 绕行的轮廓（外轮廓 + 内轮廓）填出来。所以在这里手绘"差不多的线条"永远
//! 对不上——粗细、端点、圆角都会差一截。
//!
//! ## 渲染方式
//!
//! 路径 → 白色 + alpha 的纹理（懒生成、每图标一份、按需上传），绘制时用
//! `tint` 上色。好处：形状由 `neo_theme::svg` 用扫描线精确填充（支持挖空），
//! 抗锯齿在纹理里一次性算好，每帧只是一个贴图四边形 —— 比每帧三角化便宜得多。
//!
//! 只有 [`Icon::Mic`] 仍是手绘描边：上游图标集里没有麦克风，它是 Neo 场景卡
//! 自己的图形。
//!
//! 所有图标统一映射到目标矩形内接居中（保持 viewBox 比例），
//! 因此同一个图标在 16pt 的行内动作和 28pt 的场景卡上比例完全一致。

use egui::{Color32, Id, Painter, Pos2, Rect, Shape, Stroke, TextureHandle, TextureOptions, Vec2};

pub mod paths;

/// 设计栅格边长（仅手绘图标使用；上游图标按各自 viewBox 映射）。
const GRID: f32 = 24.0;

/// 组件库认得的图标。
///
/// 枚举而不是散函数：调用方在按钮/输入框上只写 `Icon::Board`，
/// 不需要记住具体的自由函数签名，也方便日后加"选中态/填充态"变体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Plus,
    ArrowUp,
    Stop,
    Folder,
    ChevronDown,
    Sun,
    Moon,
    Close,
    Cog,
    Trash,
    // 场景
    Board,
    Checklist,
    Pen,
    Mic,
    // 通用
    Check,
    Info,
    Warn,
    Sparkle,
    Copy,
    Search,
    ArrowLeft,
    ArrowRight,
    Dots,
}

/// 全部图标（顺序即枚举顺序），供遍历与测试使用。
pub const ALL: &[Icon] = &[
    Icon::Plus,
    Icon::ArrowUp,
    Icon::Stop,
    Icon::Folder,
    Icon::ChevronDown,
    Icon::Sun,
    Icon::Moon,
    Icon::Close,
    Icon::Cog,
    Icon::Trash,
    Icon::Board,
    Icon::Checklist,
    Icon::Pen,
    Icon::Mic,
    Icon::Check,
    Icon::Info,
    Icon::Warn,
    Icon::Sparkle,
    Icon::Copy,
    Icon::Search,
    Icon::ArrowLeft,
    Icon::ArrowRight,
    Icon::Dots,
];

impl Icon {
    /// 上游字形。`None` = 上游没有这个图形，由 Neo 手绘。
    pub fn glyph(self) -> Option<&'static paths::Glyph> {
        use paths as g;
        Some(match self {
            Icon::Plus => &g::PLUS,
            Icon::ArrowUp => &g::ARROW_UP,
            Icon::Stop => &g::STOP,
            Icon::Folder => &g::FOLDER,
            Icon::ChevronDown => &g::CHEVRON_DOWN,
            Icon::Sun => &g::SUN,
            Icon::Moon => &g::MOON,
            Icon::Close => &g::CLOSE,
            Icon::Cog => &g::COG,
            Icon::Trash => &g::TRASH,
            Icon::Board => &g::BOARD,
            Icon::Checklist => &g::CHECKLIST,
            Icon::Pen => &g::PEN,
            Icon::Check => &g::CHECK,
            Icon::Info => &g::INFO,
            Icon::Warn => &g::WARN,
            Icon::Sparkle => &g::SPARKLE,
            Icon::Copy => &g::COPY,
            Icon::Search => &g::SEARCH,
            Icon::ArrowLeft => &g::ARROW_LEFT,
            Icon::ArrowRight => &g::ARROW_RIGHT,
            Icon::Dots => &g::DOTS,
            // 上游图标集里没有麦克风 —— 这是 Neo 场景卡自己的图形。
            Icon::Mic => return None,
        })
    }

    /// 在 `rect` 内绘制。`stroke_w` 以设计单位计，**只对手绘图标有效**
    /// （目前只有 `Mic`）—— 上游图标的粗细写在路径里，不随参数变化。
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32, stroke_w: f32) {
        if let Some(glyph) = self.glyph() {
            paint_glyph(painter, rect, color, glyph);
            return;
        }
        if self == Icon::Mic {
            mic(painter, rect, color, stroke_w);
        }
    }
}

/// 用预光栅化的纹理绘制一个上游字形。
fn paint_glyph(painter: &Painter, rect: Rect, color: Color32, glyph: &'static paths::Glyph) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || color.a() == 0 {
        return;
    }
    // 上游把 viewBox 铺满一个 `size × size` 的方框；这里同样**内接居中**，
    // 非正方形 viewBox（如 8×10）也不会被拉伸。
    let scale = (rect.width() / glyph.vw).min(rect.height() / glyph.vh);
    let target =
        Rect::from_center_size(rect.center(), Vec2::new(glyph.vw * scale, glyph.vh * scale));
    let texture = glyph_texture(painter, glyph);
    painter.image(
        texture.id(),
        target,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        color,
    );
}

/// 取（必要时生成）图标的白色 alpha 纹理。
///
/// 存在 egui 的临时数据里：每个 `Context` 一份，测试与真机互不影响，
/// 也不需要全局静态（纹理的生命周期本就跟着 Context 走）。
/// 键用字形自身的地址 —— 它在静态表里，全进程唯一且稳定。
fn glyph_texture(painter: &Painter, glyph: &'static paths::Glyph) -> TextureHandle {
    let ctx = painter.ctx();
    let id = Id::new(("neo-icon-glyph", glyph as *const paths::Glyph as usize));
    if let Some(tex) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return tex;
    }
    let w = paths::TEXTURE_PX;
    let h = ((w as f32 / glyph.aspect()).round() as usize).max(1);
    let image = egui::ColorImage::new([w, h], glyph.pixels().to_vec());
    let tex = ctx.load_texture(format!("neo-icon-{w}x{h}"), image, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
    tex
}

// ---------------------------------------------------------------------------
// 手绘图标（上游没有对应图形）
// ---------------------------------------------------------------------------

/// 映射后的长度：设计单位 → 像素。
fn u(rect: Rect, v: f32) -> f32 {
    rect.width().min(rect.height()) / GRID * v
}

/// 把设计坐标映射到目标矩形（保持中心对齐与等比缩放）。
fn p(rect: Rect, x: f32, y: f32) -> Pos2 {
    let s = rect.width().min(rect.height()) / GRID;
    let c = rect.center();
    Pos2::new(c.x + (x - GRID * 0.5) * s, c.y + (y - GRID * 0.5) * s)
}

fn stroke(rect: Rect, design_w: f32, color: Color32) -> Stroke {
    Stroke::new(u(rect, design_w).max(0.75), color)
}

fn seg(painter: &Painter, rect: Rect, a: (f32, f32), b: (f32, f32), color: Color32, w: f32) {
    painter.add(Shape::line(
        vec![p(rect, a.0, a.1), p(rect, b.0, b.1)],
        stroke(rect, w, color),
    ));
}

/// 麦克风：胶囊 + 弧底 + 支杆（Neo 自有）。
fn mic(painter: &Painter, rect: Rect, color: Color32, w: f32) {
    for x in [9.0f32, 15.0f32] {
        seg(painter, rect, (x, 8.4), (x, 12.4), color, w);
    }
    let top: Vec<Pos2> = (0..=14)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 14.0) + std::f32::consts::PI;
            p(rect, 12.0 + a.cos() * 3.0, 8.4 + a.sin() * 3.0)
        })
        .collect();
    painter.add(Shape::line(top, stroke(rect, w, color)));
    let bot: Vec<Pos2> = (0..=14)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 14.0);
            p(rect, 12.0 + a.cos() * 3.0, 12.4 + a.sin() * 3.0)
        })
        .collect();
    painter.add(Shape::line(bot, stroke(rect, w, color)));
    seg(painter, rect, (12.0, 15.4), (12.0, 18.2), color, w);
    seg(painter, rect, (8.6, 18.4), (15.4, 18.4), color, w);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 覆盖率采样：按 viewBox 归一化坐标取一个像素的覆盖率。
    fn coverage(glyph: &paths::Glyph, x: f32, y: f32) -> f32 {
        let pixels = glyph.pixels();
        let w = paths::TEXTURE_PX;
        let h = ((w as f32 / glyph.aspect()).round() as usize).max(1);
        let px = ((x / glyph.vw) * w as f32).clamp(0.0, (w - 1) as f32) as usize;
        let py = ((y / glyph.vh) * h as f32).clamp(0.0, (h - 1) as f32) as usize;
        pixels[py * w + px].a() as f32 / 255.0
    }

    /// 把光栅化结果打成字符画 —— 校验字形几何的"眼睛"。
    ///
    /// 默认 `#[ignore]`；需要看的时候：
    /// `cargo test -p neo-ui dump_glyphs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dump_glyphs() {
        for icon in [Icon::Moon, Icon::Plus, Icon::Folder, Icon::Sun, Icon::Cog] {
            let g = icon.glyph().unwrap();
            let px = g.pixels();
            let w = paths::TEXTURE_PX;
            let h = ((w as f32 / g.aspect()).round() as usize).max(1);
            println!("=== {icon:?} ({w}×{h}) ===");
            for row in 0..32 {
                let mut line = String::new();
                for col in 0..32 {
                    let x = col * w / 32;
                    let y = row * h / 32;
                    let a = px[y * w + x].a();
                    line.push(if a > 200 {
                        '#'
                    } else if a > 60 {
                        '+'
                    } else {
                        '.'
                    });
                }
                println!("{line}");
            }
        }
    }

    #[test]
    fn every_icon_maps_to_a_glyph_except_the_hand_drawn_one() {
        for &icon in ALL {
            assert_eq!(
                icon.glyph().is_some(),
                icon != Icon::Mic,
                "{icon:?} 的字形映射与预期不符"
            );
        }
    }

    #[test]
    fn no_glyph_rasterizes_empty() {
        for &icon in ALL {
            let Some(g) = icon.glyph() else { continue };
            let ink = g.pixels().iter().filter(|c| c.a() > 8).count();
            assert!(ink > 20, "{icon:?} 光栅化后几乎没墨：{ink} 个像素");
        }
    }

    /// 上游月亮是**双轮廓挖空**的填充路径 —— 这条测试守住"填充规则别退化成实心"。
    /// 手绘时代最容易错的正是这里（填实了就变成一坨饼）。
    #[test]
    fn moon_has_a_cut_out_and_no_solid_blob() {
        let moon = Icon::Moon.glyph().unwrap();
        // 外环左缘有墨（采样点按 32×32 字符画定过，避开线条边缘）
        assert!(
            coverage(moon, 2.0, 8.0) > 0.8,
            "月亮外环没被填: {}",
            coverage(moon, 2.0, 8.0)
        );
        // 圆环以内必须是空的（这就是上游的挖空）
        assert!(
            coverage(moon, 5.0, 8.0) < 0.1,
            "月亮内侧没有挖空: {}",
            coverage(moon, 5.0, 8.0)
        );
    }

    /// 光栅化墨迹的包围盒（viewBox 单位）。
    fn ink_bbox(glyph: &paths::Glyph) -> [f32; 4] {
        let px = glyph.pixels();
        let w = paths::TEXTURE_PX;
        let h = ((w as f32 / glyph.aspect()).round() as usize).max(1);
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for y in 0..h {
            for x in 0..w {
                if px[y * w + x].a() > 8 {
                    let vx = x as f32 / w as f32 * glyph.vw;
                    let vy = y as f32 / h as f32 * glyph.vh;
                    x0 = x0.min(vx);
                    y0 = y0.min(vy);
                    x1 = x1.max(vx);
                    y1 = y1.max(vy);
                }
            }
        }
        [x0, y0, x1, y1]
    }

    /// **核心不变式**：光栅化出来的墨迹范围必须与路径自身的几何包围盒一致。
    ///
    /// 一次性守住一串容易错的地方：坐标翻转、viewBox 映射比例、漏掉的 `transform`
    /// （上游有 5 个图标用 `translate` 把图案摆进 viewBox）、以及上限裁切。
    /// 比手挑采样点可靠得多 —— 采到线条边缘就会得到 0.4 这种模棱两可的值。
    #[test]
    fn rasterized_ink_matches_path_bounds() {
        use neo_theme::svg;
        for &icon in ALL {
            let Some(g) = icon.glyph() else { continue };
            let mut contours = Vec::new();
            for d in g.d {
                contours.extend(svg::parse_subpaths(d));
            }
            let want = svg::bounds(&contours).expect("路径非空");
            let got = ink_bbox(g);
            for (i, axis) in ["x0", "y0", "x1", "y1"].iter().enumerate() {
                assert!(
                    (got[i] - want[i]).abs() < 0.35,
                    "{icon:?} 的 {axis} 对不上：光栅化 {:.3}，路径 {:.3}",
                    got[i],
                    want[i]
                );
            }
        }
    }

    #[test]
    fn plus_is_ink_in_the_middle_and_empty_at_corners() {
        let g = Icon::Plus.glyph().unwrap();
        assert!(coverage(g, 8.0, 8.0) > 0.9, "加号中心应为实心");
        assert!(coverage(g, 1.0, 1.0) < 0.1, "加号左上角应为空");
    }
}

#[cfg(test)]
mod cache_key_probe {
    use super::paths;
    use egui::Id;

    /// 探测：两个不同字形会不会拿到同一个缓存键。
    ///
    /// 缓存键是「字形静态量的地址」。如果链接器把两个 `Glyph` 合并了
    /// （identical-code-folding 之类），地址就会撞上，界面里会出现
    /// "月亮的位置画出加号"这种荒谬现象。
    #[test]
    fn glyph_addresses_are_distinct() {
        let a = &paths::PLUS as *const paths::Glyph as usize;
        let b = &paths::MOON as *const paths::Glyph as usize;
        let c = &paths::SUN as *const paths::Glyph as usize;
        assert_ne!(a, b, "PLUS 与 MOON 的地址相同 —— 缓存键会撞");
        assert_ne!(a, c, "PLUS 与 SUN 的地址相同 —— 缓存键会撞");
        assert_ne!(b, c, "MOON 与 SUN 的地址相同 —— 缓存键会撞");

        assert_ne!(
            Id::new(("neo-icon-glyph", a)),
            Id::new(("neo-icon-glyph", b)),
            "Id 哈希撞了"
        );
    }

    /// 探测：两个字形光栅化出来的像素必须不同（否则是渲染管线拿错了数据）。
    #[test]
    fn glyph_pixels_differ_between_icons() {
        assert_ne!(
            paths::PLUS.pixels(),
            paths::MOON.pixels(),
            "PLUS 与 MOON 的像素完全相同"
        );
    }
}
