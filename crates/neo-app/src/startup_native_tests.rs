use super::*;
// 几何、动画和像素测试为纯逻辑；降级区域测试只创建内存 GDI 区域，不创建窗口。
mod visual {
    use super::*;

    #[test]
    fn layout_scales_without_overlap() {
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let card = layout(scale);
            assert_eq!(card.height, (144. * scale).round() as i32);
            assert_eq!(card.width, (400. * scale).round() as i32);
            assert!(card.icon.right < card.brand.left);
            assert_eq!(
                card.icon.right - card.icon.left,
                card.icon.bottom - card.icon.top
            );
            assert!(card.brand.bottom < card.caption.top);
            assert!(card.caption.bottom < card.progress.top);
            assert!(card.progress.left > card.body.left && card.progress.right < card.body.right);
            assert!(card.progress.bottom < card.body.bottom);
            assert!(((card.progress.bottom - card.progress.top) as f32 - 3. * scale).abs() <= 1.);
            for r in [
                card.body,
                card.icon,
                card.brand,
                card.caption,
                card.progress,
            ] {
                assert!(
                    r.left >= 0 && r.top >= 0 && r.right <= card.width && r.bottom <= card.height
                );
                assert!(r.left < r.right && r.top < r.bottom);
            }
        }
    }

    #[test]
    fn screen_sizing_tracks_work_area_and_fits_scaled_displays() {
        let area = |w, h| RECT {
            left: -2400,
            top: -200,
            right: -2400 + w,
            bottom: -200 + h,
        };
        let mut previous_width = 0;
        for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)] {
            let g = layout(screen_scale(&area(w, h), 96).unwrap());
            assert!(g.width > previous_width);
            previous_width = g.width;
        }
        for (w, h) in [
            (640, 360),
            (1080, 1920),
            (1920, 1040),
            (3440, 1440),
            (7680, 4320),
        ] {
            for dpi in [0, 96, 120, 144, 192, 288, 384] {
                let g = layout(screen_scale(&area(w, h), dpi).unwrap());
                assert!(g.width <= 1440 && g.width as f32 <= w as f32 * 0.85 + 1.);
                assert!(g.height as f32 <= h as f32 * 0.4 + 1.);
                assert!((g.width as f32 - g.height as f32 * (400. / 144.)).abs() <= 2.5);
                assert!(g.brand.bottom < g.caption.top && g.caption.bottom < g.progress.top);
                assert!(g.progress.top < g.progress.bottom);
            }
        }
        let hd = layout(screen_scale(&area(1920, 1080), 96).unwrap());
        let hd_scaled = layout(screen_scale(&area(1920, 1080), 144).unwrap());
        assert_eq!((hd.width, hd.height), (422, 152));
        assert_eq!(
            (hd.width, hd.height),
            (hd_scaled.width, hd_scaled.height),
            "do not multiply screen pixels by DPI twice"
        );
        assert!(screen_scale(&area(0, 1080), 96).is_none());
        assert!(screen_scale(&area(1920, -1), 96).is_none());
        assert!(screen_scale(&area(80, 40), 96).is_none());
        assert!(screen_scale(
            &RECT {
                left: i32::MIN,
                right: i32::MAX,
                top: 0,
                bottom: 1080
            },
            96
        )
        .is_none());
    }

    #[test]
    fn backdrop_uses_system_colors_when_required() {
        assert_eq!(backdrop_choice(true, false, true), Backdrop::Glass);
        assert_eq!(backdrop_choice(true, false, false), Backdrop::Opaque);
        for (readable, contrast) in [(false, false), (false, true), (true, true)] {
            for available in [false, true] {
                assert_eq!(
                    backdrop_choice(readable, contrast, available),
                    Backdrop::HighContrast
                );
            }
        }
    }

    #[test]
    fn text_contrast_improves_without_clouding_the_glass() {
        let mut mask = vec![0; 7 * 5];
        mask[2 * 7 + 3] = 255;
        for index in 0..mask.len() {
            let halo = text_halo(&mask, 7, index, 1);
            let near = (1..=3).contains(&(index / 7)) && (2..=4).contains(&(index % 7));
            assert_eq!(halo, if near { 255 } else { 0 });
        }
        // At row boundaries, dilation must not wrap into the next row.
        mask.fill(0);
        mask[6] = 255;
        assert_eq!(text_halo(&mask, 7, 7, 1), 0);
        let luminance = |color: [u8; 3]| {
            let linear = color.map(|c| {
                let s = c as f32 / 255.;
                if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            });
            linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
        };
        for body in [[0, 0, 0], [112, 114, 116], [180, 160, 200], [255, 255, 255]] {
            let base = Palette {
                body,
                ..Palette::light()
            };
            assert_eq!(readable_text_palette(base, 0).body, body);
            let outlined = readable_text_palette(base, 255);
            assert!(luminance(outlined.brand) > luminance([24, 35, 76]));
            assert!(luminance(outlined.caption) > luminance([22, 27, 36]));
            for ink in [outlined.brand, outlined.caption] {
                assert!((luminance(outlined.body) + 0.05) / (luminance(ink) + 0.05) >= 4.5);
            }
        }
    }

    #[test]
    fn rounded_edges_shadow_and_inset_track_are_premultiplied() {
        let palette = Palette::light();
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let g = layout(scale);
            let segment = progress_segment(&g, 0.);
            for duplicate in [false, true] {
                let pixel = |x, y| card_pixel(&g, x, y, 0, &segment, duplicate, &palette);
                for x in [0, g.width - 1] {
                    for y in [0, g.height - 1] {
                        assert_eq!(pixel(x, y), 0);
                    }
                }
                assert_eq!(
                    pixel(g.width / 2, g.height / 2),
                    premultiplied(palette.body, 1.)
                );
                for (x, y) in [(-1, 0), (0, -1), (g.width, 0), (0, g.height)] {
                    assert_eq!(pixel(x, y), 0);
                }
                let mut antialiased = false;
                for y in 0..g.height {
                    for x in 0..g.width {
                        let value = pixel(x, y);
                        let alpha = value >> 24;
                        antialiased |= alpha > 0 && alpha < 255;
                        for shift in [0, 8, 16] {
                            assert!((value >> shift) & 255 <= alpha);
                        }
                        assert_eq!(opaque_pixel(value, palette.body) >> 24, 255);
                    }
                }
                assert!(antialiased);
                assert_eq!(
                    pixel(g.width / 2, g.body.top + g.body.bottom / 2) >> 24,
                    255
                );
            }
        }
    }

    #[test]
    fn glyph_coverage_is_clear_and_premultiplied() {
        let g = layout(1.);
        let palette = Palette::light();
        let segment = progress_segment(&g, 0.);
        for coverage in 0..=255 {
            for (x, y, ink) in [(150, 40, [32, 43, 64]), (150, 85, palette.caption)] {
                let value = card_pixel(&g, x, y, coverage, &segment, false, &palette);
                for (shift, index) in [(16, 0), (8, 1), (0, 2)] {
                    let expected = (ink[index] as u32 * coverage as u32
                        + palette.body[index] as u32 * (255 - coverage as u32)
                        + 127)
                        / 255;
                    assert!((((value >> shift) & 255) as i32 - expected as i32).abs() <= 1);
                }
                assert_eq!(value >> 24, 255);
            }
            for alpha in 0..=255 {
                let value = premultiplied([coverage, 107, 254], alpha as f32 / 255.);
                assert_eq!(value >> 24, alpha);
                for shift in [0, 8, 16] {
                    assert!((value >> shift) & 255 <= alpha);
                }
            }
        }
        assert_eq!(
            card_pixel(&g, 150, 40, 255, &segment, false, &palette),
            0xff202b40
        );
        assert_eq!(
            card_pixel(&g, 150, 85, 255, &segment, false, &palette),
            0xff303746
        );
        assert_eq!(opaque_pixel(0, palette.body), 0xfff2f4f8);
    }

    #[test]
    fn frozen_glass_composite_keeps_text_and_progress_opaque() {
        let g = layout(1.);
        let palette = Palette {
            body: [201, 209, 214],
            ..Palette::light()
        };
        let segment = progress_segment(&g, 0.);
        let pixel = |x, y, glyph| card_pixel(&g, x, y, glyph, &segment, false, &palette);
        assert_eq!(pixel(0, 0, 0), 0);
        assert_eq!(pixel(180, 40, 255), 0xff202b40);
        assert_eq!(pixel(180, 85, 255), 0xff303746);
        for glyph in 0..=255 {
            let value = pixel(180, 40, glyph);
            let alpha = value >> 24;
            assert_eq!(alpha, 255);
            for shift in [0, 8, 16] {
                assert!((value >> shift) & 255 <= alpha);
            }
            assert_eq!(opaque_pixel(value, palette.body) >> 24, 255);
        }
        for x in g.progress.left..g.progress.right {
            assert_eq!(pixel(x, g.progress.top, 0) >> 24, 255);
        }
    }

    #[test]
    fn animation_changes_only_bottom_bar_and_duplicate_is_static() {
        for elapsed in [0., 0.04, 1., 100., 100000.] {
            let phase = animation_phase(elapsed, false);
            assert!((0. ..std::f32::consts::TAU).contains(&phase));
            assert_eq!(animation_phase(elapsed, true), 0.);
        }
        let palette = Palette::light();
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let g = layout(scale);
            let first_segment = progress_segment(&g, 0.);
            let next_segment = progress_segment(&g, animation_phase(0.5, false));
            let mut changed = 0;
            for y in 0..g.height {
                for x in 0..g.width {
                    let first = card_pixel(&g, x, y, 0, &first_segment, false, &palette);
                    let next = card_pixel(&g, x, y, 0, &next_segment, false, &palette);
                    assert_eq!(first >> 24, next >> 24);
                    if first != next {
                        changed += 1;
                        assert!(y >= g.progress.top && y < g.progress.bottom);
                    }
                    assert_eq!(
                        card_pixel(&g, x, y, 0, &first_segment, true, &palette),
                        card_pixel(&g, x, y, 0, &next_segment, true, &palette)
                    );
                }
            }
            assert!(changed > 0 && changed <= g.width * (g.progress.bottom - g.progress.top));
        }
    }

    #[test]
    fn progress_segment_stays_bounded_and_reaches_both_edges() {
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let g = layout(scale);
            let first = progress_segment(&g, 0.);
            assert_eq!(first.left, g.progress.left);
            assert_eq!(
                progress_segment(&g, std::f32::consts::PI).right,
                g.progress.right
            );
            assert_eq!(
                progress_segment(&g, std::f32::consts::TAU).left,
                g.progress.left
            );
            for step in 0..=1000 {
                let segment = progress_segment(&g, animation_phase(step as f32 * 0.04, false));
                assert!(segment.left >= g.progress.left && segment.right <= g.progress.right);
                assert_eq!(segment.right - segment.left, first.right - first.left);
                assert_eq!(
                    (segment.top, segment.bottom),
                    (g.progress.top, g.progress.bottom)
                );
            }
        }
    }

    #[test]
    fn fallback_region_matches_rounded_body_without_shadow() {
        for scale in [1., 1.25, 1.5, 2., 3.] {
            let g = layout(scale);
            unsafe {
                let region = fallback_region(&g);
                assert!(!region.is_null());
                let cx = g.width / 2;
                let cy = g.height / 2;
                assert_ne!(PtInRegion(region, cx, cy), 0);
                for (x, y) in [(-1, cy), (g.width, cy), (cx, -1), (cx, g.height)] {
                    assert_eq!(PtInRegion(region, x, y), 0);
                }
                for x in [0, g.width - 1] {
                    for y in [0, g.height - 1] {
                        assert_eq!(PtInRegion(region, x, y), 0);
                    }
                }
                let mut bounds: RECT = std::mem::zeroed();
                assert_eq!(GetRgnBox(region, &mut bounds), COMPLEXREGION);
                assert_eq!(
                    (bounds.left, bounds.top, bounds.right, bounds.bottom),
                    (g.body.left, g.body.top, g.body.right, g.body.bottom)
                );
                assert_ne!(DeleteObject(region), 0);
            }
        }
    }
}
// Memory DIB only: no HWND, screen DC or capture API.
unsafe fn memory_glyphs(g: &Layout, scale: f32, caption: &str) -> Vec<u8> {
    let dc = CreateCompatibleDC(null_mut());
    assert!(!dc.is_null());
    let mut info: BITMAPINFO = std::mem::zeroed();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: g.width,
        biHeight: -g.height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        ..std::mem::zeroed()
    };
    let mut bits = null_mut();
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    assert!(!bitmap.is_null() && !bits.is_null());
    let old = SelectObject(dc, bitmap);
    let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (g.width * g.height) as usize);
    pixels.fill(0);
    let drawn = draw_glyphs(dc, g, scale, caption);
    let mask = pixels.iter().map(|p| (*p & 255) as u8).collect();
    SelectObject(dc, old);
    DeleteObject(bitmap);
    DeleteDC(dc);
    assert!(drawn);
    mask
}

#[test]
fn cached_plate_preserves_glass_brand_and_animation_bounds() {
    let g = layout(1.);
    let mask = unsafe { memory_glyphs(&g, 1., "正在启动") };
    assert!(mask.contains(&255));
    let (icon, _, _) = crate::brand::whale_rgba(76);
    let glass = vec![0xffbbc9d8; (g.width * g.height) as usize];
    let palette = Palette::light();
    let plate = render_plate(&g, &mask, &icon, Some(&glass), palette).unwrap();
    assert_eq!(plate[(105 * g.width + 200) as usize], 0xffbbc9d8);
    assert!(plate.iter().any(|p| *p == premultiplied(palette.brand, 1.)));
    let mut first = plate.clone();
    let mut next = plate.clone();
    render_progress(&mut first, &g, 0., false, palette);
    render_progress(&mut next, &g, 2., false, palette);
    assert_ne!(first, next);
    for (i, (a, b)) in first.iter().zip(&next).enumerate() {
        if a != b {
            let (x, y) = (i as i32 % g.width, i as i32 / g.width);
            assert!(x >= g.progress.left && x < g.progress.right);
            assert!(y >= g.progress.top && y < g.progress.bottom);
        }
    }
    let mut duplicate = plate.clone();
    render_progress(&mut duplicate, &g, 2., true, palette);
    assert_eq!(plate, duplicate);
    assert!(render_plate(&g, &[], &icon, None, palette).is_none());
    assert!(render_plate(&g, &mask, &[], None, palette).is_none());
    assert!(render_plate(&g, &mask, &icon, Some(&[]), palette).is_none());
    assert!(render_plate(&layout(10.), &[], &[], None, palette).is_none());
}

#[test]
fn high_contrast_ignores_glass_and_has_no_decorative_shadow() {
    let g = layout(1.);
    let mask = vec![0; (g.width * g.height) as usize];
    let (icon, _, _) = crate::brand::whale_rgba(76);
    for (body, ink) in [([0, 0, 0], [255, 255, 255]), ([255, 255, 255], [0, 0, 0])] {
        let palette = Palette {
            body,
            brand: ink,
            caption: ink,
            track: ink,
            high_contrast: true,
        };
        let plate = render_plate(&g, &mask, &icon, None, palette).unwrap();
        let mut first = plate.clone();
        let mut next = plate.clone();
        render_progress(&mut first, &g, 0., false, palette);
        render_progress(&mut next, &g, 2., false, palette);
        assert_ne!(
            first, next,
            "high contrast must retain a visible waiting phase"
        );
        let glass = vec![0xff804080; mask.len()];
        assert_eq!(
            plate,
            render_plate(&g, &mask, &icon, Some(&glass), palette).unwrap()
        );
        assert_eq!(plate[(135 * g.width + 200) as usize], 0);
        assert_eq!(
            plate[(60 * g.width + 200) as usize],
            premultiplied(body, 1.)
        );
        let segment = progress_segment(&g, 0.);
        assert_eq!(
            card_pixel(&g, 150, 40, 255, &segment, true, &palette),
            premultiplied(ink, 1.)
        );
    }
    unsafe {
        let palette = Palette::for_backdrop(Backdrop::HighContrast);
        let color = GetSysColor(COLOR_WINDOWTEXT);
        assert_eq!(
            palette.brand,
            [color as u8, (color >> 8) as u8, (color >> 16) as u8]
        );
        let region = fallback_region(&g);
        for (x, y) in [(g.body.left, g.body.top), (g.body.right - 1, g.body.top)] {
            assert_eq!(PtInRegion(region, x, y), 0);
        }
        for x in g.progress.left..g.progress.right {
            assert_ne!(PtInRegion(region, x, g.progress.top), 0);
        }
        DeleteObject(region);
    }
    let edge = premultiplied([242, 244, 248], 0.3);
    assert_eq!(
        opaque_pixel(edge, [242, 244, 248]),
        0xfff2f4f8,
        "no black fringe in opaque fallback"
    );
}

#[test]
#[ignore = "opt-in synthetic PNG; memory GDI only, never captures the desktop"]
fn synthetic_startup_preview() {
    let g = layout(1.);
    let (icon, _, _) = crate::brand::whale_rgba(76);
    let background = |x: u32, y: u32| {
        let wave = ((x as f32 / 180.).sin() * (y as f32 / 210.).cos() + 1.) * 0.5;
        mix([93, 132, 182], [229, 203, 186], wave)
    };
    let mut image = image::RgbImage::from_fn(880, 576, |x, y| image::Rgb(background(x, y)));
    for tile in 0..6 {
        let (ox, oy) = (30 + tile % 2 * 430, 24 + tile / 2 * 188);
        let mut palette = Palette::light();
        if tile >= 4 {
            let (body, ink) = if tile == 4 {
                ([0, 0, 0], [255, 255, 255])
            } else {
                ([255, 255, 255], [0, 0, 0])
            };
            palette = Palette {
                body,
                brand: ink,
                caption: ink,
                track: ink,
                high_contrast: true,
            };
        }
        let duplicate = tile == 1;
        let caption = if duplicate {
            "Neo 已经在运行"
        } else {
            "正在启动"
        };
        let mask = unsafe { memory_glyphs(&g, 1., caption) };
        let glass: Vec<_> = (0..g.width * g.height)
            .map(|i| {
                let color = background(ox + (i % g.width) as u32, oy + (i / g.width) as u32);
                premultiplied(mix(color, [240, 242, 245], 0.5), 1.)
            })
            .collect();
        let mut pixels = render_plate(
            &g,
            &mask,
            &icon,
            (tile < 2).then_some(glass.as_slice()),
            palette,
        )
        .unwrap();
        render_progress(&mut pixels, &g, 1.2, duplicate, palette);
        let region = if tile == 3 {
            unsafe { fallback_region(&g) }
        } else {
            null_mut()
        };
        for y in 0..g.height {
            for x in 0..g.width {
                if tile == 3 && unsafe { PtInRegion(region, x, y) } == 0 {
                    continue;
                }
                let mut p = pixels[(y * g.width + x) as usize];
                if tile == 3 {
                    p = opaque_pixel(p, palette.body);
                }
                let target = image.get_pixel_mut(ox + x as u32, oy + y as u32);
                p = opaque_pixel(p, target.0);
                *target = image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        if !region.is_null() {
            unsafe {
                DeleteObject(region);
            }
        }
    }
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs-pri/screens");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("startup-synthetic.png");
    image.save(&path).unwrap();
    println!("Synthetic only (no screenshot): {}", path.display());
    println!("Rows: glass starting / duplicate; opaque / GDI fallback; HC dark / HC light");
}

#[test]
fn mutex_wait_state_machine() {
    assert_eq!(ownership(WAIT_OBJECT_0), Ownership::Primary);
    assert_eq!(ownership(WAIT_ABANDONED), Ownership::Primary);
    assert_eq!(ownership(WAIT_TIMEOUT), Ownership::Secondary);
    assert_eq!(ownership(WAIT_FAILED), Ownership::Failed);
}
#[test]
fn isolated_kernel_mutex_and_event_lifecycle() {
    let name = format!(
        "Local\\Neo.Startup.Test.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else {
        panic!("primary expected")
    };
    let request = primary.request.0;
    let ack = primary.ack.0;
    let name2 = name.clone();
    // 竞争者只用内核原语，测试绝不创建通知或其他真实窗口。
    std::thread::spawn(move || unsafe {
        let mutex = handle(CreateMutexW(
            null(),
            0,
            wide(&format!("{name2}.mutex")).as_ptr(),
        ))
        .unwrap();
        assert_eq!(
            ownership(WaitForSingleObject(mutex.0 as HANDLE, 0)),
            Ownership::Secondary
        );
        assert_ne!(SetEvent(request as HANDLE), 0);
    })
    .join()
    .unwrap();
    assert!(poll(request, ack));
    assert!(!poll(request, ack));
    drop(primary);
    assert!(matches!(
        Instance::acquire(&name).unwrap(),
        Acquisition::Primary(_)
    ));
}
#[test]
fn concurrent_contenders_have_exactly_one_owner() {
    let name = format!(
        "Local\\Neo.Startup.Race.{}.{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let name = name.clone();
            let barrier = barrier.clone();
            let winners = winners.clone();
            std::thread::spawn(move || unsafe {
                let mutex = handle(CreateMutexW(null(), 0, wide(&name).as_ptr())).unwrap();
                barrier.wait();
                let result = ownership(WaitForSingleObject(mutex.0 as HANDLE, 0));
                assert_ne!(result, Ownership::Failed);
                if result == Ownership::Primary {
                    winners.fetch_add(1, Ordering::SeqCst);
                }
                barrier.wait();
                if result == Ownership::Primary {
                    ReleaseMutex(mutex.0 as HANDLE);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(winners.load(Ordering::SeqCst), 1);
}
#[test]
fn new_primary_acquires_while_old_contender_keeps_handles() {
    let name = format!("Local\\Neo.Startup.Retained.{}", std::process::id());
    let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else {
        panic!("primary expected")
    };
    unsafe {
        let observer = handle(CreateMutexW(
            null(),
            0,
            wide(&format!("{name}.mutex")).as_ptr(),
        ))
        .unwrap();
        let request = handle(CreateEventW(
            null(),
            0,
            0,
            wide(&format!("{name}.request")).as_ptr(),
        ))
        .unwrap();
        let ack = handle(CreateEventW(
            null(),
            0,
            0,
            wide(&format!("{name}.ack")).as_ptr(),
        ))
        .unwrap();
        SetEvent(request.0 as HANDLE);
        SetEvent(ack.0 as HANDLE);
        drop(primary);
        let Acquisition::Primary(next) = Instance::acquire(&name).unwrap() else {
            panic!("next primary expected")
        };
        assert!(!poll(next.request.0, next.ack.0));
        assert_eq!(WaitForSingleObject(ack.0 as HANDLE, 0), WAIT_TIMEOUT);
        drop(next);
        drop(observer);
    }
}
#[test]
fn abandoned_owner_can_be_recovered() {
    let name = format!("Local\\Neo.Startup.Abandon.{}", std::process::id());
    unsafe {
        let observer = handle(CreateMutexW(null(), 0, wide(&name).as_ptr())).unwrap();
        let raw = observer.0;
        std::thread::spawn(move || {
            assert_eq!(WaitForSingleObject(raw as HANDLE, 0), WAIT_OBJECT_0);
        })
        .join()
        .unwrap();
        assert_eq!(
            ownership(WaitForSingleObject(observer.0 as HANDLE, 0)),
            Ownership::Primary
        );
        assert_ne!(ReleaseMutex(observer.0 as HANDLE), 0);
    }
}
