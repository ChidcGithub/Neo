//! Brand marks and system icon bitmaps.
//!
//! The in-app brand mark is the Neo cat mascot. It ships as an embedded PNG
//! pair (black for light surfaces, white for dark surfaces); the theme's
//! `dark_mode` flag selects the texture at paint time. System icons (tray,
//! window, installer) use separately cropped square exports.

use egui::{Color32, Context, Painter, Pos2, Rect, TextureHandle, TextureOptions, Vec2};

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

/// Physical pixel size of a notification-area icon.
///
/// Rendering the wordmark at exactly this size keeps Windows from resampling
/// the thin glyphs into a blur.
pub fn tray_size() -> usize {
    #[cfg(windows)]
    let size = {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        unsafe { GetSystemMetrics(SM_CXSMICON) }
    };
    #[cfg(not(windows))]
    let size = 32;
    usize::try_from(size)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(32)
}

/// Square tray wordmark. `white = true` selects the dark-background variant.
///
/// The embedded PNG has no horizontal padding, so the wordmark fills the
/// notification-area slot instead of floating inside a mostly empty square.
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

/// Tray's alternate style (`neo` + spark), same color as [`tray_rgba`].
pub fn tray_spark_rgba(size: usize, white: bool) -> (Vec<u8>, u32, u32) {
    window_rgba(size, white)
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
