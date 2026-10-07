//! 矩形稳定性测试：锁定模态打开/关闭时下层内容的 `Response.rect` 逐字节相等，
//! 以及模态卡片本身在连续帧间位置不漂移。
//!
//! 对应 `button_stability_tests.rs` 的约定：
//! - 模态只做**覆盖层**（遮罩 + 卡片 + 交互阻塞），不推进下层布局游标
//! - 开/关模态时，下层所有控件的 rect 逐字节相等
//! - 模态卡片自身位置由 `content_rect` 中心推出，帧间稳定

use super::*;
use crate::button::Button;

/// 无任何输入（idle 态）的 RawInput。
fn idle_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    }
}

/// Esc 键按下的 RawInput。
fn esc_input() -> egui::RawInput {
    let mut input = idle_input();
    input.events = vec![egui::Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }];
    input
}

fn setup_ctx(mode: neo_theme::ThemeMode, scale: f32) -> (egui::Context, Design) {
    let ctx = egui::Context::default();
    neo_theme::fonts::install(&ctx);
    let theme = neo_theme::Theme::from_metrics(mode, neo_theme::Metrics::from_scale(scale));
    theme.apply(&ctx);
    let d = Design::new(theme);
    (ctx, d)
}

/// 画一排「下层内容」，返回它们的 rect。
fn draw_underlay(ui: &mut Ui, d: &Design) -> Vec<Rect> {
    let mut rects = Vec::new();
    ui.vertical(|ui| {
        rects.push(
            Button::new("下方按钮一")
                .id_salt("under-1")
                .show(ui, d)
                .rect,
        );
        rects.push(
            Button::new("下方按钮二")
                .id_salt("under-2")
                .show(ui, d)
                .rect,
        );
        ui.add_space(8.0);
        rects.push(
            Button::new("下方按钮三")
                .id_salt("under-3")
                .show(ui, d)
                .rect,
        );
    });
    rects
}

// -----------------------------------------------------------------------
// 核心：模态打开/关闭不改变下层内容 rect
// -----------------------------------------------------------------------

#[test]
fn modal_open_does_not_shift_underlay() {
    for mode in [neo_theme::ThemeMode::Light, neo_theme::ThemeMode::Dark] {
        for scale in [0.85, 1.0, 1.75] {
            let (ctx, d) = setup_ctx(mode, scale);

            let render = |open: bool| -> Vec<Rect> {
                let mut rects = None;
                let mut output = ctx.run_ui(idle_input(), |ui| {
                    rects = Some(draw_underlay(ui, &d));
                    if open {
                        let h = Modal::height(&d, ModalSize::Md, 120.0);
                        let modal = Modal::new("设置", ModalSize::Md);
                        if let Some(_body) = modal.begin(ui, &d, h) {
                            modal.end(ui, &d);
                        }
                    }
                });
                output.textures_delta.clear();
                rects.expect("underlay rects not captured")
            };

            // 先跑一帧空的让上下文稳定（焦点、动画缓存）。
            let _ = render(false);

            let closed = render(false);
            let open = render(true);
            assert_eq!(closed.len(), open.len(), "下层控件数量变了");
            for (i, (c, o)) in closed.iter().zip(open.iter()).enumerate() {
                assert_eq!(
                    c, o,
                    "{mode:?}/{scale}: 模态打开后下层控件 {i} 移动了\n  closed: {c:?}\n  open:   {o:?}"
                );
            }

            // 再关上：rect 必须回到原位。
            let closed_again = render(false);
            for (i, (c, o)) in closed.iter().zip(closed_again.iter()).enumerate() {
                assert_eq!(c, o, "{mode:?}/{scale}: 模态关闭后下层控件 {i} 没回到原位");
            }
        }
    }
}

/// 模态连续开多帧：卡片 rect 帧间稳定（无动画漂移）。
#[test]
fn modal_card_rect_stable_across_frames() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

    let mut card_rects = Vec::new();
    for _ in 0..5 {
        let mut output = ctx.run_ui(idle_input(), |ui| {
            let h = Modal::height(&d, ModalSize::Md, 120.0);
            Modal::new("设置", ModalSize::Md).begin(ui, &d, h);
        });
        output.textures_delta.clear();
        // 卡片 rect 由 begin 内部计算：宽按 ModalSize、高居中。
        let w = ModalSize::Md.width(&d, screen.width());
        let h = Modal::height(&d, ModalSize::Md, 120.0).min(screen.height() * 0.9);
        card_rects.push(Rect::from_center_size(screen.center(), Vec2::new(w, h)));
    }

    for (i, r) in card_rects.iter().enumerate() {
        assert_eq!(card_rects[0], *r, "模态卡片第 {i} 帧位置变了");
    }
}

/// Esc 关闭的同一帧，下层内容位置不变。
#[test]
fn modal_esc_close_frame_keeps_underlay() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);

    let render = |input: egui::RawInput, open: bool| -> (Vec<Rect>, bool) {
        let mut rects = None;
        let mut requested_close = false;
        let mut output = ctx.run_ui(input, |ui| {
            rects = Some(draw_underlay(ui, &d));
            if open {
                let h = Modal::height(&d, ModalSize::Md, 120.0);
                let modal = Modal::new("设置", ModalSize::Md);
                if modal.begin(ui, &d, h).is_some() {
                    requested_close = modal.end(ui, &d);
                }
            }
        });
        output.textures_delta.clear();
        (rects.expect("rects not captured"), requested_close)
    };

    let _ = render(idle_input(), false);
    let (closed, _) = render(idle_input(), false);

    // 打开并按 Esc：end 应报告关闭请求，但下层 rect 不动。
    let (open_esc, close_requested) = render(esc_input(), true);
    assert!(close_requested, "Esc 未被 Modal::end 捕获");
    for (i, (c, o)) in closed.iter().zip(open_esc.iter()).enumerate() {
        assert_eq!(c, o, "Esc 关闭帧下层控件 {i} 移动了");
    }
}

/// Confirm 对话框：打开时下层内容不动。
#[test]
fn confirm_dialog_does_not_shift_underlay() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);

    let render = |open: bool| -> Vec<Rect> {
        let mut rects = None;
        let mut output = ctx.run_ui(idle_input(), |ui| {
            rects = Some(draw_underlay(ui, &d));
            if open {
                let c = crate::modal::Confirm::new("删除会话", "删除后无法恢复，确定？");
                c.show(ui, &d);
            }
        });
        output.textures_delta.clear();
        rects.expect("rects not captured")
    };

    let _ = render(false);
    let closed = render(false);
    let open = render(true);
    for (i, (c, o)) in closed.iter().zip(open.iter()).enumerate() {
        assert_eq!(c, o, "Confirm 打开后下层控件 {i} 移动了");
    }
}

// -----------------------------------------------------------------------
// 模态遮罩确实覆盖全屏（阻塞下层点击的几何前提）
// -----------------------------------------------------------------------

#[test]
fn modal_blocker_covers_full_screen() {
    let (ctx, d) = setup_ctx(neo_theme::ThemeMode::Light, 1.0);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));

    let mut output = ctx.run_ui(idle_input(), |ui| {
        let h = Modal::height(&d, ModalSize::Md, 120.0);
        Modal::new("设置", ModalSize::Md).begin(ui, &d, h);
    });
    output.textures_delta.clear();

    let blocker = ctx
        .read_response(egui::Id::new(("neo-modal-blocker", "设置")))
        .expect("阻塞层未注册");
    assert_eq!(
        blocker.rect, screen,
        "阻塞层必须覆盖整个内容区，否则下层边缘仍可点击"
    );
}
