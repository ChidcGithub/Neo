//! 工具确认卡：模型请求权限时弹出，**画在统一渲染层里**（不再是独立窗口）。
//!
//! 后台静默执行时主窗藏在托盘 —— 确认请求若画在主窗里，用户根本看不见，
//! 授权就等于必须先把主窗叫出来。画进常驻渲染层后：主窗开着也好、藏着也好，
//! 确认卡都直接出现在屏幕中央，且**没有新建窗口**（闪黑在结构上不存在）。
//!
//! 离屏测试没有渲染层（`overlay == None`）：退回旧的 egui 视口路径，
//! 让快照测试照常覆盖确认 UI 的观感。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use egui::{Context, Pos2, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel};
use neo_theme::Theme;

use crate::brand::WhaleMark;
use crate::state::AppState;
use crate::ui::{Skin, tools};

use super::miniwin::OFFSCREEN;

/// 渲染层里的卡片 id（分配表在 neo_overlay::card_id）。
const CARD_ID: u8 = neo_overlay::card_id::CONFIRM;

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
    pub fn tick(
        &mut self,
        ctx: &Context,
        state: &mut AppState,
        theme: Theme,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        let pending = state.awaiting_tool();
        let open = pending.is_some();

        // 1. 答案回传：回调里点出的按钮在这里落到 state。
        if let Some(index) = pending {
            // ask_user 的提问卡复用本条通道：10+i = 点中第 i 个选项，3 = 跳过。
            let is_ask = state.messages[index]
                .tool
                .as_ref()
                .is_some_and(|t| t.name == "ask_user");
            let answer = self.answer.swap(0, Ordering::Relaxed);
            if is_ask {
                match answer {
                    a @ 10..=13 => {
                        let options = state.messages[index]
                            .tool
                            .as_ref()
                            .and_then(|t| t.args.get("options"))
                            .and_then(|v| v.as_str())
                            .map(neo_tools::tools::ask_user::parse_options)
                            .unwrap_or_default();
                        state.answer_question(
                            index,
                            options.get((a - 10) as usize).cloned(),
                        );
                    }
                    3 => state.answer_question(index, None),
                    _ => {}
                }
            } else {
                match answer {
                    1 => state.approve_tool(index),
                    2 => {
                        // 「都允许」只在本次会话内有效，重启即失效；
                        // 且要追溯本轮已挂起的其它待确认项，不能光批当前这条。
                        state.approve_all_awaiting();
                    }
                    3 => state.deny_tool(index),
                    _ => {}
                }
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

        // 3. 内容快照与答案句柄（两条路径共用）。
        let snapshot = pending.and_then(|i| state.messages[i].tool.clone());
        let remaining = state.awaiting_tool_count();
        let answer = Arc::clone(&self.answer);
        let draw = move |ui: &mut egui::Ui| {
            // 渲染层的 egui 上下文不管主题：卡片每次露面都同步一次（幂等、便宜）。
            theme.apply(ui.ctx());
            let Some(meta) = &snapshot else { return };
            let whale = WhaleMark::cached(ui.ctx());
            let skin = Skin::new(theme, &whale);
            if meta.name == "ask_user" {
                // 提问卡：问题 + 选项按钮（无「本会话都允许」——它不是权限请求）。
                match tools::ask(ui, &skin, meta) {
                    Some(tools::AskAnswer::Pick(i)) => {
                        answer.store(10 + i as u8, Ordering::Relaxed)
                    }
                    Some(tools::AskAnswer::Skip) => answer.store(3, Ordering::Relaxed),
                    None => {}
                }
                return;
            }
            match tools::confirm(ui, &skin, meta, remaining) {
                Some(tools::Answer::Once) => answer.store(1, Ordering::Relaxed),
                Some(tools::Answer::Always) => answer.store(2, Ordering::Relaxed),
                Some(tools::Answer::Deny) => answer.store(3, Ordering::Relaxed),
                None => {}
            }
        };

        // 4. 渲染层优先；没有层（离屏测试 / 初始化失败）退回独立视口。
        //    draw 只读捕获（theme Copy / 其余都是共享句柄），天然 Fn，
        //    两条路径都直接用。
        if let Some(layer) = overlay {
            let card = if open {
                Some(neo_overlay::Card::interactive(
                    [center.x, center.y, size.x, size.y],
                    draw,
                ))
            } else {
                None
            };
            layer.set_card(CARD_ID, card);
            self.open = open;
            return;
        }

        // ---- 测试回退路径：独立视口（每帧注册防回收，休眠缩 1x1 挪屏外） ----
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
            move |ui, _class| draw(ui),
        );
    }
}
