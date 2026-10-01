//! 工具确认卡：每张卡绑定会话代次和调用身份，旧回调不能回答新请求。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use egui::{Context, Pos2, Vec2, ViewportBuilder, ViewportCommand, ViewportId, WindowLevel};
use neo_theme::Theme;

use super::miniwin::OFFSCREEN;
use crate::brand::WhaleMark;
use crate::state::AppState;
use crate::ui::{tools, Skin};

const CARD_ID: u8 = neo_overlay::card_id::CONFIRM;
const CLOSED: u8 = u8::MAX;
type Identity = (u64, usize, String);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-confirmwin")
}

fn pending_identity(state: &AppState) -> Option<Identity> {
    let index = state.awaiting_tool()?;
    Some((
        state.session_epoch,
        index,
        state.messages[index].tool.as_ref()?.call_id.clone(),
    ))
}

#[derive(Default)]
pub struct ConfirmWin {
    open: bool,
    using_overlay: bool,
    identity: Option<Identity>,
    answer: Arc<AtomicU8>,
}

impl ConfirmWin {
    fn consume(&mut self, state: &mut AppState) {
        let pending = pending_identity(state);
        if self.identity != pending {
            self.answer.store(CLOSED, Ordering::Release);
            self.answer = Arc::new(AtomicU8::new(0));
            self.identity = pending;
            return;
        }
        let Some((_, index, _)) = self.identity.as_ref() else {
            return;
        };
        let index = *index;
        let answer = self.answer.load(Ordering::Acquire);
        if answer == 0 || answer == CLOSED {
            return;
        }
        self.answer.store(CLOSED, Ordering::Release);
        let meta = state.messages[index].tool.as_ref().unwrap();
        if meta.name == "ask_user" {
            match answer {
                a @ 10..=13 => {
                    let options = meta
                        .args
                        .get("options")
                        .and_then(|v| v.as_str())
                        .map(neo_tools::tools::ask_user::parse_options)
                        .unwrap_or_default();
                    if let Some(picked) = options.get((a - 10) as usize) {
                        state.answer_question(index, Some(picked.clone()));
                    }
                }
                3 => state.answer_question(index, None),
                _ => {}
            }
        } else {
            match answer {
                1 => state.approve_tool(index),
                // 安全开关可能在卡片绘制之后变化，消费时再次拒绝批量授权。
                2 if !state.classroom_safe => state.approve_all_awaiting(),
                3 => state.deny_tool(index),
                _ => {}
            }
        }
        self.identity = pending_identity(state);
        self.answer = Arc::new(AtomicU8::new(0));
    }

    pub fn tick(
        &mut self,
        ctx: &Context,
        state: &mut AppState,
        theme: Theme,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        if let Some(layer) = overlay { layer.set_card(CARD_ID, None); }
        if self.using_overlay {
            self.answer.store(CLOSED, Ordering::Release);
            self.answer = Arc::new(AtomicU8::new(0));
            self.open = false;
            self.using_overlay = false;
        }
        self.consume(state);
        let pending = state.awaiting_tool();
        let open = pending.is_some();
        let m = theme.metrics;
        let monitor = super::miniwin::screen_geometry(ctx, false).monitor;
        let size = Vec2::new(m.s(560.0), m.s(420.0))
            .min(monitor - egui::vec2(m.s(32.0), m.s(32.0)))
            .max(egui::vec2(m.s(280.0), m.s(180.0)));
        let center = Pos2::new(
            ((monitor.x - size.x) * 0.5).max(m.s(16.0)),
            ((monitor.y - size.y) * 0.42).max(m.s(16.0)),
        );
        let snapshot = pending.and_then(|i| state.messages[i].tool.clone());
        let remaining = state.awaiting_tool_count();
        let allow_batch = !state.classroom_safe;
        let answer = self.answer.clone();
        let main_ctx = ctx.clone();
        let draw = move |ui: &mut egui::Ui| {
            theme.apply(ui.ctx());
            let Some(meta) = &snapshot else { return };
            let whale = WhaleMark::cached(ui.ctx());
            let skin = Skin::new(theme, &whale);
            let clicked = ui
                .add_enabled_ui(answer.load(Ordering::Acquire) == 0, |ui| {
                    if meta.name == "ask_user" {
                        match tools::ask(ui, &skin, meta) {
                            Some(tools::AskAnswer::Pick(i)) => Some(10 + i as u8),
                            Some(tools::AskAnswer::Skip) => Some(3),
                            None => None,
                        }
                    } else {
                        match tools::confirm(ui, &skin, meta, remaining, allow_batch) {
                            Some(tools::Answer::Once) => Some(1),
                            Some(tools::Answer::Always) => Some(2),
                            Some(tools::Answer::Deny) => Some(3),
                            None => None,
                        }
                    }
                })
                .inner;
            if let Some(value) = clicked {
                if answer
                    .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    main_ctx.request_repaint();
                    ui.ctx().request_repaint();
                }
            }
        };
        if open != self.open {
            ctx.send_viewport_cmd_to(
                viewport_id(),
                ViewportCommand::OuterPosition(if open { center } else { OFFSCREEN }),
            );
            ctx.send_viewport_cmd_to(
                viewport_id(),
                ViewportCommand::InnerSize(if open { size } else { Vec2::new(1.0, 1.0) }),
            );
            if open {
                ctx.send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop),
                );
            }
            self.open = open;
        }
        let suspended = crate::app::desktop_viewport(ctx, viewport_id());
        ctx.show_viewport_deferred(
            viewport_id(),
            ViewportBuilder::default()
                .with_title("Neo 需要许可")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_always_on_top()
                .with_active(false)
                .with_transparent(true)
                .with_visible(!suspended)
                .with_inner_size(if open { size } else { Vec2::new(1.0, 1.0) })
                .with_position(if open { center } else { OFFSCREEN }),
            move |ui, _class| {
                if !crate::app::desktop_suspended(ui.ctx()) { draw(ui); }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ChatMessage, ToolMeta, ToolState};

    fn request(state: &mut AppState, id: &str) {
        let mut meta = ToolMeta::restored("write_file · test");
        meta.call_id = id.into();
        meta.state = ToolState::AwaitingConfirm;
        state
            .messages
            .push(ChatMessage::tool_result(meta, String::new()));
    }

    #[test]
    fn safety_overlay_loss_reopens_fallback_and_invalidates_old_answer() {
        let ctx = Context::default();
        ctx.set_embed_viewports(false);
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        request(&mut state, "pending");
        win.consume(&mut state);
        win.open = true;
        win.using_overlay = true;
        let old = win.answer.clone();
        old.store(1, Ordering::Release);
        let theme = Theme::new(neo_theme::ThemeMode::Dark, 1080.0, neo_theme::Distance::default());
        ctx.begin_pass(egui::RawInput::default());
        win.tick(&ctx, &mut state, theme, None);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_eq!(state.awaiting_tool(), Some(0));
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        assert!(!win.using_overlay && win.open);
        let viewport = output.viewport_output.get(&viewport_id()).unwrap();
        assert!(viewport.viewport_ui_cb.is_some());
        assert_ne!(viewport.builder.mouse_passthrough, Some(true));
        assert_ne!(viewport.builder.position, Some(OFFSCREEN));
        assert!(!viewport.commands.iter().any(|cmd| matches!(cmd, ViewportCommand::Close)));
        assert!(old.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_err());
        win.answer.store(3, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.messages[0].tool.as_ref().unwrap().state, ToolState::Denied);
    }

    #[test]
    fn safety_batch_permission_and_consecutive_questions_are_isolated() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        state.classroom_safe = false;
        request(&mut state, "permission");
        for id in ["question-1", "question-2"] {
            let mut meta = ToolMeta::restored("ask_user");
            meta.call_id = id.into();
            meta.args = serde_json::json!({"question":"choose", "options":"A|B"});
            meta.state = ToolState::AwaitingConfirm;
            state
                .messages
                .push(ChatMessage::tool_result(meta, String::new()));
        }
        request(&mut state, "permission-2");
        win.consume(&mut state);
        let permission = win.answer.clone();
        permission.store(2, Ordering::Release);
        win.consume(&mut state);
        assert!(state.auto_approve_tools);
        for index in [0, 3] {
            assert_eq!(
                state.messages[index].tool.as_ref().unwrap().state,
                ToolState::Running
            );
        }
        assert_eq!(state.awaiting_tool(), Some(1));
        assert_eq!(permission.load(Ordering::Acquire), CLOSED);
        assert!(permission
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_err());
        win.answer.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(1));
        assert!(state.messages[1].content.is_empty());
        let first_question = win.answer.clone();
        first_question.store(10, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(2));
        assert!(state.messages[1].content.contains('A'));
        assert!(first_question
            .compare_exchange(0, 11, Ordering::AcqRel, Ordering::Acquire)
            .is_err());
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(2));
        assert!(state.messages[2].content.is_empty());
        win.answer.store(11, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        assert!(state.messages[2].content.contains('B'));
    }

    #[test]
    fn safety_mode_rejects_batch_answer_from_previously_enabled_card() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        state.classroom_safe = false;
        request(&mut state, "permission");
        win.consume(&mut state);
        let old = win.answer.clone();
        state.classroom_safe = true;
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(0));
        assert!(!state.auto_approve_tools);
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        win.answer.store(3, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        assert_eq!(state.messages[0].tool.as_ref().unwrap().state, ToolState::Denied);
    }

    #[test]
    fn safety_stale_card_cannot_approve_next_request_or_session() {
        let mut state = AppState::default();
        let mut win = ConfirmWin::default();
        request(&mut state, "same");
        win.consume(&mut state);
        let old = win.answer.clone();
        state.new_session();
        request(&mut state, "same");
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(0));
        assert!(!state.auto_approve_tools);
        assert_eq!(old.load(Ordering::Acquire), CLOSED);
        win.answer.store(1, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), None);
        request(&mut state, "next");
        old.store(2, Ordering::Release);
        win.consume(&mut state);
        assert_eq!(state.awaiting_tool(), Some(1));
        assert!(!state.auto_approve_tools);
    }
}
