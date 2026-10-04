use super::*;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Default)]
struct Wire {
    sent: Vec<(String, String, Value)>,
    events: VecDeque<Event>,
    replies: Vec<(String, Result<Value, drawing_runtime::RpcError>)>,
    live: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    block_replies: bool,
    defer_guarded: bool,
    queued_guarded: Vec<(
        String,
        Result<Value, drawing_runtime::RpcError>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )>,
}
#[derive(Clone)]
pub(crate) struct Fake(Rc<RefCell<Wire>>, u64);
impl Connection for Fake {
    fn request(&mut self, method: &str, params: Value) -> Result<String, String> {
        let mut wire = self.0.borrow_mut();
        let id = format!("neo:{}:{}", self.1, wire.sent.len());
        wire.sent.push((id.clone(), method.into(), params));
        Ok(id)
    }
    fn recv(&mut self) -> Option<Event> {
        self.0.borrow_mut().events.pop_front()
    }
    fn reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
    ) -> Result<(), String> {
        if self.0.borrow().block_replies {
            return Err("runtime request queue full".into());
        }
        self.0.borrow_mut().replies.push((id.into(), result));
        Ok(())
    }
    fn guarded_reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
        guard: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        if self.0.borrow().block_replies {
            return Err("runtime request queue full".into());
        }
        if self.0.borrow().defer_guarded {
            self.0
                .borrow_mut()
                .queued_guarded
                .push((id.into(), result, guard));
            Ok(())
        } else {
            self.reply(
                id,
                if guard.load(std::sync::atomic::Ordering::Acquire) {
                    result
                } else {
                    Err(host::error("cancelled", "Revoked before writer started"))
                },
            )
        }
    }
    fn live(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.0
            .borrow_mut()
            .live
            .get_or_insert_with(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)))
            .clone()
    }
}
impl Fake {
    pub(crate) fn emit(&self, event: Event) {
        if matches!(event, Event::Exited | Event::Failed(_)) {
            self.live()
                .store(false, std::sync::atomic::Ordering::Release);
        }
        self.0.borrow_mut().events.push_back(event);
    }
    pub(crate) fn defer_guarded(&self) {
        self.0.borrow_mut().defer_guarded = true;
    }
    pub(crate) fn queued_guard(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.0.borrow().queued_guarded.last().unwrap().2.clone()
    }
    pub(crate) fn flush_guarded(&self) {
        let queued = std::mem::take(&mut self.0.borrow_mut().queued_guarded);
        for (id, result, guard) in queued {
            let reply = if guard.load(std::sync::atomic::Ordering::Acquire) {
                result
            } else {
                Err(host::error("cancelled", "Revoked before writer started"))
            };
            self.0.borrow_mut().replies.push((id, reply));
        }
    }
    pub(crate) fn block_replies(&self, blocked: bool) {
        self.0.borrow_mut().block_replies = blocked;
    }
    pub(crate) fn replies(&self) -> Vec<(String, Result<Value, drawing_runtime::RpcError>)> {
        self.0.borrow().replies.clone()
    }
    pub(crate) fn sent(&self) -> Vec<(String, String, Value)> {
        self.0.borrow().sent.clone()
    }
    pub(crate) fn respond(&self, state: State) {
        let (id, method, _) = self.sent().last().unwrap().clone();
        self.emit(Event::StateChanged(state));
        self.emit(Event::Response {
            id,
            method,
            result: Ok(json!({})),
        });
    }
    pub(crate) fn error(&self, code: &str) {
        let (id, method, _) = self.sent().last().unwrap().clone();
        self.emit(Event::Response {
            id,
            method,
            result: Err(drawing_runtime::RpcError {
                code: code.into(),
                message: code.into(),
                data: None,
            }),
        });
    }
}
pub(crate) fn state(hidden: bool, dirty: bool, safe: bool) -> State {
    State {
        visible: !hidden,
        hidden_confirmed: hidden,
        has_window: true,
        dirty,
        closed: false,
        configured: true,
        document_id: "document-1".into(),
        page_id: "page-1".into(),
        revision: 7,
        desired_visible: true,
        effective_visible: !hidden,
        close_pending: false,
        connected: true,
        permissions: Permissions::restricted(safe),
    }
}
pub(crate) fn attach(manager: &mut DrawingManager, kind: BoardKind, generation: u64) -> Fake {
    let fake = Fake(Rc::new(RefCell::new(Wire::default())), generation);
    manager
        .boards
        .push(Board::new(kind, Box::new(fake.clone())));
    fake
}
fn ready(manager: &mut DrawingManager, kind: BoardKind, generation: u64) -> Fake {
    let fake = attach(manager, kind, generation);
    fake.emit(Event::Ready(state(false, true, true)));
    manager.poll(true);
    fake.respond(state(false, true, true));
    manager.poll(true);
    fake
}

#[test]
fn repeated_open_preserves_one_connection_and_dirty_document() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    for _ in 0..3 {
        manager
            .open(BoardKind::Drawing, &egui::Context::default(), true)
            .unwrap();
    }
    manager.poll(true);
    assert_eq!(manager.boards.len(), 1);
    assert!(manager.boards[0].state.as_ref().unwrap().dirty);
    assert_eq!(
        fake.sent().iter().map(|r| r.1.as_str()).collect::<Vec<_>>(),
        ["show", "show"]
    );
    assert_eq!(
        manager.boards[0].state.as_ref().unwrap().document_id,
        "document-1"
    );
}

#[test]
fn unready_safety_change_configures_latest_restrictions_before_show() {
    let mut manager = DrawingManager::default();
    let fake = attach(&mut manager, BoardKind::Drawing, 1);
    manager.poll(false);
    manager.poll(true);
    assert!(manager.has_open_boards());
    assert!(fake.sent().is_empty());
    fake.emit(Event::Ready(state(false, true, false)));
    manager.poll(true);
    assert_eq!(fake.sent()[0].1, "configure");
    assert_eq!(fake.sent()[0].2, Permissions::restricted(true).params());
    fake.respond(state(false, true, true));
    manager.poll(true);
    assert_eq!(fake.sent()[1].1, "show");
}

#[test]
fn open_board_guard_ignores_visibility_and_requires_every_process_exit() {
    let mut manager = DrawingManager::default();
    let a = ready(&mut manager, BoardKind::Drawing, 11);
    let b = attach(&mut manager, BoardKind::Blackboard, 22);
    a.emit(Event::StateChanged(state(true, true, true)));
    manager.poll(true);
    assert!(manager.has_open_boards());
    a.emit(Event::StateChanged(state(false, true, true)));
    manager.poll(true);
    assert!(manager.has_open_boards());
    a.emit(Event::Exited);
    manager.poll(true);
    assert!(
        manager.has_open_boards(),
        "unready board still blocks desktop admission"
    );
    b.emit(Event::Failed("EOF".into()));
    manager.poll(true);
    assert!(manager.has_open_boards());
    b.emit(Event::Exited);
    manager.poll(true);
    assert!(!manager.has_open_boards());
    assert!(!a
        .sent()
        .iter()
        .chain(b.sent().iter())
        .any(|r| r.1.starts_with("window.")));
}

#[test]
fn eof_is_not_hidden_and_actual_reaping_is_required_before_replacement() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    fake.emit(Event::StateChanged(state(true, true, true)));
    fake.emit(Event::Failed("EOF".into()));
    manager.poll(true);
    assert!(manager.has_open_boards());
    assert!(manager
        .open(BoardKind::Drawing, &egui::Context::default(), true)
        .is_err());
    assert_eq!(manager.boards.len(), 1);
    assert!(manager.boards[0].state.as_ref().unwrap().dirty);
    fake.emit(Event::Exited);
    manager.poll(true);
    assert!(manager.closed());
}

#[test]
fn configure_timeout_queries_state_and_throttles_retry_without_marking_dead() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    manager.poll(false);
    assert_eq!(fake.sent().last().unwrap().1, "configure");
    fake.error("timeout");
    manager.poll(false);
    assert!(!manager.boards[0].failed);
    assert!(manager.boards[0].configure_uncertain);
    assert_eq!(fake.sent().last().unwrap().1, "get_state");
    fake.respond(state(false, true, true));
    manager.poll(false);
    let count = fake.sent().len();
    for _ in 0..10 {
        manager.poll(false);
    }
    assert_eq!(fake.sent().len(), count);
    manager.boards[0].next_configure = Instant::now();
    manager.poll(false);
    assert_eq!(fake.sent().last().unwrap().1, "configure");
    assert_eq!(
        fake.sent().last().unwrap().2,
        Permissions::restricted(false).params()
    );
    fake.respond(state(false, true, false));
    manager.poll(false);
    assert!(!manager.boards[0].configure_uncertain);
    assert!(manager.has_open_boards());
}

#[test]
fn dirty_close_refuses_exit_then_retries_without_discard_and_waits_for_exit() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    manager.begin_close();
    manager.poll(true);
    assert_eq!(fake.sent().last().unwrap().1, "close");
    assert_eq!(fake.sent().last().unwrap().2, json!({}));
    fake.error("unsaved_changes");
    manager.poll(true);
    assert!(manager.take_close_failure());
    assert!(!manager.closed());
    assert_eq!(fake.sent().last().unwrap().1, "show");
    fake.respond(state(false, false, true));
    manager.poll(true);
    manager.begin_close();
    manager.poll(true);
    let mut closed = state(true, false, true);
    closed.closed = true;
    fake.respond(closed);
    manager.poll(true);
    assert!(!manager.closed(), "close success alone is not process exit");
    fake.emit(Event::Exited);
    manager.poll(true);
    assert!(manager.closed());
    assert!(!fake
        .sent()
        .iter()
        .any(|r| r.2.get("discard_unsaved").is_some()));
}

#[test]
fn state_cache_tracks_get_state_and_same_revision_visibility_and_dirty_changes() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    manager.boards[0].next_state = Instant::now();
    manager.poll(true);
    assert_eq!(fake.sent().last().unwrap().1, "get_state");
    let mut saved = state(true, false, true);
    saved.desired_visible = false;
    fake.respond(saved.clone());
    manager.poll(true);
    assert_eq!(manager.boards[0].state, Some(saved));
    fake.emit(Event::StateChanged(state(false, true, true)));
    manager.poll(true);
    assert_eq!(manager.boards[0].state, Some(state(false, true, true)));
}

#[test]
fn configure_timeout_does_not_prevent_close_or_discard_dirty_state() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    manager.poll(false);
    fake.error("timeout");
    manager.begin_close();
    manager.poll(false);
    assert_eq!(fake.sent().last().unwrap().1, "close");
    assert_eq!(fake.sent().last().unwrap().2, json!({}));
    assert!(!manager.boards[0].failed);
    assert!(manager.boards[0].state.as_ref().unwrap().dirty);
    assert!(manager.has_open_boards());
}

#[test]
fn close_timeout_preserves_handle_and_blocks_exit() {
    let mut manager = DrawingManager::default();
    let fake = ready(&mut manager, BoardKind::Drawing, 1);
    manager.begin_close();
    manager.poll(true);
    fake.error("timeout");
    manager.poll(true);
    assert!(manager.take_close_failure());
    assert!(!manager.closed());
    assert!(manager.boards[0].state.as_ref().unwrap().dirty);
}
