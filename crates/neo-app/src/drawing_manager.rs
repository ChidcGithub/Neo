//! Hosted boards and per-session host services. Closing waits for host cleanup.
#[path = "drawing_host.rs"]
mod host;
use crate::drawing_runtime::{self, BoardKind, Event, Permissions, RuntimeHandle, State};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

trait Connection {
    fn request(&mut self, method: &str, params: Value) -> Result<String, String>;
    fn recv(&mut self) -> Option<Event>;
    fn reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
    ) -> Result<(), String>;
    /// Implementations must recheck the guard immediately before beginning the write.
    fn guarded_reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
        guard: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String>;
    fn live(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool>;
}
impl Connection for RuntimeHandle {
    fn request(&mut self, method: &str, params: Value) -> Result<String, String> {
        self.request(method, params)
    }
    fn recv(&mut self) -> Option<Event> {
        self.try_recv()
    }
    fn reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
    ) -> Result<(), String> {
        self.reply_host(id, result)
    }
    fn guarded_reply(
        &self,
        id: &str,
        result: Result<Value, drawing_runtime::RpcError>,
        guard: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.reply_host_guarded(id, result, guard)
    }
    fn live(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.connection_live()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Configure,
    Show,

    State,
    Close,
}
struct Pending {
    id: String,
    op: Operation,
}

struct Board {
    kind: BoardKind,
    connection: Box<dyn Connection>,
    state: Option<State>,
    ready: bool,
    failed: bool,
    exited: bool,
    pending: Option<Pending>,
    show: bool,
    configure_uncertain: bool,
    next_configure: Instant,
    retry_at: Instant,
    close_sent: bool,
    close_ok: bool,
    next_state: Instant,
    host: host::Host,
}
impl Board {
    fn new(kind: BoardKind, connection: Box<dyn Connection>) -> Self {
        Self {
            kind,
            connection,
            state: None,
            ready: false,
            failed: false,
            exited: false,
            pending: None,
            show: true,
            configure_uncertain: false,
            next_configure: Instant::now(),
            retry_at: Instant::now(),
            close_sent: false,
            close_ok: false,
            next_state: Instant::now(),
            host: host::Host::default(),
        }
    }
    fn send(&mut self, op: Operation, params: Value) -> Result<(), String> {
        let method = match op {
            Operation::Configure => "configure",
            Operation::Show => "show",

            Operation::State => "get_state",
            Operation::Close => "close",
        };
        let id = self.connection.request(method, params)?;
        self.pending = Some(Pending { id, op });
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct DrawingManager {
    boards: Vec<Board>,
    safe: bool,
    cfg: Option<neo_llm::Config>,
    ctx: egui::Context,
    services: bool,

    closing: Option<Instant>,
    close_failed: bool,
    notices: Vec<&'static str>,
}
impl DrawingManager {
    pub(crate) fn open(
        &mut self,
        kind: BoardKind,
        ctx: &egui::Context,
        safe: bool,
    ) -> Result<(), &'static str> {
        if self.closing.is_some() || self.capture_active() {
            return Err("桌面任务或画板关闭正在进行，请稍后再打开");
        }
        self.safe = safe;
        if let Some(board) = self.boards.iter_mut().find(|b| b.kind == kind && !b.exited) {
            if board.failed || board.close_sent {
                return Err("画板连接不可用，请先在画板内保存并关闭窗口");
            }
            board.show = true;
            return Ok(());
        }
        // Test builds never spawn installed GUI executables, even when present.
        #[cfg(not(test))]
        let connection = {
            let path = drawing_runtime::resolve_executable(kind)
                .map_err(|_| "画板程序缺失或路径不受信任，请检查安装")?;
            Box::new(
                RuntimeHandle::start(kind, &path, ctx.clone(), safe)
                    .map_err(|_| "画板启动失败，请检查安装后重试")?,
            ) as Box<dyn Connection>
        };
        #[cfg(not(test))]
        {
            self.boards.push(Board::new(kind, connection));
            Ok(())
        }
        #[cfg(test)]
        {
            let _ = (ctx, drawing_runtime::resolve_executable(kind));
            Err("画板程序缺失或路径不受信任，请检查安装")
        }
    }

    /// Includes startup, hidden windows, failed connections and acknowledged close
    /// until actual Exited. EOF can release runtime leases and restore its window,
    /// so Phase 1 never uses external leases to admit desktop work.
    pub fn has_open_boards(&self) -> bool {
        !self.boards.is_empty()
    }
    pub(crate) fn begin_close(&mut self) {
        if self.closing.is_some() {
            return;
        }
        self.close_failed = false;
        self.closing = Some(Instant::now() + Duration::from_secs(20));
        for board in &mut self.boards {
            board.show = false;
            board.host.cancel();
        }
    }
    pub(crate) fn closed(&self) -> bool {
        !self.has_open_boards()
    }
    pub(crate) fn take_close_failure(&mut self) -> bool {
        std::mem::take(&mut self.close_failed)
    }
    pub(crate) fn take_notices(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.notices)
    }

    /// Call before poll: configure only implemented services; consent is still per request.
    pub(crate) fn set_services(&mut self, cfg: neo_llm::Config, ctx: &egui::Context) {
        let cfg = (cfg.is_configured() && neo_llm::normalize_model_id(&cfg.model).is_some())
            .then_some(cfg);
        if self.cfg != cfg {
            self.cancel_host_tasks();
        }
        self.cfg = cfg;
        self.ctx = ctx.clone();
        self.services = true;
    }
    pub(crate) fn cancel_host_tasks(&mut self) {
        for b in &mut self.boards {
            b.host.cancel();
        }
    }
    pub(crate) fn capture_active(&self) -> bool {
        self.boards.iter().any(|b| b.host.capture_active())
    }
    pub(crate) fn capture_waiting(&self) -> bool {
        self.boards
            .iter()
            .any(|b| b.host.capture_ready(b.state.as_ref(), self.permissions()))
    }
    pub(crate) fn consent_pending(&mut self) -> bool {
        self.boards.iter_mut().any(|b| b.host.consent().is_some())
    }
    pub(crate) fn host_panel(&mut self, ctx: &egui::Context) {
        for b in &mut self.boards {
            host::panel(&mut b.host, ctx);
        }
    }
    /// UI barrier completed on a later native pass, with overlay and floating hidden.
    pub(crate) fn capture_permitted(&mut self, result: Result<(), String>, ctx: &egui::Context) {
        let permissions = self.permissions();
        for b in &mut self.boards {
            if b.failed || b.exited || b.close_sent || self.closing.is_some() {
                b.host.cancel();
            }
            b.host.capture_permitted(
                result.clone(),
                b.connection.as_ref(),
                ctx,
                b.state.as_ref(),
                permissions,
            );
        }
    }
    #[cfg(test)]
    pub(crate) fn inject_capture(&mut self, worker: crate::drawing_capture::CaptureHandle) {
        self.boards
            .iter_mut()
            .find(|b| b.host.capture_waiting())
            .unwrap()
            .host
            .inject_capture(worker);
    }
    fn permissions(&self) -> Permissions {
        Permissions::new(
            self.safe,
            self.services && cfg!(windows),
            self.cfg.is_some(),
        )
    }
    pub(crate) fn poll(&mut self, safe: bool) {
        if self.safe != safe {
            self.cancel_host_tasks();
        }
        self.safe = safe;
        let permissions = self.permissions();
        if self.closing.is_some() && self.boards.iter().any(|b| b.host.busy()) {
            // Cleanup is not a close timeout: keep the exit continuation until it is real.
            self.closing = Some(Instant::now() + Duration::from_secs(20));
        }
        let single_board = self.boards.len() == 1;
        let mut host_busy = self.boards.iter().any(|b| b.host.busy());
        let mut close_error = false;
        for board in &mut self.boards {
            board.host.check(
                board.state.as_ref(),
                permissions,
                !board.failed
                    && !board.exited
                    && board
                        .connection
                        .live()
                        .load(std::sync::atomic::Ordering::Acquire),
            );
            while let Some(event) = board.connection.recv() {
                match event {
                    Event::Ready(state) => {
                        board.ready = true;
                        board.state = Some(state);
                    }
                    Event::StateChanged(state) => {
                        if board.state.as_ref().is_some_and(|old| {
                            old.permissions != state.permissions
                                || old.document_id != state.document_id
                                || state.closed
                                || state.close_pending
                        }) {
                            board.host.cancel();
                        }
                        board.state = Some(state);
                    }
                    Event::HostRequest { id, method, params } => {
                        board.host.request(
                            id,
                            &method,
                            params,
                            board.state.as_ref(),
                            permissions,
                            self.cfg.as_ref(),
                            host_busy || self.closing.is_some() || board.close_sent,
                            single_board,
                        );
                        host_busy |= board.host.busy();
                        if board.host.capture_active() {
                            board.show = false;
                        }
                    }
                    Event::Exited => {
                        board.host.cancel();
                        board.exited = true;
                        // Only actual reaping proves a disconnected window cannot obstruct.
                        if !board.close_ok {
                            self.notices.push("画板进程已退出，请检查板内保存的文件");
                        }
                    }
                    Event::Failed(_) => {
                        board.host.cancel();
                        board.failed = true;
                        board.pending = None;
                        close_error |= self.closing.is_some();
                        self.notices
                            .push("画板连接不可用，请先在画板内保存并关闭窗口");
                    }
                    Event::Response { id, result, .. } => {
                        if board.host.response(&id, result.clone()) {
                            continue;
                        }
                        if !board.pending.as_ref().is_some_and(|p| p.id == id) {
                            continue;
                        }
                        let op = board.pending.take().unwrap().op;
                        match result {
                            Ok(_) => match op {
                                Operation::Configure | Operation::State => {
                                    board.configure_uncertain = false;
                                }
                                Operation::Close => {
                                    board.close_ok = true;
                                }
                                _ => {}
                            },
                            Err(error) => {
                                match op {
                                    Operation::Close => {
                                        board.close_sent = false;
                                        close_error = true;
                                        if error.code == "unsaved_changes" {
                                            board.show = true;
                                            self.notices.push(
                                                "画板有未保存内容，请先在画板内保存，再退出 Neo",
                                            );
                                        } else {
                                            self.notices.push("画板尚未确认关闭，已阻止 Neo 退出；请先在板内保存并关闭");
                                        }
                                    }
                                    Operation::Configure => {
                                        // Timeout is uncertainty, not a dead connection. Keep
                                        // state/close available and throttle configure retries.
                                        board.configure_uncertain = true;
                                        board.next_configure =
                                            Instant::now() + Duration::from_secs(2);
                                        board.next_state = Instant::now();
                                        self.notices.push("画板操作未完成，请在画板窗口中检查");
                                    }
                                    _ => {
                                        self.notices.push("画板操作未完成，请在画板窗口中检查");
                                    }
                                }
                            }
                        }
                    }
                }
                board.host.check(
                    board.state.as_ref(),
                    permissions,
                    !board.failed
                        && !board.exited
                        && board
                            .connection
                            .live()
                            .load(std::sync::atomic::Ordering::Acquire),
                );
            }
            board.host.check(
                board.state.as_ref(),
                permissions,
                !board.failed
                    && !board.exited
                    && board
                        .connection
                        .live()
                        .load(std::sync::atomic::Ordering::Acquire),
            );
            board
                .host
                .poll(board.connection.as_mut(), board.kind, &self.ctx);
        }
        self.boards.retain(|b| !b.exited || b.host.busy());
        if self.closing.is_some_and(|d| Instant::now() >= d) && !self.closed() {
            close_error = true;
            self.notices
                .push("画板尚未确认关闭，已阻止 Neo 退出；请先在板内保存并关闭");
        }
        if close_error {
            self.closing = None;
            self.close_failed = true;
        }

        for board in &mut self.boards {
            if !board.ready
                || board.failed
                || board.pending.is_some()
                || board.close_ok
                || Instant::now() < board.retry_at
            {
                continue;
            }
            let needs_configure = board.configure_uncertain
                || board
                    .state
                    .as_ref()
                    .is_none_or(|s| s.permissions != permissions);
            if board.host.capture_active() || (self.closing.is_some() && board.host.busy()) {
                // HostRequest can precede its hidden StateChanged. Query state while
                // waiting, but never show/configure/close away the runtime capture lease.
                if board.host.capture_waiting() && Instant::now() >= board.next_state {
                    if board.send(Operation::State, json!({})).is_err() {
                        board.host.cancel();
                    }
                    board.next_state = Instant::now() + Duration::from_secs(2);
                }
                continue;
            }
            let action = if self.closing.is_some() && !board.close_sent {
                Some((Operation::Close, json!({})))
            } else if needs_configure && Instant::now() >= board.next_configure {
                Some((Operation::Configure, permissions.params()))
            } else if board.show && !needs_configure && self.closing.is_none() {
                Some((Operation::Show, json!({})))
            } else if Instant::now() >= board.next_state {
                Some((Operation::State, json!({})))
            } else {
                None
            };
            if let Some((op, params)) = action {
                if board.send(op, params).is_err() {
                    board.retry_at = Instant::now() + Duration::from_secs(2);
                    if self.closing.is_some() {
                        self.closing = None;
                        self.close_failed = true;
                    }
                    self.notices
                        .push("画板连接不可用，请先在画板内保存并关闭窗口");
                    continue;
                }
                match op {
                    Operation::Show => board.show = false,
                    Operation::Close => board.close_sent = true,
                    _ => {}
                }
                board.next_state = Instant::now() + Duration::from_secs(2);
            }
        }
    }
}

#[cfg(test)]
#[path = "drawing_manager_tests.rs"]
pub(crate) mod tests;
