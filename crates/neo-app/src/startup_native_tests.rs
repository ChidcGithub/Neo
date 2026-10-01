
        use super::*;
        // 几何、动画和像素测试为纯逻辑；降级区域测试只创建内存 GDI 区域，不创建窗口。
        mod rectangular {
            use super::*;

            #[test]
            fn layout_scales_without_overlap() {
                for scale in [1., 1.25, 1.5, 2., 3.] {
                    let card = layout(scale);
                    assert_eq!(card.width, (220. * scale).round() as i32);
                    assert_eq!(card.height, (112. * scale).round() as i32);
                    assert!(card.width > card.height);
                    assert!(card.width * card.height < (236. * scale).powi(2) as i32 / 2);
                    assert!(card.brand.bottom < card.caption.top);
                    assert!(card.caption.bottom < card.progress.top);
                    assert_eq!((card.progress.left, card.progress.right, card.progress.bottom),
                        (0, card.width, card.height));
                    assert_eq!(card.progress.bottom - card.progress.top, (3. * scale).round() as i32);
                    for r in [card.brand, card.caption, card.progress] {
                        assert!(r.left >= 0 && r.top >= 0 && r.right <= card.width && r.bottom <= card.height);
                        assert!(r.left < r.right && r.top < r.bottom);
                    }
                }
            }

            #[test]
            fn backdrop_uses_system_colors_when_required() {
                assert_eq!(backdrop_choice(true, false), Backdrop::Opaque);
                for (readable, contrast) in [(false, false), (false, true), (true, true)] {
                    assert_eq!(backdrop_choice(readable, contrast), Backdrop::HighContrast);
                }
            }

            #[test]
            fn square_corners_and_full_width_bottom_track_are_opaque() {
                let palette = Palette::light();
                for scale in [1., 1.25, 1.5, 2., 3.] {
                    let g = layout(scale);
                    let segment = progress_segment(&g, 0.);
                    for duplicate in [false, true] {
                        let pixel = |x, y| card_pixel(&g, x, y, 0, &segment, duplicate, &palette);
                        for x in [0, g.width - 1] {
                            for y in [0, g.height - 1] { assert_eq!(pixel(x, y) >> 24, 255); }
                        }
                        assert_eq!(pixel(g.width / 2, g.height / 2), 0xfff8faff);
                        for (x, y) in [(-1, 0), (0, -1), (g.width, 0), (0, g.height)] {
                            assert_eq!(pixel(x, y), 0);
                        }
                        for y in 0..g.height {
                            for x in 0..g.width {
                                let value = pixel(x, y);
                                assert_eq!(value >> 24, 255);
                                let color = if y < g.progress.top { palette.body }
                                    else if !duplicate && x < segment.right { palette.brand }
                                    else { palette.track };
                                assert_eq!(value, premultiplied(color, 1.));
                                assert_eq!(opaque_pixel(value, palette.body), value);
                            }
                        }

                    }
                }
            }

            #[test]
            fn glyph_coverage_is_clear_and_premultiplied() {
                let g = layout(1.);
                let palette = Palette::light();
                let segment = progress_segment(&g, 0.);
                for coverage in 0..=255 {
                    for (x, y, ink) in [(110, 37, palette.brand), (110, 78, palette.caption)] {
                        let value = card_pixel(&g, x, y, coverage, &segment, false, &palette);
                        for (shift, index) in [(16, 0), (8, 1), (0, 2)] {
                            let expected = (ink[index] as u32 * coverage as u32
                                + palette.body[index] as u32 * (255 - coverage as u32) + 127) / 255;
                            assert_eq!((value >> shift) & 255, expected);
                        }
                        assert_eq!(value >> 24, 255);
                    }
                    for alpha in 0..=255 {
                        let value = premultiplied([coverage, 107, 254], alpha as f32 / 255.);
                        assert_eq!(value >> 24, alpha);
                        for shift in [0, 8, 16] { assert!((value >> shift) & 255 <= alpha); }
                    }
                }
                assert_eq!(card_pixel(&g, 110, 37, 255, &segment, false, &palette), 0xff4d6bfe);
                assert_eq!(card_pixel(&g, 110, 78, 255, &segment, false, &palette), 0xff465067);
                assert_eq!(opaque_pixel(0, palette.body), 0xfff8faff);
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
                            assert_eq!(card_pixel(&g, x, y, 0, &first_segment, true, &palette),
                                card_pixel(&g, x, y, 0, &next_segment, true, &palette));
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
                    assert_eq!(first.left, 0);
                    assert_eq!(progress_segment(&g, std::f32::consts::PI).right, g.width);
                    assert_eq!(progress_segment(&g, std::f32::consts::TAU).left, 0);
                    for step in 0..=1000 {
                        let segment = progress_segment(&g, animation_phase(step as f32 * 0.04, false));
                        assert!(segment.left >= 0 && segment.right <= g.width);
                        assert_eq!(segment.right - segment.left, first.right - first.left);
                        assert_eq!((segment.top, segment.bottom), (g.progress.top, g.height));
                    }
                }
            }

            #[test]
            fn fallback_region_is_a_full_rectangle() {
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
                            for y in [0, g.height - 1] { assert_ne!(PtInRegion(region, x, y), 0); }
                        }
                        let mut bounds: RECT = std::mem::zeroed();
                        assert_eq!(GetRgnBox(region, &mut bounds), SIMPLEREGION);
                        assert_eq!((bounds.left, bounds.top, bounds.right, bounds.bottom),
                            (0, 0, g.width, g.height));
                        assert_ne!(DeleteObject(region), 0);
                    }
                }
            }
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
            let name = format!("Local\\Neo.Startup.Test.{}.{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
            let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else { panic!("primary expected") };
            let request = primary.request.0;
            let ack = primary.ack.0;
            let name2 = name.clone();
            // 竞争者只用内核原语，测试绝不创建通知或其他真实窗口。
            std::thread::spawn(move || unsafe {
                let mutex = handle(CreateMutexW(null(), 0, wide(&format!("{name2}.mutex")).as_ptr())).unwrap();
                assert_eq!(ownership(WaitForSingleObject(mutex.0 as HANDLE, 0)), Ownership::Secondary);
                assert_ne!(SetEvent(request as HANDLE), 0);
            }).join().unwrap();
            assert!(poll(request, ack));
            assert!(!poll(request, ack));
            drop(primary);
            assert!(matches!(Instance::acquire(&name).unwrap(), Acquisition::Primary(_)));
        }
        #[test]
        fn concurrent_contenders_have_exactly_one_owner() {
            let name = format!("Local\\Neo.Startup.Race.{}.{}", std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
            let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let threads: Vec<_> = (0..8).map(|_| {
                let name = name.clone();
                let barrier = barrier.clone();
                let winners = winners.clone();
                std::thread::spawn(move || unsafe {
                    let mutex = handle(CreateMutexW(null(), 0, wide(&name).as_ptr())).unwrap();
                    barrier.wait();
                    let result = ownership(WaitForSingleObject(mutex.0 as HANDLE, 0));
                    assert_ne!(result, Ownership::Failed);
                    if result == Ownership::Primary { winners.fetch_add(1, Ordering::SeqCst); }
                    barrier.wait();
                    if result == Ownership::Primary { ReleaseMutex(mutex.0 as HANDLE); }
                })
            }).collect();
            for thread in threads { thread.join().unwrap(); }
            assert_eq!(winners.load(Ordering::SeqCst), 1);
        }
        #[test]
        fn new_primary_acquires_while_old_contender_keeps_handles() {
            let name = format!("Local\\Neo.Startup.Retained.{}", std::process::id());
            let Acquisition::Primary(primary) = Instance::acquire(&name).unwrap() else { panic!("primary expected") };
            unsafe {
                let observer = handle(CreateMutexW(null(), 0, wide(&format!("{name}.mutex")).as_ptr())).unwrap();
                let request = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.request")).as_ptr())).unwrap();
                let ack = handle(CreateEventW(null(), 0, 0, wide(&format!("{name}.ack")).as_ptr())).unwrap();
                SetEvent(request.0 as HANDLE);
                SetEvent(ack.0 as HANDLE);
                drop(primary);
                let Acquisition::Primary(next) = Instance::acquire(&name).unwrap() else { panic!("next primary expected") };
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
                std::thread::spawn(move || { assert_eq!(WaitForSingleObject(raw as HANDLE, 0), WAIT_OBJECT_0); }).join().unwrap();
                assert_eq!(ownership(WaitForSingleObject(observer.0 as HANDLE, 0)), Ownership::Primary);
                assert_ne!(ReleaseMutex(observer.0 as HANDLE), 0);
            }
        }
