// Startup-only frozen backdrop. No file, clipboard, model or screenshot registry IO.
use super::*;

const MAX_PIXELS: usize = 4 * 1024 * 1024;

fn pixel_count(width: i32, height: i32) -> Option<usize> {
    if width <= 0 || height <= 0 { return None; }
    (width as usize).checked_mul(height as usize).filter(|n| *n <= MAX_PIXELS)
}

// Downsampling before blur bounds startup cost independently of desktop resolution.
fn render(source: &[u32], sw: u32, sh: u32, width: i32, height: i32) -> Option<Vec<u32>> {
    let count = pixel_count(width, height)?;
    if sw == 0 || sh == 0 || sw > 512 || sh > 256 || source.len() != (sw * sh) as usize {
        return None;
    }
    let image = image::RgbImage::from_fn(sw, sh, |x, y| {
        let p = source[(y * sw + x) as usize];
        image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
    });
    let blurred = image::imageops::blur(&image, 6.0);
    let expanded = image::imageops::resize(&blurred, width as u32, height as u32,
        image::imageops::FilterType::Triangle);
    let mut result = Vec::with_capacity(count);
    for (x, y, pixel) in expanded.enumerate_pixels() {
        let hash = x.wrapping_mul(374761393).wrapping_add(y.wrapping_mul(668265263));
        let noise = ((hash ^ (hash >> 13)) % 3) as i32 - 1;
        let sheen = 4 - (y * 8 / height as u32) as i32;
        let tint = [240, 242, 245];
        // Equal backdrop/tint weighting keeps the frost while revealing more color.
        let channel = |i: usize| ((pixel[i] as i32 + tint[i] + 1) / 2 + sheen + noise)
            .clamp(0, 255) as u32;
        result.push(0xff000000 | channel(0) << 16 | channel(1) << 8 | channel(2));
    }
    Some(result)
}

/// Called once, before showing/creating the card, so it cannot capture itself.
/// Capture failure (including inaccessible desktops) falls back to a solid plate.
/// Only the small blurred/tinted result survives; it is released with the card.
pub(super) unsafe fn capture(x: i32, y: i32, width: i32, height: i32) -> Option<Vec<u32>> {
    pixel_count(width, height)?;
    let sw = ((width + 3) / 4).min(512);
    let sh = ((height + 3) / 4).min(256);
    let screen = GetDC(null_mut());
    if screen.is_null() { return None; }
    let dc = CreateCompatibleDC(screen);
    if dc.is_null() { ReleaseDC(null_mut(), screen); return None; }
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: sw, biHeight: -sh, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB,
        ..std::mem::zeroed() };
    let mut bits = null_mut();
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if bitmap.is_null() || bits.is_null() {
        if !bitmap.is_null() { DeleteObject(bitmap); }
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
        return None;
    }
    let old = SelectObject(dc, bitmap);
    let selected = !old.is_null() && old as isize != GDI_ERROR as isize;
    let mut captured = None;
    if selected {
        SetStretchBltMode(dc, HALFTONE as i32);
        SetBrushOrgEx(dc, 0, 0, null_mut());
        let copied = StretchBlt(dc, 0, 0, sw, sh, screen, x, y, width, height, SRCCOPY | CAPTUREBLT);
        let flushed = GdiFlush();
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (sw * sh) as usize);
        if copied != 0 && flushed != 0 { captured = Some(pixels.to_vec()); }
        pixels.fill(0);
        SelectObject(dc, old);
    }
    DeleteObject(bitmap);
    DeleteDC(dc);
    ReleaseDC(null_mut(), screen);
    let mut raw = captured?;
    let result = render(&raw, sw as u32, sh as u32, width, height);
    raw.fill(0);
    result
}

#[cfg(test)]
mod tests {
    include!("startup_glass_tests.rs");
}
