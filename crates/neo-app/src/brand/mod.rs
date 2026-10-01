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
#[derive(Clone)]
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

    /// 每上下文只光栅化一次：确认卡 / 小窗这类逐帧绘制处用缓存版，
    /// 每帧 `load` 重光栅化 512px 是纯浪费。
    ///
    /// 注意两段式（先查再插）：`load` 内部会 `ctx.input(...)` 读统一锁，
    /// 放进 `data_mut` 的写锁闭包里就是同线程自死锁。
    pub fn cached(ctx: &Context) -> Self {
        let id = egui::Id::new("neo-whale-mark");
        if let Some(mark) = ctx.data(|d| d.get_temp::<Self>(id)) {
            return mark;
        }
        let mark = Self::load(ctx);
        ctx.data_mut(|d| d.insert_temp(id, mark.clone()));
        mark
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
#[path = "tests.rs"]
mod tests;
