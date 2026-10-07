// Pure, bounded software composition shared by the window and synthetic preview.
const MAX_CARD_PIXELS: usize = 4 * 1024 * 1024;

fn rounded_distance(r: &RECT, radius: f32, x: f32, y: f32) -> f32 {
    let hx = (r.right - r.left) as f32 * 0.5;
    let hy = (r.bottom - r.top) as f32 * 0.5;
    let radius = radius.min(hx).min(hy);
    let qx = (x - (r.left + r.right) as f32 * 0.5).abs() - hx + radius;
    let qy = (y - (r.top + r.bottom) as f32 * 0.5).abs() - hy + radius;
    qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - radius
}

fn over(foreground: u32, background: u32) -> u32 {
    let missing = 255 - (foreground >> 24);
    let channel = |shift: u32| -> u32 {
        ((foreground >> shift) & 255) + (((background >> shift) & 255) * missing + 127) / 255
    };
    channel(24) << 24 | channel(16) << 16 | channel(8) << 8 | channel(0)
}

fn mix(a: [u8; 3], b: [u8; 3], coverage: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        (a[i] as f32 + (b[i] as f32 - a[i] as f32) * coverage.clamp(0., 1.)).round() as u8
    })
}

fn card_pixel(
    g: &Layout,
    x: i32,
    y: i32,
    glyph: u8,
    segment: &RECT,
    duplicate: bool,
    palette: &Palette,
) -> u32 {
    if x < 0 || y < 0 || x >= g.width || y >= g.height {
        return 0;
    }
    let scale = g.width as f32 / 400.;
    let (xx, yy) = (x as f32 + 0.5, y as f32 + 0.5);
    let distance = rounded_distance(&g.body, g.radius, xx, yy);
    let coverage = (0.5 - distance).clamp(0., 1.);
    let shadow = if palette.high_contrast {
        0
    } else {
        let d = rounded_distance(&g.body, g.radius, xx, yy - 2. * scale).max(0.);
        let falloff = (1. - d / (7. * scale)).clamp(0., 1.);
        premultiplied([30, 40, 65], 0.14 * falloff * falloff)
    };
    if coverage == 0. {
        return shadow;
    }
    let edge = (1. - (-distance / scale).max(0.)).clamp(0., 1.);
    let border = if palette.high_contrast {
        palette.brand
    } else {
        mix(palette.body, [255, 255, 255], 0.7)
    };
    let mut color = mix(palette.body, border, edge);
    let ink = if palette.high_contrast {
        palette.brand
    } else if y >= g.caption.top {
        palette.caption
    } else {
        [32, 43, 64]
    };
    color = mix(color, ink, glyph as f32 / 255.);
    if x >= g.progress.left && x < g.progress.right && y >= g.progress.top && y < g.progress.bottom
    {
        let radius = (g.progress.bottom - g.progress.top) as f32 * 0.5;
        let track = (0.5 - rounded_distance(&g.progress, radius, xx, yy)).clamp(0., 1.);
        color = mix(color, palette.track, track);
        if !duplicate {
            let active = (0.5 - rounded_distance(segment, radius, xx, yy)).clamp(0., 1.);
            let ink = if palette.high_contrast {
                palette.body
            } else {
                palette.brand
            };
            color = mix(color, ink, active.min(track));
        }
    }
    over(premultiplied(color, coverage), shadow)
}

fn render_plate(
    g: &Layout,
    mask: &[u8],
    icon: &[u8],
    glass: Option<&[u32]>,
    palette: Palette,
) -> Option<Vec<u32>> {
    if g.width <= 0 || g.height <= 0 {
        return None;
    }
    let count = (g.width as usize).checked_mul(g.height as usize)?;
    let side = usize::try_from(g.icon.right.checked_sub(g.icon.left)?).ok()?;
    if count > MAX_CARD_PIXELS
        || mask.len() != count
        || icon.len() != side.checked_mul(side)?.checked_mul(4)?
        || glass.is_some_and(|p| p.len() != count)
    {
        return None;
    }
    let radius = (g.width as f32 / 400.).round().clamp(1., 4.) as usize;
    let segment = progress_segment(g, 0.);
    Some(
        (0..count)
            .map(|index| {
                let (x, y) = (index as i32 % g.width, index as i32 / g.width);
                let mut local = palette;
                if !palette.high_contrast {
                    if let Some(glass) = glass {
                        let p = glass[index];
                        local.body = [(p >> 16) as u8, (p >> 8) as u8, p as u8];
                    }
                    // A local glyph keyline preserves the frozen backdrop elsewhere.
                    let near_text = [g.brand, g.caption].iter().any(|r| {
                        x >= r.left - radius as i32
                            && x < r.right + radius as i32
                            && y >= r.top - radius as i32
                            && y < r.bottom + radius as i32
                    });
                    if near_text {
                        local = readable_text_palette(
                            local,
                            text_halo(mask, g.width as usize, index, radius),
                        );
                    }
                }
                let mut pixel = card_pixel(g, x, y, mask[index], &segment, true, &local);
                if x >= g.icon.left && x < g.icon.right && y >= g.icon.top && y < g.icon.bottom {
                    let i = ((y - g.icon.top) as usize * side + (x - g.icon.left) as usize) * 4;
                    pixel = over(
                        premultiplied(palette.brand, icon[i + 3] as f32 / 255.),
                        pixel,
                    );
                }
                pixel
            })
            .collect(),
    )
}

fn render_progress(pixels: &mut [u32], g: &Layout, phase: f32, duplicate: bool, palette: Palette) {
    if duplicate {
        return;
    }
    let segment = progress_segment(g, phase);
    let radius = (g.progress.bottom - g.progress.top) as f32 * 0.5;
    for y in g.progress.top..g.progress.bottom {
        for x in segment.left..segment.right {
            let coverage = (0.5
                - rounded_distance(&segment, radius, x as f32 + 0.5, y as f32 + 0.5))
            .clamp(0., 1.);
            let index = (y * g.width + x) as usize;
            // System-color cutout remains visible even when track and ink are identical.
            let ink = if palette.high_contrast {
                palette.body
            } else {
                palette.brand
            };
            pixels[index] = over(premultiplied(ink, coverage), pixels[index]);
        }
    }
}
