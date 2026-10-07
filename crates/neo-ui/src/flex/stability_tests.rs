//! 跨帧尺寸稳定性的 headless 测试。
//!
//! 背景（见 `mod.rs` 文件头）：上游 egui_flex 对上一帧记忆的 item 尺寸做精确
//! f32 相等比较。分数倍缩放下 egui 的像素对齐（`GUI_ROUNDING = 1/32`）会让同一
//! 控件的实测尺寸在相邻量化档之间抖动，精确比较永远不相等 → 每帧
//! `request_discard` → 布局双跑、egui 刷 PERF WARNING。
//! 本地修复：[`state_materially_changed`] 对所有"实测/缓存"尺寸字段用
//! `SIZE_EPSILON` 容差比较；身份与配置字段（id / content_id / grow / basis /
//! shrink / margin / 项目数）仍精确比较 —— 真实内容变化不会被吞。

use super::*;

// ---------- 单元测试：state_materially_changed 的容差语义 ----------

fn item_state(id: &str, size: Vec2) -> ItemState {
    ItemState {
        id: Id::new(id),
        config: FlexItemState::default(),
        inner_size: size,
        inner_min_size: size,
    }
}

fn state_with_items(items: Vec<ItemState>) -> FlexState {
    FlexState {
        items,
        max_item_size: Vec2::new(400.0, 300.0),
        frame_time: 0.0,
        passes: 0,
        shrunk_item_cross_size: None,
    }
}

#[test]
fn sub_pixel_size_jitter_is_absorbed() {
    let prev = state_with_items(vec![item_state("a", Vec2::new(80.0, 23.8125))]);
    // 文档里实测的抖动对：23.8125 ↔ 23.78125（恰好一个 GUI_ROUNDING 档）
    let next = state_with_items(vec![item_state("a", Vec2::new(80.0, 23.78125))]);
    assert!(
        !state_materially_changed(&prev, &next),
        "1/32pt 量化档抖动应被容差吸收"
    );

    // 容差边界内侧（< SIZE_EPSILON）两个方向都不触发
    let next = state_with_items(vec![item_state("a", Vec2::new(80.4, 23.4))]);
    assert!(!state_materially_changed(&prev, &next));
}

#[test]
fn real_size_changes_still_trigger_relayout() {
    let prev = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);
    // 文本变长：宽度 +2pt，远超容差，必须触发
    let mut next = state_with_items(vec![item_state("a", Vec2::new(82.0, 24.0))]);
    assert!(
        state_materially_changed(&prev, &next),
        ">1pt 的真实尺寸变化必须触发重排"
    );
    // 高度方向同理
    next.items[0].inner_size = Vec2::new(80.0, 24.0);
    next.items[0].inner_min_size = Vec2::new(80.0, 25.5);
    assert!(state_materially_changed(&prev, &next));
}

#[test]
fn identity_and_config_changes_are_exact() {
    let prev = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);

    // 增删项
    let mut next = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);
    next.items.push(item_state("b", Vec2::new(10.0, 10.0)));
    assert!(state_materially_changed(&prev, &next), "增项必须触发");

    // id 变化
    let next = state_with_items(vec![item_state("b", Vec2::new(80.0, 24.0))]);
    assert!(state_materially_changed(&prev, &next), "换 id 必须触发");

    // content_id 变化（真实场景：模型名变了，宽度跟着变）
    let mut next = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);
    next.items[0].config.content_id = Some(Id::new("model-x"));
    assert!(state_materially_changed(&prev, &next));

    // grow / margin 等配置变化：再小也必须触发（配置没有"亚像素抖动"一说）
    let mut next = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);
    next.items[0].config.grow = Some(0.001);
    assert!(state_materially_changed(&prev, &next));
    let mut next = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);
    next.items[0].config.margin = Margin::same(1);
    assert!(state_materially_changed(&prev, &next));
}

#[test]
fn cached_container_sizes_use_epsilon_too() {
    let prev = state_with_items(vec![item_state("a", Vec2::new(80.0, 24.0))]);

    // max_item_size / shrunk_item_cross_size 同样是跨帧缓存的实测值，
    // 亚像素抖动也不该触发重排。
    let mut next = prev.clone();
    next.max_item_size = Vec2::new(400.4, 300.4);
    assert!(
        !state_materially_changed(&prev, &next),
        "max_item_size 亚像素抖动应被吸收"
    );
    next.max_item_size = Vec2::new(402.0, 300.0);
    assert!(
        state_materially_changed(&prev, &next),
        "max_item_size 真实变化（窗口拉伸）必须触发"
    );

    let mut prev_shrunk = prev.clone();
    prev_shrunk.shrunk_item_cross_size = Some(30.0);
    let mut next = prev_shrunk.clone();
    next.shrunk_item_cross_size = Some(30.4);
    assert!(
        !state_materially_changed(&prev_shrunk, &next),
        "shrunk_item_cross_size 亚像素抖动应被吸收"
    );
    next.shrunk_item_cross_size = Some(32.0);
    assert!(state_materially_changed(&prev_shrunk, &next));
    next.shrunk_item_cross_size = None;
    assert!(
        state_materially_changed(&prev_shrunk, &next),
        "Some→None 是结构性变化，必须触发"
    );
}

// ---------- 集成测试：headless Context 跑真实帧，数每帧 pass 数 ----------
//
// `run_ui` 内部做多 pass 布局：内容稳定时 1 个 pass 收敛；
// flex 调 `request_discard` 时会再跑一个 pass（默认 max_passes=2）。
// 所以 `num_completed_passes == 1` 就是「这一帧没有触发 discard」的精确信号。

/// 测量尺寸以 `jitter` 幅度逐帧交替的内容 —— 模拟分数倍缩放下
/// 同一内容在亚像素网格上的实测抖动（内容本身从未改变）。
fn alternating_content(flex: &mut FlexInstance, frame: u64, base: Vec2, jitter: f32) {
    let wobble = if frame.is_multiple_of(2) { 0.0 } else { jitter };
    flex.add_ui(item(), move |ui| {
        ui.allocate_exact_size(base + Vec2::splat(wobble), Sense::hover())
    });
    flex.add_ui(item(), |ui| ui.button("按钮"));
    flex.grow();
    flex.add_ui(item(), |ui| ui.button("尾"));
}

fn run_frames(ctx: &egui::Context, frames: u64, jitter: f32) -> Vec<(usize, bool)> {
    let mut log = Vec::new();
    for frame in 0..frames {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            Flex::horizontal()
                .id_salt("stability-flex")
                .align_items(FlexAlign::Center)
                .w_full()
                .show(ui, |flex| {
                    alternating_content(flex, frame, Vec2::new(60.0, 24.0), jitter);
                });
        });
        log.push((
            out.platform_output.num_completed_passes,
            out.platform_output.requested_discard(),
        ));
        out.textures_delta.clear();
    }
    log
}

#[test]
fn sub_pixel_jitter_does_not_request_discard_at_fractional_zoom() {
    // 1.25× 是用户实测出问题的分数倍缩放
    for zoom in [1.25_f32, 0.85, 1.0] {
        let ctx = egui::Context::default();
        ctx.set_zoom_factor(zoom);
        // 0.3pt < SIZE_EPSILON：同内容的量化档抖动
        let log = run_frames(&ctx, 6, 0.3);
        // 首帧初始化允许一次 discard（first frame settle）；
        // 之后每帧都必须单 pass 收敛，且最终一帧没有遗留 discard 请求。
        for (frame, (passes, _)) in log.iter().enumerate().skip(1) {
            assert_eq!(
                *passes, 1,
                "zoom={zoom} frame={frame}: 亚像素抖动触发了 request_discard（log={log:?}）"
            );
        }
        let (_, last_discard) = log.last().unwrap();
        assert!(
            !last_discard,
            "zoom={zoom}: 稳态下仍在请求 discard（无限循环信号）"
        );
    }
}

#[test]
fn real_content_change_still_requests_discard() {
    let ctx = egui::Context::default();
    ctx.set_zoom_factor(1.25);
    // 2pt > 1pt：真实内容变化
    let log = run_frames(&ctx, 4, 2.0);
    // 交替变化期间必须持续触发 discard（每帧 2 pass）
    for (frame, (passes, _)) in log.iter().enumerate().skip(1) {
        assert!(
            *passes >= 2,
            "zoom=1.25 frame={frame}: >1pt 的真实变化被容差吞掉了（log={log:?}）"
        );
    }
}

#[test]
fn steady_content_converges_after_first_frame() {
    // 完全不动的内容：首帧之后永远单 pass
    for zoom in [1.25_f32, 0.85, 1.0, 2.0] {
        let ctx = egui::Context::default();
        ctx.set_zoom_factor(zoom);
        let log = run_frames(&ctx, 5, 0.0);
        for (frame, (passes, discard)) in log.iter().enumerate().skip(1) {
            assert_eq!(
                (*passes, *discard),
                (1, false),
                "zoom={zoom} frame={frame}: 静态内容未收敛（log={log:?}）"
            );
        }
    }
}
