//! Brand marks and system icon bitmaps.
//!
//! The in-app brand mark is the Neo cat mascot. It ships as an embedded PNG
//! pair (black for light surfaces, white for dark surfaces); the theme's
//! `dark_mode` flag selects the texture at paint time. System icons (tray,
//! window, installer) use separately cropped square exports.

pub mod whale_path;

use egui::{Color32, Context, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use neo_theme::svg;

use whale_path::{FISH_LOGO_PATH, VIEWBOX_H, VIEWBOX_W};

const SUPERSAMPLE: usize = 4;

/// Main-window mascot textures, trimmed to their content and capped at 1024px.
const MASCOT_BLACK_PNG: &[u8] = include_bytes!("../../../../resources/icons/mascot_black.png");
const MASCOT_WHITE_PNG: &[u8] = include_bytes!("../../../../resources/icons/mascot_white.png");
/// Tray mark: plain `neo` wordmark in a square canvas.
const TRAY_BLACK_PNG: &[u8] = include_bytes!("../../../../resources/icons/tray_black.png");
const TRAY_WHITE_PNG: &[u8] = include_bytes!("../../../../resources/icons/tray_white.png");
/// Window mark: `neo` + spark in a square canvas.
const WINDOW_BLACK_PNG: &[u8] = include_bytes!("../../../../resources/icons/window_black.png");
const WINDOW_WHITE_PNG: &[u8] = include_bytes!("../../../../resources/icons/window_white.png");
/// Large application icon: full cat + wordmark lockup.
const APP_BLACK_PNG: &[u8] = include_bytes!("../../../../resources/icons/app_black.png");
const APP_WHITE_PNG: &[u8] = include_bytes!("../../../../resources/icons/app_white.png");

/// The in-app brand mark: mascot textures + aspect ratio.
#[derive(Clone)]
pub struct MascotMark {
    black: TextureHandle,
    white: TextureHandle,
    aspect: f32,
}

impl MascotMark {
    /// Decode and upload both mascot variants.
    pub fn load(ctx: &Context) -> Self {
        let (black, aspect) = load_texture(ctx, "neo-mascot-black", MASCOT_BLACK_PNG);
        let (white, white_aspect) = load_texture(ctx, "neo-mascot-white", MASCOT_WHITE_PNG);
        debug_assert!((aspect - white_aspect).abs() < 0.001);
        Self {
            black,
            white,
            aspect,
        }
    }

    /// Cache one mark per context. The mark only changes when the embedded
    /// assets change, so regenerating it every frame is unnecessary.
    pub fn cached(ctx: &Context) -> Self {
        let id = egui::Id::new("neo-brand-mark");
        if let Some(mark) = ctx.data(|d| d.get_temp::<Self>(id)) {
            return mark;
        }
        let mark = Self::load(ctx);
        ctx.data_mut(|d| d.insert_temp(id, mark.clone()));
        mark
    }

    /// Width / height of the mascot artwork.
    pub fn aspect(&self) -> f32 {
        self.aspect
    }

    /// Paint the variant that contrasts with the current egui surface.
    pub fn paint(&self, painter: &Painter, rect: Rect, _tint: Color32) {
        let texture = if painter.ctx().theme() == egui::Theme::Dark {
            &self.white
        } else {
            &self.black
        };
        let target = fit_aspect(rect, self.aspect);
        painter.image(
            texture.id(),
            target,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

fn load_texture(ctx: &Context, name: &str, bytes: &[u8]) -> (TextureHandle, f32) {
    let image = image::load_from_memory(bytes)
        .expect("embedded brand PNG must decode")
        .to_rgba8();
    let (width, height) = image.dimensions();
    let pixels = image
        .pixels()
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect::<Vec<_>>();
    let texture = ctx.load_texture(
        name,
        egui::ColorImage::new([width as usize, height as usize], pixels),
        TextureOptions::LINEAR,
    );
    (texture, width as f32 / height.max(1) as f32)
}

fn rgba_from_png(bytes: &[u8], size: usize) -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(bytes)
        .expect("embedded icon PNG must decode")
        .to_rgba8();
    let resized = image::imageops::resize(
        &image,
        size as u32,
        size as u32,
        image::imageops::FilterType::Lanczos3,
    );
    (resized.into_raw(), size as u32, size as u32)
}

/// Square tray icon. `white = true` selects the dark-background variant.
pub fn tray_rgba(size: usize, white: bool) -> (Vec<u8>, u32, u32) {
    rgba_from_png(
        if white {
            TRAY_WHITE_PNG
        } else {
            TRAY_BLACK_PNG
        },
        size,
    )
}

/// Square window/taskbar icon (`neo` + spark). `white = true` selects the
/// dark-background variant.
pub fn window_rgba(size: usize, white: bool) -> (Vec<u8>, u32, u32) {
    rgba_from_png(
        if white {
            WINDOW_WHITE_PNG
        } else {
            WINDOW_BLACK_PNG
        },
        size,
    )
}

/// Square large application icon (full cat + wordmark).
pub fn app_rgba(size: usize, white: bool) -> (Vec<u8>, u32, u32) {
    rgba_from_png(if white { APP_WHITE_PNG } else { APP_BLACK_PNG }, size)
}

/// Legacy brand-blue whale bitmap, still used by the floating control until a
/// dedicated small mascot crop is available.
pub fn whale_rgba(size: usize) -> (Vec<u8>, u32, u32) {
    let subpaths = svg::parse_subpaths(FISH_LOGO_PATH);
    let aspect = VIEWBOX_W / VIEWBOX_H;
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
            rgba[px] = 0x4D;
            rgba[px + 1] = 0x6B;
            rgba[px + 2] = 0xFE;
            rgba[px + 3] = a;
        }
    }
    (rgba, size as u32, size as u32)
}

/// Fit `outer` into the artwork aspect ratio without stretching.
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
