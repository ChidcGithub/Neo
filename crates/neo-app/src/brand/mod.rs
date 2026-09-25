//! 鲸鱼标志。
//!
//! Harness 的 hero 标志是一个 SVG 路径（上游 `FishLogo.tsx`）。egui 不能直接填充
//! 凹多边形路径，所以这里做两件事：
//!
//! 1. 把 SVG 的 `M/C/L/Z` 路径展平成闭合折线；
//! 2. 按**奇偶填充规则**做扫描线光栅化，生成一张白色带 alpha 的纹理，
//!    绘制时用 `tint` 上色——这样一份纹理可以适配任意主题色。
//!
//! 用奇偶规则是有意为之：鲸鱼的「眼睛」与「鳍」在上游是**挖空**的，
//! 它们是嵌在身体轮廓内部的独立子路径，奇偶规则天然把它们变成洞。
//!
//! 解析与光栅化本身已经抽到 [`neo_theme::svg`] —— 图标（`neo-ui::icons`）
//! 与标志共用同一份实现，两边不会各自演化。

pub mod whale_path;

use egui::{Color32, Context, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use neo_theme::svg;

use whale_path::{FISH_LOGO_PATH, VIEWBOX_H, VIEWBOX_W};

/// 光栅化纹理的宽度（像素）。高度按 viewBox 比例推导。
///
/// 512 足以覆盖 4K + 高 DPI 下 hero 标志的物理像素（34pt × 2.8 × 3 ≈ 286px）。
const TEXTURE_W: usize = 512;

/// 垂直超采样倍数（水平方向的抗锯齿由扫描线的小数端点天然给出）。
const SUPERSAMPLE: usize = 4;

/// 鲸鱼标志：预光栅化的纹理 + 宽高比。
pub struct WhaleMark {
    texture: TextureHandle,
    aspect: f32,
}

impl WhaleMark {
    /// 解析路径、光栅化并上传纹理。
    pub fn load(ctx: &Context) -> Self {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        let aspect = VIEWBOX_W / VIEWBOX_H;
        let w = TEXTURE_W;
        let h = ((w as f32) / aspect).round() as usize;

        // 用奇偶规则（与上游等价，这里另有测试钉住这一点；见文件头的说明）。
        let coverage = svg::rasterize(
            &subpaths,
            (VIEWBOX_W, VIEWBOX_H),
            w,
            h,
            SUPERSAMPLE,
            svg::FillRule::EvenOdd,
        );
        let pixels = coverage
            .iter()
            .map(|c| {
                let a = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                Color32::from_white_alpha(a)
            })
            .collect::<Vec<_>>();

        let image = egui::ColorImage::new([w, h], pixels);
        let texture = ctx.load_texture("neo-whale", image, TextureOptions::LINEAR);

        Self { texture, aspect }
    }

    /// 标志的宽高比（宽 / 高）。
    pub fn aspect(&self) -> f32 {
        self.aspect
    }

    /// 在 `rect` 内以 `tint` 绘制标志。
    ///
    /// `rect` 的宽高比会被忽略，始终按 viewBox 比例内接居中，避免拉伸变形。
    pub fn paint(&self, painter: &Painter, rect: Rect, tint: Color32) {
        let target = fit_aspect(rect, self.aspect);
        painter.image(
            self.texture.id(),
            target,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            tint,
        );
    }
}

/// 在 `outer` 内按 `aspect` 内接一个居中的矩形。
fn fit_aspect(outer: Rect, aspect: f32) -> Rect {
    let w = outer.width();
    let h = w / aspect;
    if h <= outer.height() {
        Rect::from_center_size(outer.center(), Vec2::new(w, h))
    } else {
        let h = outer.height();
        Rect::from_center_size(outer.center(), Vec2::new(h * aspect, h))
    }
}

/// 把鲸鱼光栅化成方形 RGBA 位图（系统托盘图标与窗口图标共用）。
///
/// 返回 `(rgba, size, size)`。鲸鱼按 viewBox 比例内接居中，四周留白；
/// 颜色用品牌蓝——托盘背景深浅不定，纯色比白色更稳。
pub fn whale_rgba(size: usize) -> (Vec<u8>, u32, u32) {
    let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
    let aspect = VIEWBOX_W / VIEWBOX_H;
    // 内接进方形画布，短边方向居中留白。
    let w = size;
    let h = ((size as f32) / aspect).round() as usize;
    let coverage = svg::rasterize(
        &subpaths,
        (VIEWBOX_W, VIEWBOX_H),
        w,
        h,
        SUPERSAMPLE,
        svg::FillRule::EvenOdd,
    );
    let mut rgba = vec![0u8; size * size * 4];
    let y0 = (size - h.min(size)) / 2;
    for y in 0..h.min(size) {
        for x in 0..w.min(size) {
            let a = (coverage[y * w + x].clamp(0.0, 1.0) * 255.0).round() as u8;
            let px = ((y0 + y) * size + x) * 4;
            rgba[px] = 0x4D; // 品牌蓝 #4D6BFE
            rgba[px + 1] = 0x6B;
            rgba[px + 2] = 0xFE;
            rgba[px + 3] = a;
        }
    }
    (rgba, size as u32, size as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_four_subpaths() {
        // 上游路径由 4 条子路径组成：身体 + 鳍挖空 + 眼睛 + 鳍/嘴挖空。
        // 数量由 `d` 字符串里的 M/Z 对决定，与 SVG 源一一对应。
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        assert_eq!(subpaths.len(), 4, "子路径数量与上游不符");
        for sp in &subpaths {
            assert!(sp.len() > 3, "子路径点数过少: {}", sp.len());
        }
    }

    #[test]
    fn all_points_inside_viewbox() {
        for sp in svg::parse_subpaths(FISH_LOGO_PATH) {
            for p in sp {
                assert!(p[0] >= -1.0 && p[0] <= VIEWBOX_W + 1.0, "x 越界: {}", p[0]);
                assert!(p[1] >= -1.0 && p[1] <= VIEWBOX_H + 1.0, "y 越界: {}", p[1]);
            }
        }
    }

    #[test]
    fn eye_and_fin_are_cut_out() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        let (w, h, ss) = (256usize, 188usize, 4usize);
        let cov = svg::rasterize(
            &subpaths,
            (VIEWBOX_W, VIEWBOX_H),
            w,
            h,
            ss,
            svg::FillRule::EvenOdd,
        );
        let at = |x: f32, y: f32| -> f32 {
            let px = ((x / VIEWBOX_W) * w as f32) as usize;
            let py = ((y / VIEWBOX_H) * h as f32) as usize;
            cov[py.min(h - 1) * w + px.min(w - 1)]
        };
        // 身体实心区（鲸鱼左侧下腹）。
        assert!(at(2.0, 10.0) > 0.9, "身体区域未被填充: {}", at(2.0, 10.0));
        assert!(at(10.0, 8.0) > 0.9, "身体区域未被填充: {}", at(10.0, 8.0));
        // 眼睛与鳍在上游是挖空 —— 必须透出背景。
        assert!(at(12.44, 8.26) < 0.2, "眼睛未成为镂空: {}", at(12.44, 8.26));
        assert!(at(14.0, 8.5) < 0.2, "鳍未成为镂空: {}", at(14.0, 8.5));
    }

    /// 上游 SVG 用默认的非零填充；这里用奇偶规则实现。
    /// 两者只有在子路径互相重叠时才会分歧，因此需要一条测试把「当前几何下等价」钉住 ——
    /// 一旦上游改了鲸鱼路径（比如子路径开始重叠），这条会先炸。
    #[test]
    fn even_odd_matches_nonzero_on_this_geometry() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);

        let even_odd = |x: f32, y: f32| -> bool {
            let mut crossings = Vec::new();
            for sp in &subpaths {
                for i in 0..sp.len() {
                    let a = sp[i];
                    let b = sp[(i + 1) % sp.len()];
                    if (a[1] <= y) != (b[1] <= y) {
                        let t = (y - a[1]) / (b[1] - a[1]);
                        crossings.push(a[0] + t * (b[0] - a[0]));
                    }
                }
            }
            crossings.sort_by(|p, q| p.partial_cmp(q).unwrap());
            crossings.chunks_exact(2).any(|c| x > c[0] && x < c[1])
        };

        let nonzero = |x: f32, y: f32| -> bool {
            let mut winding = 0i32;
            for sp in &subpaths {
                for i in 0..sp.len() {
                    let a = sp[i];
                    let b = sp[(i + 1) % sp.len()];
                    let crosses_up = a[1] <= y && b[1] > y;
                    let crosses_down = b[1] <= y && a[1] > y;
                    if !crosses_up && !crosses_down {
                        continue;
                    }
                    let t = (y - a[1]) / (b[1] - a[1]);
                    let x_at = a[0] + t * (b[0] - a[0]);
                    if x_at > x {
                        winding += if crosses_up { 1 } else { -1 };
                    }
                }
            }
            winding != 0
        };

        for iy in 0..40 {
            for ix in 0..54 {
                let x = (ix as f32 + 0.5) / 54.0 * VIEWBOX_W;
                let y = (iy as f32 + 0.5) / 40.0 * VIEWBOX_H;
                assert_eq!(
                    even_odd(x, y),
                    nonzero(x, y),
                    "填充规则在 ({x:.3}, {y:.3}) 处出现分歧 —— 光栅化需要改用非零规则"
                );
            }
        }
    }

    #[test]
    fn coverage_is_binary_inside_and_outside() {
        let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
        let (w, h) = (128usize, 94usize);
        let cov = svg::rasterize(
            &subpaths,
            (VIEWBOX_W, VIEWBOX_H),
            w,
            h,
            2,
            svg::FillRule::EvenOdd,
        );
        let filled = cov.iter().filter(|c| **c > 0.9).count();
        assert!(filled > 0, "没有任何像素被填充");
        // 角落必须为空（鲸鱼轮廓不触及 viewBox 右上角）。
        assert!(cov[0] < 0.1, "左上角不应被填充");
        assert!(cov[w - 1] < 0.1, "右上角不应被填充");
    }
}
