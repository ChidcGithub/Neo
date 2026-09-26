//! 工具确认窗：模型请求权限时**独立弹出**的窗口，不绑定主窗。
//!
//! 后台静默执行时主窗藏在托盘 —— 确认请求若画在主窗里，用户根本看不见，
//! 授权就等于必须先把主窗叫出来。独立弹出后这套依赖没了：主窗开着也好、
//! 藏着也好，确认窗都直接出现在屏幕中央。
//!
//! 机制与迷你窗一致（见 miniwin.rs 的 `OFFSCREEN` 注释）：每帧注册防回收，
//! 恒可见、休眠时缩 1x1 挪屏幕外（透明视口切 `Visible` 的首帧会闪黑），
//! 显隐走尺寸/位置命令。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use egui::{Context, Pos2, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel};
use neo_theme::Theme;

use crate::brand::WhaleMark;
use crate::state::AppState;
use crate::ui::{Skin, tools};

use super::miniwin::OFFSCREEN;

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-confirmwin")
}

/// 独立确认窗的运行时状态，挂在 `NeoApp` 上，每帧由 [`ConfirmWin::tick`] 驱动。
#[derive(Default)]
pub struct ConfirmWin {
    /// 上一帧是否开着（沿检测用）。
    open: bool,
    /// 回调里写、tick 里读：0 未决 / 1 仅此次 / 2 本会话都允许 / 3 拒绝。
    answer: Arc<AtomicU8>,
}

impl ConfirmWin {
    /// 每帧驱动一次，挂在 `NeoApp::tick` 里（与迷你窗同路，logic-only 也经过）。
    pub fn tick(&mut self, ctx: &Context, state: &mut AppState, theme: Theme) {
        let pending = state.awaiting_tool();
        let open = pending.is_some();

        // 1. 答案回传：回调里点出的按钮在这里落到 state。
        if let Some(index) = pending {
            match self.answer.swap(0, Ordering::Relaxed) {
                1 => state.approve_tool(index),
                2 => {
                    // 「都允许」只在本次会话内有效，重启即失效；
                    // 且要追溯本轮已挂起的其它待确认项，不能光批当前这条。
                    state.approve_all_awaiting();
                }
                3 => state.deny_tool(index),
                _ => {}
            }
        } else {
            // 没有待确认项时清掉残留答案，下一个请求从 0 开始。
            self.answer.store(0, Ordering::Relaxed);
        }

        // 2. 位置：屏幕中央偏上（长面板的视觉重心比几何中心略高）。
        //    尺寸随显示器收窄：窗比屏大时右/下缘会画出屏外。
        let m = theme.metrics;
        let monitor = ctx
            .input(|i| i.viewport().monitor_size)
            .unwrap_or(Vec2::new(1920.0, 1080.0));
        let size = Vec2::new(m.s(560.0), m.s(420.0))
            .min(monitor - egui::vec2(m.s(32.0), m.s(32.0)))
            .max(egui::vec2(m.s(280.0), m.s(180.0)));
        let center = Pos2::new(
            ((monitor.x - size.x) * 0.5).max(m.s(16.0)),
            ((monitor.y - size.y) * 0.42).max(m.s(16.0)),
        );

        // 3. 显隐沿：先落位再恢复尺寸；休眠缩 1x1 回 OFFSCREEN。
        //    确认是要用户马上看见的东西，露面时重新提顶。
        if open != self.open {
            if open {
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(center));
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
            } else {
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::InnerSize(Vec2::new(1.0, 1.0)),
                );
                ctx.send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
            }
            self.open = open;
        }

        // 4. 每帧注册（防回收 + 内容增量；主视口托盘态 10fps，够刷确认文案）。
        let snapshot = pending.and_then(|i| state.messages[i].tool.clone());
        let remaining = state.awaiting_tool_count();
        let answer = Arc::clone(&self.answer);
        ctx.show_viewport_deferred(
            viewport_id(),
            ViewportBuilder::default()
                .with_title("Neo 需要许可")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                // 不抢焦点：用户可能正在打字；按钮靠鼠标点（no_activate 照收点击）。
                .with_active(false)
                .with_transparent(true)
                .with_visible(true)
                .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
                .with_position(if open { center } else { OFFSCREEN }),
            move |ui, _class| {
                let Some(meta) = &snapshot else { return };
                let whale = WhaleMark::load(ui.ctx());
                let skin = Skin::new(theme, &whale);
                match tools::confirm(ui, &skin, meta, remaining) {
                    Some(tools::Answer::Once) => answer.store(1, Ordering::Relaxed),
                    Some(tools::Answer::Always) => answer.store(2, Ordering::Relaxed),
                    Some(tools::Answer::Deny) => answer.store(3, Ordering::Relaxed),
                    None => {}
                }
            },
        );
    }
}
