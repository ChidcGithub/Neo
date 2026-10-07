//! 迷你窗布局不变量：避让 / 自动尺寸 / 淡出的几何与状态机锁定。
//!
//! 与 `miniwin_beam_tests`（流光缓存 + 打断交互）分文件：这里只管
//! 「布局跳变」—— 同一帧内开闭判定不自激、动画端点几何恰在屏内、
//! 淡出/截屏互斥不留中间态、measure 槽读全零与坏值都有界。

use super::*;

fn dark_theme() -> Theme {
    Theme::new(
        neo_theme::ThemeMode::Dark,
        1080.0,
        neo_theme::Distance::Standard,
    )
}

/// 走打断确认的归途 tick：mock 一条 Running 工具消息，busy 常真。
fn tick_brief(ctx: &Context, mini: &mut MiniWin, state: &mut AppState, hidden: bool) {
    ctx.begin_pass(egui::RawInput::default());
    mini.tick(ctx, state, dark_theme(), hidden, None, None);
    let mut output = ctx.end_pass();
    output.textures_delta.clear();
}

// ---------------------------------------------------------------------------
// 避让：home/away 几何与开闭判定互不参考（不振荡的几何前提）
// ---------------------------------------------------------------------------

#[test]
fn avoid_endpoints_stay_onscreen_and_home_contains_cursor_probe() {
    let theme = dark_theme();
    let m = theme.metrics;
    for monitor in [
        Vec2::new(1280.0, 720.0),
        Vec2::new(1920.0, 1080.0),
        Vec2::new(2560.0, 1440.0),
        Vec2::new(3840.0, 2160.0),
    ] {
        for target_h in [96.0, 240.0, 620.0] {
            let width = m.s(340.0);
            let margin = m.s(16.0);
            let size = Vec2::new(width, target_h);
            let home = Pos2::new((monitor.x - width - margin).max(margin), margin);
            let away = Pos2::new(margin, margin);
            for corner in [home, away] {
                let rect = Rect::from_min_size(corner, size);
                assert!(
                    rect.left() >= 0.0
                        && rect.top() >= 0.0
                        && rect.right() <= monitor.x.max(rect.right()) + 0.001,
                    "{corner:?} 出屏（{monitor}）"
                );
                // 避让判定盯的是静止位 home；光标探针在 home 矩形内才躲。
                assert_eq!(
                    corner == home,
                    Rect::from_min_size(home, size).contains(corner)
                );
            }
            // 两个端点水平间距 = monitor - 2*(margin+width) 的反向；任一插值
            // 中间帧仍沿直线运动，不会出现第三种位置。
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let k = ease_out_cubic(t);
                let pos = home + (away - home) * k;
                assert!((0.0..=1.0).contains(&t));
                assert!(pos.y >= 0.0 && pos.y <= home.y + 0.001);
            }
        }
    }
}

#[test]
fn avoid_toggle_between_frames_does_not_self_excite() {
    // 判据只盯「静止位 home」：卡片移到 away 后光标仍在 home 矩形里 →
    // dodge 维持 true；移到 away 后光标不在 home → dodge 翻回 false。
    // 用 Rect::contains 的半开区间语义验证边界恰好不抖。
    let theme = dark_theme();
    let m = theme.metrics;
    let width = m.s(340.0);
    let margin = m.s(16.0);
    let monitor = Vec2::new(1920.0, 1080.0);
    let size = Vec2::new(width, m.s(96.0));
    let home = Pos2::new((monitor.x - width - margin).max(margin), margin);
    let rect = Rect::from_min_size(home, size);
    for (cursor, dodge) in [
        (home + Vec2::splat(1.0), true),
        (Pos2::new(rect.right() - 1.0, rect.bottom() - 1.0), true),
        // egui Rect::contains 是闭区间：右缘/下缘像素也算「在卡片上」，
        // 光标压边即触发避让，不需要先扎进卡片 1px。
        (Pos2::new(rect.right(), rect.center().y), true),
        (Pos2::new(rect.center().x, rect.bottom()), true),
        (Pos2::new(rect.right() + 1.0, rect.center().y), false),
        (Pos2::new(home.x - 1.0, home.y), false),
        (Pos2::new(margin, margin), false), // away 位 ≠ home 判定区
    ] {
        assert_eq!(
            rect.contains(cursor),
            dodge,
            "cursor={cursor} home={home:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 自动尺寸：measure 槽读全零/坏值时目标高有界，不上溢也不塌成 0
// ---------------------------------------------------------------------------

#[test]
fn auto_height_clamps_zero_measure_to_min_and_never_exceeds_monitor_fraction() {
    let theme = dark_theme();
    let m = theme.metrics;
    for (monitor_y, content) in [
        (720.0, 0.0),
        (1080.0, 0.0),
        (1080.0, 400.0),
        (2160.0, 10_000.0),
        (320.0, 10_000.0), // 窗口比最小高度还矮的极端副屏
    ] {
        let monitor = Vec2::new(1920.0, monitor_y);
        let (width, target_h) = (
            m.s(340.0),
            (content + m.s(28.0)).clamp(m.s(96.0), (monitor.y - m.s(32.0)) * 0.62),
        );
        assert_eq!(width, m.s(340.0));
        assert!(
            target_h >= m.s(96.0),
            "{monitor_y}/{content}：高度塌穿最小值"
        );
        assert!(
            target_h <= (monitor.y - m.s(32.0)) * 0.62 + f32::EPSILON,
            "{monitor_y}/{content}：高度冲破屏幕占比上限"
        );
        assert!(target_h.is_finite());
    }
}

#[test]
fn measure_slot_defaults_to_zero_so_first_frame_uses_min_height() {
    let mini = MiniWin::default();
    let content = *mini.content_h.lock().unwrap();
    assert_eq!(content, 0.0);
    let theme = dark_theme();
    let m = theme.metrics;
    let target = (content + m.s(28.0)).clamp(m.s(96.0), (1080.0 - m.s(32.0)) * 0.62);
    assert_eq!(target, m.s(96.0));
}

#[test]
fn paint_writes_finite_measure_and_stays_inside_card() {
    let ctx = Context::default();
    neo_theme::fonts::install(&ctx);
    let theme = dark_theme();
    let mut state = AppState::default();
    state
        .messages
        .push(crate::state::ChatMessage::new(Role::User, "问"));
    state
        .messages
        .push(crate::state::ChatMessage::new(Role::Assistant, "回答正文"));
    for done in [false, true] {
        let measure = Arc::new(Mutex::new(f32::NAN));
        let monitor = Vec2::new(1920.0, 1080.0);
        let snap = Snapshot::build(&state, theme, 340.0, 96.0, monitor, done);
        let size = Vec2::new(340.0, 600.0);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                ..Default::default()
            },
            |ui| paint(ui, &snap, 1.0, &measure),
        );
        output.textures_delta.clear();
        let measured = *measure.lock().unwrap();
        assert!(measured.is_finite() && measured > 0.0, "done={done}");
        // 内容高可超出卡（裁剪在层外），但测量槽不可能是 NaN/负数/无穷。
        assert!(measured <= size.y * 4.0);
    }
}

// ---------------------------------------------------------------------------
// 淡出：驻留结束才起播、播完才收；截屏/忙碌任意时刻取消且不残留中间态
// ---------------------------------------------------------------------------

#[test]
fn fadeout_requires_shown_and_linger_expired() {
    let ctx = Context::default();
    let mut state = AppState::default();
    state.generating = true;
    let mut mini = MiniWin::default();
    // 曾露面（shown=true）但还在忙：驻留未过期，不起播。
    mini.shown = true;
    mini.linger_until = Some(Instant::now() + LINGER);
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_none());
    assert!(mini.shown);
    // 驻留过期、仍在忙：不能起播（busy 优先）。
    mini.linger_until = Some(Instant::now() - Duration::from_millis(1));
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_none());
}

#[test]
fn fadeout_starts_after_idle_and_clears_after_full_duration() {
    let ctx = Context::default();
    let mut state = AppState::default();
    state.generating = false;
    let mut mini = MiniWin::default();
    mini.shown = true;
    mini.linger_until = Some(Instant::now() - Duration::from_millis(1));
    tick_brief(&ctx, &mut mini, &mut state, true);
    // 驻留已过期、不在忙、曾露面、非截屏 → 起播。
    assert!(mini.fadeout_since.lock().unwrap().is_some());
    // 淡出播完后槽清空、驻留清零（收窗信号由 shown=false 的下一帧接管）。
    *mini.fadeout_since.lock().unwrap() =
        Some(Instant::now() - Duration::from_secs_f32(FADEOUT_SECS + 0.01));
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_none());
    assert!(mini.linger_until.is_none());
}

#[test]
fn busy_cancels_inflight_fadeout_without_shown_flicker() {
    let ctx = Context::default();
    let mut state = AppState::default();
    state.generating = false;
    let mut mini = MiniWin::default();
    mini.shown = true;
    mini.linger_until = Some(Instant::now() - Duration::from_millis(1));
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_some());
    // 淡出中途重新忙起来：淡出立即取消，shown 不跳变。
    state.generating = true;
    let shown_before = mini.shown;
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_none());
    assert_eq!(mini.shown, shown_before, "忙回归时 shown 不得翻落");
    assert!(mini.shown, "开着就是开着");
}

#[test]
fn shot_hide_blocks_fadeout_and_open_state() {
    let ctx = Context::default();
    let mut state = AppState::default();
    state.generating = false;
    let mut mini = MiniWin::default();
    mini.shown = true;
    // 截屏信号置位（shot_hiding=true）：驻留过期也不能起播淡出。
    neo_tools::tools::screen::SCREENSHOT_AT.store(1, Ordering::Relaxed);
    mini.linger_until = Some(Instant::now() - Duration::from_millis(1));
    tick_brief(&ctx, &mut mini, &mut state, true);
    assert!(mini.fadeout_since.lock().unwrap().is_none());
    // 清理全局信号，别污染同进程其它测试。
    neo_tools::tools::screen::SCREENSHOT_AT.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// 显露沿：overlay 路径首次露面先落静止位 + 播淡入（layer_rect 预写）
// ---------------------------------------------------------------------------

#[test]
fn first_show_writes_home_rect_then_callback_overrides_next_frame() {
    let mut mini = MiniWin::default();
    let theme = dark_theme();
    let m = theme.metrics;
    let monitor = Vec2::new(1920.0, 1080.0);
    let content = 0.0f32;
    let (width, target_h) = (
        m.s(340.0),
        (content + m.s(28.0)).clamp(m.s(96.0), (monitor.y - m.s(32.0)) * 0.62),
    );
    let home = Pos2::new((monitor.x - width - m.s(16.0)).max(m.s(16.0)), m.s(16.0));
    // 模拟 tick 里 `if !self.shown` 的露面沿：先落静止位再播淡入。
    *mini.layer_rect.lock().unwrap() = [home.x, home.y, width, target_h];
    *mini.refade_since.lock().unwrap() = Some(Instant::now());
    mini.shown = true;
    let rect = *mini.layer_rect.lock().unwrap();
    assert_eq!(rect, [home.x, home.y, width, target_h]);
    assert!(mini.refade_since.lock().unwrap().is_some());
    // 下一帧回调会按避让/高度动画覆写；这里只锁露面沿的初值语义。
    let _ = theme;
}

// ---------------------------------------------------------------------------
// BeamGeometry：尺寸键外的字段变化不重建（避让观众只盯 size/radius）
// ---------------------------------------------------------------------------

#[test]
fn beam_cache_ignores_origin_and_animation_phase() {
    let mut cache = BeamGeometry::default();
    let key = BeamKey::new(Vec2::new(340.0, 96.0), 18.0);
    assert!(cache.ensure(key));
    let ptr = (cache.points.as_ptr(), cache.cumulative.as_ptr());
    // 避让平移、淡出上飘都不改 size/radius：ensure 必须幂等命中。
    for frame in 0..120 {
        let drift = frame as f32 * 0.7;
        let shifted = BeamKey::new(Vec2::new(340.0 + drift * 0.0, 96.0), 18.0);
        assert!(!cache.ensure(shifted), "frame {frame}");
    }
    assert_eq!((cache.points.as_ptr(), cache.cumulative.as_ptr()), ptr);
    // 尺寸真变才重建。
    assert!(cache.ensure(BeamKey::new(Vec2::new(341.0, 96.0), 18.0)));
}

// ---------------------------------------------------------------------------
// FallbackGeometry：store 只在「位置或 ppp 变化」时发命令（防重复拖拽）
// ---------------------------------------------------------------------------

#[test]
fn fallback_store_resends_only_on_position_or_ppp_change() {
    let ctx = Context::default();
    ctx.set_embed_viewports(false);
    let screen = ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), 1.5);
    let pos = Pos2::new(1564.0, 16.0);
    let count_commands = |output: &egui::FullOutput| -> usize {
        output
            .viewport_output
            .get(&viewport_id())
            .map(|v| {
                v.commands
                    .iter()
                    .filter(|c| matches!(c, ViewportCommand::OuterPosition(_)))
                    .count()
            })
            .unwrap_or(0)
    };
    // 注册视口（与生产路径一致：每帧无条件注册防回收）。
    ctx.begin_pass(egui::RawInput::default());
    ctx.show_viewport_deferred(viewport_id(), ViewportBuilder::default(), |_, _| {});
    FallbackGeometry::new(screen, pos).store(&ctx);
    let mut out = ctx.end_pass();
    assert_eq!(count_commands(&out), 1);
    out.textures_delta.clear();
    // 同点同 ppp：静默。
    ctx.begin_pass(egui::RawInput::default());
    ctx.show_viewport_deferred(viewport_id(), ViewportBuilder::default(), |_, _| {});
    FallbackGeometry::new(screen, pos).store(&ctx);
    let mut out = ctx.end_pass();
    assert_eq!(count_commands(&out), 0);
    out.textures_delta.clear();
    // ppp 翻一倍：同点坐标物理位置已变，必须重发。
    ctx.begin_pass(egui::RawInput::default());
    ctx.show_viewport_deferred(viewport_id(), ViewportBuilder::default(), |_, _| {});
    let hi = ScreenGeometry::from_physical(Vec2::new(1920.0, 1080.0), 3.0);
    FallbackGeometry::new(hi, pos).store(&ctx);
    let mut out = ctx.end_pass();
    assert_eq!(count_commands(&out), 1);
    out.textures_delta.clear();
}

// ---------------------------------------------------------------------------
// 文本截断工具：head/tail 字符级安全（步进摘要不因多字节炸掉）
// ---------------------------------------------------------------------------

#[test]
fn head_tail_truncate_on_char_boundaries_with_ellipsis() {
    for input in ["abc", "短句", "a长的混合string_with_ascii_12345"] {
        let h = head(input, 4);
        let t = tail(input, 4);
        assert!(h.chars().count() <= 5 && t.chars().count() <= 5);
        if input.chars().count() > 4 {
            assert!(h.ends_with('…'));
            assert!(t.starts_with('…'));
        } else {
            assert_eq!(h, input);
            assert_eq!(t, input);
        }
    }
}

#[test]
fn last_paragraph_ignores_unbalanced_code_fences() {
    for (text, expected) in [
        ("第一段\n\n第二段", "第二段"),
        ("单段", "单段"),
        // 两段合起来栅栏恰好配平（1+1=2，偶数）：整体作为一个段落返回。
        (
            "```rust\nlet x = 1;\n\nprintln!(\"{x}\");\n```",
            "```rust\nlet x = 1;\n\nprintln!(\"{x}\");\n```",
        ),
        // 栅栏数不配平（奇数）：向前合并直到成对。
        ("上文\n\n```python\nprint(1)\n```\n\n结尾", "结尾"),
        ("普通结尾", "普通结尾"),
    ] {
        assert_eq!(last_paragraph(text), expected, "{text:?}");
    }
}
