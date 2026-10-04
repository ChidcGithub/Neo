//! Off-screen GDI only: no HWND, desktop capture, compositor flush or GUI.
use super::*;

// Keep the pre-optimization per-pixel geometry and duplicate coverage lookup
// as an independent reference for the complete premultiplied output.
fn reference_pixels(
    cache: &RenderCache,
    size: i32,
    scale: f32,
    amount: f32,
    glass: Option<&[u32]>,
) -> Vec<u32> {
    let icon = if amount > 0.0 {
        &cache.blurred.as_ref().unwrap().1
    } else {
        cache.icon.as_ref().unwrap()
    };
    let icon_size = icon.width() as i32;
    let icon_left = (90.0 * scale).round() as i32 - icon_size / 2;
    let mut pixels = vec![0; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let p = Point {
                x: (x as f32 + 0.5) / scale,
                y: (y as f32 + 0.5) / scale,
            };
            let Some(circle) =
                animated_circles(amount).find(|c| circle_coverage(p, *c, scale) > 0.0)
            else {
                continue;
            };
            let index = (y * size + x) as usize;
            let distance = p.distance(circle.center);
            let mut color = glass.map_or(0xffc1c1c1, |v| v[index]);
            let rim = (distance - (circle.radius - 1.0)).clamp(0.0, 1.0);
            color = blend(color, [236, 238, 240], (100.0 * rim) as u32);
            if circle.target == Target::Main {
                let (ix, iy) = (x - icon_left, y - icon_left);
                if ix >= 0 && iy >= 0 && ix < icon_size && iy < icon_size {
                    let p = icon.get_pixel(ix as u32, iy as u32);
                    color = blend(color, [65, 68, 72], p[3] as u32);
                }
            } else if circle.target == Target::Close {
                let dx = (p.x - circle.center.x).abs();
                let dy = (p.y - circle.center.y).abs();
                if dx.max(dy) < 6.5 {
                    let coverage = ((1.4 - (dx - dy).abs()) * scale).clamp(0.0, 1.0);
                    color = blend(color, [197, 54, 59], (coverage * 255.0) as u32);
                }
            } else {
                color = blend(color, [96, 99, 103], cache.mask[index] as u32);
            }
            let opacity = if circle.target == Target::Main {
                1.0
            } else {
                amount
            };
            let alpha = circle_coverage(p, circle, scale) * opacity;
            let a = (alpha * 255.0).round() as u32;
            let premul = |shift: u32| (((color >> shift) & 255) * a + 127) / 255;
            pixels[index] = a << 24 | premul(16) << 16 | premul(8) << 8 | premul(0);
        }
    }
    pixels
}

#[test]
fn precomputed_geometry_and_reused_coverage_match_original_pixels() {
    unsafe {
        let mut cache = RenderCache::default();
        for scale in [0.5, 1.0, 1.25, 2.0, 4.0] {
            let size = (EXTENT * scale) as i32;
            let glass: Vec<_> = (0..size * size)
                .map(|i| 0xff000000 | (i as u32 * 7919 & 0x00ffffff))
                .collect();
            for language in [crate::i18n::Language::ZhCn, crate::i18n::Language::EnUs] {
                crate::i18n::with_language(language, || {
                    for amount in [0.0, 0.01, 0.5, 1.0] {
                        for background in [None, Some(glass.as_slice())] {
                            cache.prepare(size, scale, amount).unwrap();
                            let expected =
                                reference_pixels(&cache, size, scale, amount, background);
                            paint(&mut cache, size, scale, amount, background).unwrap();
                            assert_eq!(
                                cache.dib.as_mut().unwrap().pixels(),
                                expected,
                                "scale={scale}, amount={amount}, language={language:?}, glass={}",
                                background.is_some()
                            );
                        }
                    }
                });
            }
        }
    }
}

#[test]
fn repeated_frames_reuse_dib_artwork_fonts_and_mask() {
    crate::i18n::with_language(crate::i18n::Language::ZhCn, || unsafe {
        let mut cache = RenderCache::default();
        paint(&mut cache, 180, 1.0, 1.0, None).unwrap();
        let pixels = cache.dib.as_mut().unwrap().pixels().to_vec();
        let bitmap = cache.dib.as_ref().unwrap().bitmap;
        let fonts = cache.fonts.0.clone();
        for _ in 0..20 {
            paint(&mut cache, 180, 1.0, 1.0, None).unwrap();
            assert_eq!(cache.dib.as_mut().unwrap().pixels(), pixels);
            assert_eq!(cache.dib.as_ref().unwrap().bitmap, bitmap);
            assert_eq!(cache.fonts.0, fonts);
        }
        assert_eq!(
            cache.work,
            CacheWork {
                dibs: 1,
                rasterizations: 1,
                blurs: 1,
                masks: 1
            }
        );
    });
}

#[test]
fn cached_pixels_match_fresh_frames_across_animation_language_and_dpi() {
    unsafe {
        let mut cache = RenderCache::default();
        for scale in [1.0, 1.25, 2.0] {
            let size = (EXTENT * scale) as i32;
            for language in [crate::i18n::Language::ZhCn, crate::i18n::Language::EnUs] {
                crate::i18n::with_language(language, || {
                    for amount in [0.0, 0.01, 0.5, 1.0, 0.5, 0.0] {
                        let mut fresh = RenderCache::default();
                        paint(&mut fresh, size, scale, amount, None).unwrap();
                        paint(&mut cache, size, scale, amount, None).unwrap();
                        assert_eq!(
                            cache.dib.as_mut().unwrap().pixels(),
                            fresh.dib.as_mut().unwrap().pixels()
                        );
                    }
                });
            }
        }
    }
}

#[test]
fn cache_invalidation_distinguishes_language_dpi_and_animation() {
    use crate::i18n::{with_language, Language};
    with_language(Language::ZhCn, || unsafe {
        let mut cache = RenderCache::default();
        cache.prepare(180, 1.0, 1.0).unwrap();
        with_language(Language::EnUs, || {
            cache.prepare(180, 1.0, 1.0).unwrap();
            assert_eq!(
                cache.work,
                CacheWork {
                    dibs: 1,
                    rasterizations: 1,
                    blurs: 1,
                    masks: 2
                }
            );
        });
        cache.prepare(225, 1.25, 1.0).unwrap();
        assert_eq!(
            cache.work,
            CacheWork {
                dibs: 2,
                rasterizations: 2,
                blurs: 2,
                masks: 3
            }
        );
        let fonts = cache.fonts.0.len();
        cache.prepare(225, 1.25, 1.0 - f32::EPSILON).unwrap();
        // Same rounded text bounds, but the exact blur amount still changes.
        assert_eq!(
            cache.work,
            CacheWork {
                dibs: 2,
                rasterizations: 2,
                blurs: 3,
                masks: 3
            }
        );
        assert_eq!(cache.fonts.0.len(), fonts);
        // A scale change must invalidate even if the rounded DIB size is equal.
        cache.prepare(225, 1.2501, 1.0).unwrap();
        assert_eq!(
            cache.work,
            CacheWork {
                dibs: 2,
                rasterizations: 3,
                blurs: 4,
                masks: 4
            }
        );
    });
}

#[test]
fn neutral_repaint_removes_all_previous_synthetic_glass_pixels() {
    unsafe {
        let mut cache = RenderCache::default();
        let glass = vec![0xff123456; 180 * 180];
        paint(&mut cache, 180, 1.0, 1.0, Some(&glass)).unwrap();
        paint(&mut cache, 180, 1.0, 0.0, None).unwrap();
        let mut fresh = RenderCache::default();
        paint(&mut fresh, 180, 1.0, 0.0, None).unwrap();
        assert_eq!(
            cache.dib.as_mut().unwrap().pixels(),
            fresh.dib.as_mut().unwrap().pixels()
        );
    }
}

#[test]
fn win10_gdi_labels_fit_circular_buttons_at_common_dpi_scales() {
    unsafe {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
            let mut dib = Dib::new((EXTENT * scale) as i32).unwrap();
            let mut fonts = Fonts::default();
            for amount in [0.01, 0.5, 1.0] {
                for language in [crate::i18n::Language::ZhCn, crate::i18n::Language::EnUs] {
                    crate::i18n::with_language(language, || {
                        dib.pixels().fill(0);
                        let mask = labels(&mut dib, scale, amount, &mut fonts).unwrap();
                        assert_eq!(mask.len(), (dib.size * dib.size) as usize);
                        assert!(mask.iter().any(|alpha| *alpha != 0));
                    });
                }
                for circle in animated_circles(amount).skip(2) {
                    let bounds = label_bounds(circle, scale);
                    for text in ["画板", "黑板", "Drawing", "Blackboard"] {
                        dib.pixels().fill(0);
                        let measured = draw_label(
                            dib.dc,
                            text,
                            bounds,
                            (8.5 * scale).round() as i32,
                            &mut fonts,
                        )
                        .unwrap();
                        assert!(measured.cx > 0 && measured.cy > 0, "{text:?}");
                        assert!(
                            measured.cx <= bounds.right - bounds.left,
                            "width: {text:?}, {scale}, {amount}"
                        );
                        assert!(
                            measured.cy <= bounds.bottom - bounds.top,
                            "height: {text:?}, {scale}, {amount}"
                        );
                        assert_ne!(GdiFlush(), 0);
                        assert!(dib.pixels().iter().any(|pixel| pixel & 0x00ffffff != 0));
                    }
                }
            }
        }
    }
}
