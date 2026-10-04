//! Hosted drawing client with asynchronous, bidirectional host request routing.
//!
//! `start` validates a trusted path, then starts all process work on a background thread.
//! Poll `try_recv` until `Exited`; only `Ready` means configure has succeeded. Requests
//! are queued, never waited on by the caller. `Response` also reports local timeouts.
//! A timeout is NOT cancellation: a late response is ignored, but subsequent ordered
//! state events remain authoritative. `Failed` invalidates all outstanding requests.
//! `is_alive` describes process lifetime (including startup), not protocol readiness.
//!
//! Drop disconnects stdin asynchronously and delegates reaping to the worker; it never
//! sends discard_unsaved, kills a process, joins a thread, or promises the GUI has exited.
//! In particular, keep the handle after an unsaved_changes close error. Permissions
//! are not a sandbox; installation/development directories must be trusted and protected
//! against concurrent modification. Host services and their authorization belong to the
//! manager; this transport only routes requests and a single final response per ID.
//! At most four process/I/O lifetimes may coexist, including disconnected cleanup.
//! A stuck child or inherited pipe retains its slot: retries cannot leak unbounded threads.
//! Disconnect can release runtime window leases; it is never a desktop-capture barrier.

#[path = "drawing_runtime/paths.rs"]
mod paths;
#[path = "drawing_runtime/protocol.rs"]
mod protocol;
#[path = "drawing_runtime/transport.rs"]
mod transport;

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub use protocol::{RpcError, State};

const MAX_LINE: usize = 65_536;
const QUEUE: usize = 32;
const EVENT_QUEUE: usize = 128;
const HOST_ID_BUDGET: usize = 4096;
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoardKind {
    Drawing,
    Blackboard,
}

impl BoardKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Drawing => "drawing",
            Self::Blackboard => "blackboard",
        }
    }

    fn executable(self) -> &'static str {
        match self {
            Self::Drawing => "neo-drawing.exe",
            Self::Blackboard => "neo-blackboard.exe",
        }
    }
}

/// Service permissions; classroom safety always disables desktop capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    classroom_safe: bool,
    capture_allowed: bool,
    agent_allowed: bool,
}

impl Permissions {
    pub const fn new(safe: bool, capture: bool, agent: bool) -> Self {
        Self {
            classroom_safe: safe,
            capture_allowed: capture && !safe,
            agent_allowed: agent,
        }
    }

    pub const fn restricted(safe: bool) -> Self {
        Self::new(safe, false, false)
    }

    pub const fn classroom_safe(self) -> bool {
        self.classroom_safe
    }

    pub const fn capture_allowed(self) -> bool {
        self.capture_allowed
    }

    pub const fn agent_allowed(self) -> bool {
        self.agent_allowed
    }

    pub fn params(self) -> Value {
        serde_json::json!({
            "classroom_safe": self.classroom_safe,
            "desktop_capture_allowed": self.capture_allowed,
            "agent_allowed": self.agent_allowed,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Ready(State),
    StateChanged(State),
    /// Dispatch asynchronously, including unknown methods and jobs.cancel. Reply once.
    HostRequest {
        id: String,
        method: String,
        params: Value,
    },
    Response {
        id: String,
        method: String,
        result: Result<Value, RpcError>,
    },
    /// Connection unusable; all pending requests are invalidated. Not proof of exit.
    Failed(String),
    /// Emitted only after the child has actually been reaped (or failed to spawn).
    Exited,
}

/// Default: exe-parent/apps/{drawing,blackboard}/neo-{drawing,blackboard}.exe.
/// Explicit absolute NEO_DRAWING_DIR / NEO_BLACKBOARD_DIR overrides are supported.
/// Rejects traversal, symlinks and Windows reparse points in every path component.
pub fn resolve_executable(kind: BoardKind) -> Result<PathBuf, String> {
    paths::resolve(kind)
}

struct Shared {
    stop: AtomicBool,
    usable: Arc<AtomicBool>,
    host_requests: Mutex<HostRequests>,
    alive: AtomicBool,
    exited: AtomicBool,
}

#[derive(Default)]
struct HostRequests {
    // Retain IDs for the whole session; true means a final response was queued.
    ids: HashMap<String, bool>,
    inflight: usize,
}

enum Command {
    Request(Request),
    HostReply {
        id: String,
        bytes: Vec<u8>,
        guard: ReplyGuard,
    },
}

struct ReplyGuard {
    valid: Arc<AtomicBool>,
    cancelled: Vec<u8>,
}

impl ReplyGuard {
    fn check(&self, bytes: &mut Vec<u8>) {
        if !self.valid.load(Ordering::Acquire) {
            bytes.clone_from(&self.cancelled);
        }
    }
}

struct Request {
    id: String,
    method: String,
    params: Value,
    deadline: Instant,
}

pub struct RuntimeHandle {
    generation: u64,
    next_request: u64,
    requests: mpsc::SyncSender<Command>,
    events: mpsc::Receiver<Event>,
    failure: mpsc::Receiver<String>,
    shared: Arc<Shared>,
    exit_delivered: bool,
}

impl RuntimeHandle {
    pub fn start(
        kind: BoardKind,
        path: &Path,
        ctx: egui::Context,
        safe: bool,
    ) -> Result<Self, String> {
        // Do not let a call site turn this client into an arbitrary executable launcher.
        let executable = paths::validate_for_start(kind, path)?;
        let generation = NEXT_SESSION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| "runtime session IDs exhausted".to_string())?;
        let (requests, incoming) = mpsc::sync_channel(QUEUE);
        let (outgoing, events) = mpsc::sync_channel(EVENT_QUEUE);
        let (failed, failure) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            usable: Arc::new(AtomicBool::new(true)),
            host_requests: Mutex::default(),
            alive: AtomicBool::new(true),
            exited: AtomicBool::new(false),
        });
        let permit = transport::WorkerPermit::acquire()?;
        let worker_shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("neo-board-{generation}"))
            .spawn(move || {
                transport::run(
                    kind,
                    executable,
                    generation,
                    Permissions::restricted(safe),
                    incoming,
                    transport::Output {
                        events: outgoing,
                        failure: failed,
                        shared: worker_shared,
                        ctx,
                    },
                    permit,
                )
            })
            .map_err(|e| format!("cannot start runtime worker: {e}"))?;
        Ok(Self {
            generation,
            next_request: 1,
            requests,
            events,
            failure,
            shared,
            exit_delivered: false,
        })
    }

    /// Globally unique within this host process; channels and IDs never cross handles.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Nonblocking bounded enqueue. Call after Ready. Unsupported/unready requests
    /// complete asynchronously with an error Response. All params must be objects.
    /// Use Permissions::params() for configure after Ready; close MUST be {}.
    /// Only the manager may enable a service after explicit UI authorization.
    pub fn request(&mut self, method: &str, params: Value) -> Result<String, String> {
        protocol::validate_outbound(method, &params)?;
        if !self.shared.usable.load(Ordering::Acquire) {
            return Err("runtime disconnected".into());
        }
        let number = self.next_request;
        self.next_request = number.checked_add(1).ok_or("request IDs exhausted")?;
        let id = format!("neo:{}:{number}", self.generation);
        let frame = protocol::request_frame(&id, method, &params)?;
        drop(frame);
        let timeout = if method == "close" {
            CLOSE_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };
        self.requests
            .try_send(Command::Request(Request {
                id: id.clone(),
                method: method.into(),
                params,
                deadline: Instant::now() + timeout,
            }))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => "runtime request queue full".to_string(),
                mpsc::TrySendError::Disconnected(_) => "runtime disconnected".to_string(),
            })?;
        Ok(id)
    }

    /// Nonblocking final response through the same bounded queue/single writer as requests.
    /// Unknown IDs and duplicate replies are rejected. Queue-full errors may be retried.
    pub fn reply_host(&self, id: &str, result: Result<Value, RpcError>) -> Result<(), String> {
        self.reply_host_guarded(id, result, Arc::new(AtomicBool::new(true)))
    }

    /// Final response whose authorization can be revoked while queued in either layer.
    /// A false guard replaces the whole response with a same-ID `cancelled` error.
    /// The manager must retain a clone even after enqueue succeeds and store false with
    /// Release on configuration/context revocation; never re-enable an old token.
    ///
    /// The writer checks with Acquire immediately before starting the frame. Once it
    /// starts sending, bytes cannot be recalled: no mid-frame replacement is attempted.
    /// The subruntime must still validate permissions and document/page/revision context.
    /// Duplicate-ID, queue-full retry and connection-liveness rules match `reply_host`.
    pub fn reply_host_guarded(
        &self,
        id: &str,
        result: Result<Value, RpcError>,
        valid: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let mut bytes = protocol::host_response(id, result)?;
        let guard = ReplyGuard {
            valid,
            cancelled: protocol::host_response(
                id,
                Err(RpcError::local(
                    "cancelled",
                    "host response authorization revoked before sending",
                )),
            )?,
        };
        let mut host = self
            .shared
            .host_requests
            .lock()
            .map_err(|_| "host request lock poisoned")?;
        if !self.shared.usable.load(Ordering::Acquire) {
            return Err("runtime disconnected".into());
        }
        let replied = host.ids.get_mut(id).ok_or("unknown runtime request ID")?;
        if *replied {
            return Err("runtime request already replied".into());
        }
        guard.check(&mut bytes);
        self.requests
            .try_send(Command::HostReply {
                id: id.into(),
                bytes,
                guard,
            })
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => "runtime request queue full".to_string(),
                mpsc::TrySendError::Disconnected(_) => "runtime disconnected".to_string(),
            })?;
        *replied = true;
        Ok(())
    }

    /// Monotonic connection-liveness token, independent of UI event consumption.
    /// Workers must load with Acquire before each capture step and before publishing
    /// results. True is not a readiness, permission or native-window-hidden guarantee.
    pub fn connection_live(&self) -> Arc<AtomicBool> {
        self.shared.usable.clone()
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        if let Ok(event) = self.events.try_recv() {
            return Some(event);
        }
        if let Ok(error) = self.failure.try_recv() {
            return Some(Event::Failed(error));
        }
        if !self.exit_delivered && self.shared.exited.load(Ordering::Acquire) {
            self.exit_delivered = true;
            return Some(Event::Exited);
        }
        None
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::Acquire)
    }
}

impl Drop for RuntimeHandle {
    fn drop(&mut self) {
        self.shared.usable.store(false, Ordering::Release);
        self.shared.stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "drawing_runtime_tests.rs"]
mod tests;
