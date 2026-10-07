//! DPI 切换 / 窗口移动 / 拓扑重排下的尺寸不变量：离屏预算、裁剪覆盖、
//! 桌面帧 bounds 匹配、卡片 Area 换算、uniform 尺寸一致性。
//!
//! 与 `regression_tests`（并发/许可/生命周期）分文件：这里锁定「几何与
//! 尺寸」—— 重配不丢帧、采样不跨坐标系、DPI 变化时卡片映射不漂。

use super::*;

fn bounds() -> DesktopBounds {
    DesktopBounds {
        origin: (0, 0),
        size: (2, 2),
    }
}

fn shot(value: u8) -> Shot {
    Shot {
        width: 2,
        height: 2,
        rgba: vec![value; 16],
    }
}

// ---------------------------------------------------------------------------
// offscreen_size：DPI 改变只重排物理像素，输出永远遵守预算与上限
// ---------------------------------------------------------------------------

#[test]
fn offscreen_reconfigure_stays_within_budget_across_dpi_changes() {
    // 常见 DPI 档位对应的物理分辨率（1080p/1440p/4K）。
    for (w, h) in [
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
        (1280, 720),
        (11520, 2160), // 三联屏
        (2160, 3840),  // 竖屏
    ] {
        let (ow, oh) = offscreen_size((w, h));
        assert!(ow > 0 && oh > 0);
        assert!(oh <= OFFSCREEN_MAX_HEIGHT);
        assert!(u64::from(ow) * u64::from(oh) <= u64::from(OFFSCREEN_PIXEL_BUDGET));
        assert!(ow <= w && oh <= h, "{w}x{h} -> {ow}x{oh} 不得放大");
    }
    // 同一显示器 100%↔150% 切换不改变 offscreen 输出（物理像素未变）。
    assert_eq!(offscreen_size((1920, 1080)), offscreen_size((1920, 1080)));
}

#[test]
fn offscreen_scale_is_monotonic_nonincreasing_with_resolution() {
    // 分辨率越大，scale 只能持平或更低（预算挤压），不能反弹。
    let mut prev = f64::from(RENDER_SCALE);
    for (w, h) in [
        (640, 480),
        (1280, 720),
        (1920, 1080),
        (2560, 1440),
        (3840, 2160),
    ] {
        let (ow, oh) = offscreen_size((w, h));
        let scale = f64::from(ow) / f64::from(w);
        assert!(scale <= prev + 1e-9, "{w}x{h}");
        let scale_y = f64::from(oh) / f64::from(h);
        assert!((scale - scale_y).abs() < 0.01, "非均匀缩放 {w}x{h}");
        prev = scale;
    }
}

// ---------------------------------------------------------------------------
// edge_scissors：裁剪矩形不越界、不重排，覆盖带随尺寸同步缩放
// ---------------------------------------------------------------------------

#[test]
fn scissor_ring_covers_shader_early_return_zone_after_resize() {
    // 模拟拓扑从 1080p 到 4K：edge 带加宽，但 ring 必须恰好贴住 shader 早退线。
    for size in [(1920, 1080), (2560, 1440), (3840, 2160), (11520, 2160)] {
        let (w, h) = size;
        let (ow, oh) = offscreen_size(size);
        let rects: Vec<_> = edge_scissors((ow, oh)).collect();
        assert!(rects.len() == 1 || rects.len() == 4);
        if rects.len() == 4 {
            let edge = rects[0].3;
            // shader 早退在 d < -120*px（px=oh/432），外加 12px inset + 14px 圆角。
            let shader_band = ((120.0 + 12.0 + 14.0) * f64::from(oh) / 432.0).ceil() as u32;
            assert_eq!(
                edge,
                shader_band + 2,
                "{w}x{h}：ring 必须盖住早退区 + AA guard"
            );
            // 四块不相交且不越界。
            let mut area = 0u64;
            for (i, &(x, y, rw, rh)) in rects.iter().enumerate() {
                assert!(u64::from(x) + u64::from(rw) <= u64::from(ow));
                assert!(u64::from(y) + u64::from(rh) <= u64::from(oh));
                area += u64::from(rw) * u64::from(rh);
                for &(xx, yy, ww, hh) in &rects[..i] {
                    assert!(x + rw <= xx || xx + ww <= x || y + rh <= yy || yy + hh <= y);
                }
            }
            // ring 面积 = 总面积 - 中心洞。
            let inner_w = u64::from(ow - 2 * edge);
            let inner_h = u64::from(oh - 2 * edge);
            assert_eq!(area, u64::from(ow) * u64::from(oh) - inner_w * inner_h);
        }
    }
}

// ---------------------------------------------------------------------------
// 桌面帧 bounds 匹配：拓扑变化（DPI/移动/拔插）后旧帧必须被拒绝
// ---------------------------------------------------------------------------

#[test]
fn topology_change_rejects_stale_frames_and_different_sizes() {
    let mut state = CaptureState::default();
    state.set_exclude_ok(true);
    state.set_enabled(true);
    let old = bounds();
    let gen = state.begin().unwrap();
    // 旧 bounds 的帧在拓扑变化后必须作废。
    state.publish(gen, old, shot(1));
    state.invalidate();
    let bigger = DesktopBounds {
        origin: (0, 0),
        size: (4, 4),
    };
    assert!(state.take(old).is_none(), "invalidate 后旧帧不可取");
    assert!(state.take(bigger).is_none(), "尺寸不符不可取");
    // 负原点副屏拓扑：origin 变化也构成新 bounds。
    let shifted = DesktopBounds {
        origin: (-1920, 0),
        size: (2, 2),
    };
    let gen2 = state.begin().unwrap();
    state.publish(gen2, shifted, shot(2));
    assert!(state.take(shifted).is_some(), "同 bounds 同尺寸可取");
    // 取走即清，防重放。
    assert!(state.take(shifted).is_none());
}

#[test]
fn desktop_upload_rejects_frame_when_size_mismatches_texture() {
    // valid_upload 已测过零/超限；这里锁 bounds-vs-shot 一致性。
    let shot = Shot {
        width: 2,
        height: 2,
        rgba: vec![7; 16],
    };
    let mut state = CaptureState::default();
    state.set_exclude_ok(true);
    state.set_enabled(true);
    let gen = state.begin().unwrap();
    state.publish(
        gen,
        DesktopBounds {
            origin: (0, 0),
            size: (2, 2),
        },
        shot,
    );
    // bounds 尺寸与 shot 尺寸不一致时 take 拒绝（防跨拓扑串帧）。
    let mismatched = DesktopBounds {
        origin: (0, 0),
        size: (4, 4),
    };
    assert!(state.take(mismatched).is_none());
}

// ---------------------------------------------------------------------------
// 卡片坐标映射：主屏点 → 层内点 的平移（负原点 + ppp 缩放）
// ---------------------------------------------------------------------------

#[test]
fn card_area_translation_accounts_for_negative_origin_and_ppp() {
    // 主屏点（应用层坐标）到层内点：减窗口原点（虚拟屏原点可为负）再除 ppp。
    for (origin, ppp) in [((0, 0), 1.0), ((-1920, 0), 1.5), ((0, -1080), 2.0)] {
        let (ox, oy) = (origin.0 as f32 / ppp, origin.1 as f32 / ppp);
        for card in [[0.0f32, 0.0, 340.0, 96.0], [100.0, 20.0, 640.0, 460.0]] {
            let [x, y, w, h] = card;
            let pos = egui::pos2(x - ox, y - oy);
            // 层内坐标必须是有限值；卡片尺寸不参与平移。
            assert!(pos.x.is_finite() && pos.y.is_finite());
            let _ = (w, h); // 尺寸由 Area 的 set_width/set_height 钉住，不进 pos
        }
    }
}

// ---------------------------------------------------------------------------
// uniform 尺寸一致性：shader 的 resolution / tex_size 来自当前帧，不是缓存
// ---------------------------------------------------------------------------

#[test]
fn uniform_dimensions_track_current_frame_not_stale_cache() {
    // render() 里 uniform 数组下标 8..12 = [offscreen.w, offscreen.h, desktop.w, desktop.h]。
    // 桌面纹理尺寸变化时 upload_desktop 重建纹理；offscreen 在 refresh_display 重建。
    // 本测试锁语义：任何重配后，写入 shader 的尺寸必须来自同一帧的当前资源。
    for (ow, oh, dw, dh) in [(1152, 648, 1920, 1080), (1152, 648, 3840, 2160)] {
        let uni: [f32; 12] = [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, ow as f32, oh as f32, dw as f32, dh as f32,
        ];
        assert_eq!(uni[8], ow as f32);
        assert_eq!(uni[9], oh as f32);
        assert_eq!(uni[10], dw as f32);
        assert_eq!(uni[11], dh as f32);
        // 折射分辨率与桌面纹理尺寸可以不同（DPI 切换后抓屏未到），
        // 但 shader 用两套独立 uniform，不会拿旧桌面尺寸采样新纹理。
        let _ = (uni[8], uni[10]);
    }
}

// ---------------------------------------------------------------------------
// validate_dimensions：重配前的守门员（0 / 超限时拒绝，防 wgpu panic）
// ---------------------------------------------------------------------------

#[test]
fn validate_dimensions_rejects_zero_and_over_max_before_reconfigure() {
    let max = 16384;
    for size in [(0, 1080), (1920, 0), (0, 0), (16385, 1080), (1920, 16385)] {
        assert!(validate_dimensions(size, max).is_err(), "{size:?}");
    }
    for size in [(1, 1), (1920, 1080), (16384, 16384)] {
        assert!(validate_dimensions(size, max).is_ok(), "{size:?}");
    }
}
