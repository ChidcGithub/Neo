//! Per-connection host dispatcher. Never reads board images or objects before Neo UI consent.
//! The app owns the native hide barrier; `capture_permitted` is its only admission
//! hook. Keep that barrier while `capture_active`, including cancelled worker cleanup.
use super::Connection;
use crate::drawing_agent::{AgentHandle, AgentRequest};
use crate::drawing_capture::CaptureHandle;
use crate::drawing_objects::{
    editable_object, validate_snapshot, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_OBJECTS,
};
use crate::drawing_runtime::{BoardKind, Permissions, RpcError, State};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const PNG_MAX: usize = 8 * 1024 * 1024;
const TOTAL_MAX: usize = 32 * 1024 * 1024;
const OBJECT_PAGE_LIMIT: usize = 32;
const OBJECT_PAGE_BYTES: usize = 48 * 1024;
const OBJECT_CHUNK_BYTES: usize = 8192;
const TIMEOUT: Duration = Duration::from_secs(60);
static NEXT_ASSET: AtomicU64 = AtomicU64::new(1);
type Reply = Result<Value, RpcError>;

pub(super) fn error(code: &str, message: &str) -> RpcError {
    RpcError {
        code: code.into(),
        message: message.into(),
        data: None,
    }
}
fn invalid() -> RpcError {
    error("invalid_params", "Invalid or unauthorized host request")
}
fn token(s: &str) -> bool {
    s.strip_prefix("asset:").is_some_and(|s| {
        !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, RpcError> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max)
        .ok_or_else(invalid)
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

struct Asset {
    bytes: Vec<u8>,
    expires: Instant,
}
#[derive(Default)]
struct Resources {
    assets: HashMap<String, Asset>,
}
impl Resources {
    fn expire(&mut self) {
        self.assets.retain(|_, a| Instant::now() < a.expires);
    }
    fn insert(&mut self, bytes: Vec<u8>) -> Reply {
        self.expire();
        if bytes.len() > PNG_MAX
            || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || self.assets.len() >= 4
            || self.assets.values().map(|a| a.bytes.len()).sum::<usize>() + bytes.len() > TOTAL_MAX
        {
            return Err(error("resource_limit", "PNG resource budget exceeded"));
        }
        let sequence = NEXT_ASSET
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| error("resource_limit", "Asset IDs exhausted"))?;
        let id = format!("asset:neo-{sequence}");
        let result = json!({"asset_ref":id,"total_bytes":bytes.len(),"crc32":crc32(&bytes)});
        self.assets.insert(
            id,
            Asset {
                bytes,
                expires: Instant::now() + TIMEOUT,
            },
        );
        Ok(result)
    }
    fn request(&mut self, method: &str, p: &Value) -> Reply {
        self.expire();
        let id = text(p, "asset_ref", 134)?;
        if !token(id) {
            return Err(invalid());
        }
        if method == "resources.release" {
            return Ok(json!({"released":self.assets.remove(id).is_some()}));
        }
        let a = self
            .assets
            .get(id)
            .ok_or_else(|| error("resource_not_found", "Unknown or expired session asset"))?;
        let offset = p["offset"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(invalid)?;
        let length = p["length"]
            .as_u64()
            .filter(|n| (1..=8192).contains(n))
            .ok_or_else(invalid)? as usize;
        if offset >= a.bytes.len() {
            return Err(invalid());
        }
        let end = offset.saturating_add(length).min(a.bytes.len());
        Ok(
            json!({"asset_ref":id,"offset":offset,"total_bytes":a.bytes.len(),
            "bytes":&a.bytes[offset..end],"next_offset":end,"eof":end == a.bytes.len()}),
        )
    }
}

pub(super) struct Consent {
    pub prompt: String,
    pub assets: Vec<String>,
    pub cfg: neo_llm::Config,
    pub write_allowed: bool,
    pub write_back: bool,
    pub structured_edit: bool,
    pub vision_confirmed: bool,
}
struct Download {
    request_id: Option<String>,
    images: Vec<Vec<u8>>,
    bytes: Vec<u8>,
    total: Option<usize>,
    snapshot: Option<Snapshot>,
}
struct Snapshot {
    objects: Vec<Value>,
    offset: usize,
    total: Option<usize>,
    bytes: usize,
    reading: Option<ObjectRead>,
}
struct ObjectRead {
    id: String,
    total: usize,
    next_object: usize,
    bytes: Vec<u8>,
}
impl Snapshot {
    fn complete(&self) -> bool {
        self.reading.is_none() && self.total == Some(self.offset)
    }
    fn context(v: &Value, original: &State) -> bool {
        v["document_id"].as_str() == Some(&original.document_id)
            && v["page_id"].as_str() == Some(&original.page_id)
            && v["revision"].as_u64() == Some(original.revision)
    }
    fn budget(&self, size: usize) -> Result<(), RpcError> {
        if self
            .bytes
            .saturating_add(size)
            .saturating_add(usize::from(self.offset > 0))
            > MAX_SNAPSHOT_BYTES
        {
            return Err(error(
                "resource_limit",
                "Current page exceeds 128 KiB; nothing sent",
            ));
        }
        Ok(())
    }
    fn begin_read(&mut self, e: RpcError, original: &State) -> Result<(), RpcError> {
        let Some(v) = e.data.as_ref().filter(|_| e.code == "object_too_large") else {
            return Err(e);
        };
        let id = v["object_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(invalid)?;
        let total = v["total_bytes"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or_else(invalid)?;
        let next = self.offset.checked_add(1).ok_or_else(invalid)?;
        let last = match v.get("next_offset") {
            Some(Value::Null) => true,
            Some(value) if value.as_u64() == Some(next as u64) => false,
            _ => return Err(invalid()),
        };
        if !Self::context(v, original)
            || v["offset"].as_u64() != Some(self.offset as u64)
            || self.reading.is_some()
            || self.offset >= MAX_SNAPSHOT_OBJECTS
            || self.total.is_some_and(|n| next > n || (last && next != n))
            || self.objects.iter().any(|o| o["id"].as_str() == Some(id))
        {
            return Err(invalid());
        }
        self.budget(total)?;
        // Explicit null identifies the last object, but completion still waits
        // for its chunks. Numeric offsets retain the legacy continuation behavior.
        if last {
            self.total = Some(next);
        }
        self.reading = Some(ObjectRead {
            id: id.to_owned(),
            total,
            next_object: next,
            bytes: Vec::new(),
        });
        Ok(())
    }
    fn accept_chunk(&mut self, v: Value, original: &State) -> Result<(), RpcError> {
        let read = self.reading.as_mut().ok_or_else(invalid)?;
        let bytes = v["bytes"].as_array().ok_or_else(invalid)?;
        let next = read
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(invalid)?;
        // A full requested chunk (or the final remainder) bounds the number of
        // round trips as well as memory; tiny/empty progress cannot prolong reads.
        if !Self::context(&v, original)
            || v["object_id"].as_str() != Some(&read.id)
            || v["encoding"] != "utf8_json_u8_array"
            || v["total_bytes"].as_u64() != Some(read.total as u64)
            || v["offset"].as_u64() != Some(read.bytes.len() as u64)
            || bytes.len() != OBJECT_CHUNK_BYTES.min(read.total - read.bytes.len())
            || v["next_offset"].as_u64() != Some(next as u64)
            || v["eof"].as_bool() != Some(next == read.total)
        {
            return Err(invalid());
        }
        for byte in bytes {
            read.bytes.push(
                byte.as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(invalid)?,
            );
        }
        if next == read.total {
            // Runtime-owned JSON, not model output. Decode only after reassembly:
            // a chunk boundary may fall in the middle of a UTF-8 code point.
            let object: Value = serde_json::from_slice(&read.bytes).map_err(|_| invalid())?;
            if object["id"].as_str() != Some(&read.id) {
                return Err(invalid());
            }
            let size = read
                .total
                .max(serde_json::to_vec(&object).map_err(|_| invalid())?.len());
            let next_object = read.next_object;
            self.budget(size)?;
            self.bytes += size + usize::from(self.offset > 0);
            if editable_object(&object) {
                self.objects.push(object);
            } else if !matches!(
                object["kind"]["type"].as_str(),
                Some("image" | "handwritten")
            ) {
                return Err(invalid());
            }
            self.offset = next_object;
            self.reading = None;
            if self.complete() {
                validate_snapshot(&self.objects).map_err(|e| error("invalid_params", &e))?;
            }
        }
        Ok(())
    }
    fn response(&mut self, result: Reply, original: &State) -> Result<(), RpcError> {
        if self.reading.is_some() {
            self.accept_chunk(result?, original)
        } else {
            match result {
                Ok(v) => self.accept(v, original),
                Err(e) => self.begin_read(e, original),
            }
        }
    }
    fn accept(&mut self, v: Value, original: &State) -> Result<(), RpcError> {
        let total = v["total"].as_u64().ok_or_else(invalid)?;
        if total > MAX_SNAPSHOT_OBJECTS as u64 {
            return Err(error(
                "resource_limit",
                "Current page exceeds 256 objects; nothing sent",
            ));
        }
        let total = total as usize;
        let objects = v["objects"].as_array().ok_or_else(invalid)?;
        let next = self.offset.checked_add(objects.len()).ok_or_else(invalid)?;
        if v["document_id"].as_str() != Some(&original.document_id)
            || v["page_id"].as_str() != Some(&original.page_id)
            || v["revision"].as_u64() != Some(original.revision)
            || v["offset"].as_u64() != Some(self.offset as u64)
            || self.total.is_some_and(|n| n != total)
            || objects.len() > OBJECT_PAGE_LIMIT
            || next > total
            || (objects.is_empty() && next != total)
            || if next < total {
                v["next_offset"].as_u64() != Some(next as u64)
            } else {
                v.get("next_offset") != Some(&Value::Null)
            }
        {
            return Err(invalid());
        }
        // Charge every object, including images and handwriting, before filtering.
        // Count array brackets and separators across pages, not only editable content.
        for (index, object) in objects.iter().enumerate() {
            let size = serde_json::to_vec(object).map_err(|_| invalid())?.len();
            self.bytes = self
                .bytes
                .saturating_add(size + usize::from(self.offset + index > 0));
            if self.bytes > MAX_SNAPSHOT_BYTES {
                return Err(error(
                    "resource_limit",
                    "Current page exceeds 128 KiB; nothing sent",
                ));
            }
            if editable_object(object) {
                self.objects.push(object.clone());
            } else if !matches!(
                object["kind"]["type"].as_str(),
                Some("image" | "handwritten")
            ) {
                return Err(invalid());
            }
        }
        self.offset = next;
        self.total = Some(total);
        if self.complete() {
            validate_snapshot(&self.objects).map_err(|e| error("invalid_params", &e))?;
        }
        Ok(())
    }
}
enum Phase {
    Consent,
    Reading(Download),
    Agent(AgentHandle),
    #[cfg(test)]
    FakeAgent {
        finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
        result: Reply,
    },
    CaptureWait,
    Capture(CaptureHandle),
}
struct Task {
    id: String,
    job: String,
    original: State,
    deadline: Instant,
    consent: Option<Consent>,
    phase: Phase,
    cancelled: Option<RpcError>,
    cancels: Vec<String>,
    reply_guard: Arc<AtomicBool>,
}
impl Task {
    fn capture(&self) -> bool {
        matches!(self.phase, Phase::CaptureWait | Phase::Capture(_))
    }
    fn revoke(&mut self, reason: RpcError) {
        self.reply_guard.store(false, Ordering::Release);
        if self.cancelled.is_none() {
            self.cancelled = Some(reason);
        }
        match &mut self.phase {
            Phase::Capture(worker) => worker.cancel(),
            Phase::Agent(worker) => worker.cancel(),
            Phase::Reading(download) => {
                download.images.clear();
                download.bytes.clear();
                download.snapshot = None;
            }
            _ => {}
        }
    }
    fn valid(&self, state: Option<&State>, permissions: Permissions) -> bool {
        state.is_some_and(|s| {
            s.connected
                && s.configured
                && !s.closed
                && !s.close_pending
                && s.document_id == self.original.document_id
                && s.page_id == self.original.page_id
                && s.revision == self.original.revision
                && s.permissions == self.original.permissions
                && permissions == self.original.permissions
                && (!self.capture()
                    || (s.has_window
                        && (matches!(self.phase, Phase::CaptureWait) || capture_hidden(s))))
        })
    }
}

fn capture_hidden(s: &State) -> bool {
    s.has_window && s.hidden_confirmed && !s.visible && !s.effective_visible
}

// Enqueue is not delivery: retain the same token the transport checks before writing.
// Expiry/eviction revokes it before dropping our reference, never leaves a valid orphan.
struct ReplyContext {
    original: State,
    // Only a completed capture owns an asset. Window visibility is no longer an
    // authorization condition: the runtime restores its window before downloading.
    asset_ref: Option<String>,
    guard: Arc<AtomicBool>,
    expires: Instant,
}
impl ReplyContext {
    fn revoke(&mut self, resources: &mut Resources) {
        self.guard.store(false, Ordering::Release);
        if let Some(id) = self.asset_ref.take() {
            resources.assets.remove(&id);
        }
    }
}
impl Drop for ReplyContext {
    fn drop(&mut self) {
        self.guard.store(false, Ordering::Release);
    }
}

#[derive(Default)]
pub(super) struct Host {
    task: Option<Task>,
    resources: Resources,
    replies: VecDeque<(String, Reply)>,
    completed: VecDeque<(String, String)>,
    reply_contexts: HashMap<String, ReplyContext>,
    #[cfg(test)]
    fake_capture: Option<CaptureHandle>,
    #[cfg(test)]
    agent_requests: Option<Vec<AgentRequest>>,
}
impl Host {
    pub fn busy(&self) -> bool {
        self.task.is_some()
            || self
                .replies
                .iter()
                .any(|(id, _)| self.reply_contexts.contains_key(id))
    }
    #[cfg(test)]
    pub(super) fn inject_capture(&mut self, worker: CaptureHandle) {
        self.fake_capture = Some(worker);
    }
    pub fn capture_active(&self) -> bool {
        self.task.as_ref().is_some_and(Task::capture)
    }
    pub fn capture_waiting(&self) -> bool {
        self.task
            .as_ref()
            .is_some_and(|t| matches!(t.phase, Phase::CaptureWait) && t.cancelled.is_none())
    }
    pub fn capture_ready(&self, state: Option<&State>, permissions: Permissions) -> bool {
        self.capture_waiting()
            && state.is_some_and(capture_hidden)
            && self
                .task
                .as_ref()
                .is_some_and(|t| t.valid(state, permissions) && Instant::now() < t.deadline)
    }
    pub fn consent(&mut self) -> Option<&mut Consent> {
        self.task
            .as_mut()
            .filter(|t| matches!(t.phase, Phase::Consent) && t.cancelled.is_none())?
            .consent
            .as_mut()
    }
    pub fn cancel(&mut self) {
        if let Some(t) = &mut self.task {
            t.revoke(error(
                "cancelled",
                "Cancelled; already transmitted data cannot be recalled",
            ));
        }
        self.resources.assets.clear();
        for context in self.reply_contexts.values_mut() {
            context.revoke(&mut self.resources);
        }
        for (id, reply) in &mut self.replies {
            if self.reply_contexts.contains_key(id) {
                *reply = Err(error("cancelled", "Authorization revoked before delivery"));
            }
        }
    }
    pub fn check(&mut self, state: Option<&State>, permissions: Permissions, live: bool) {
        if let Some(t) = &mut self.task {
            if !live || !t.valid(state, permissions) {
                t.revoke(error(
                    "cancelled",
                    "Session, document, page or permissions changed",
                ));
            } else if Instant::now() >= t.deadline {
                t.revoke(error(
                    "timeout",
                    "Host task exceeded 60 seconds; waiting for cleanup",
                ));
            }
        }
        for context in self.reply_contexts.values_mut() {
            let original = &context.original;
            let valid = live
                && Instant::now() < context.expires
                && state.is_some_and(|s| {
                    s.connected
                        && s.configured
                        && !s.closed
                        && !s.close_pending
                        && s.document_id == original.document_id
                        && s.page_id == original.page_id
                        && s.revision == original.revision
                        && s.permissions == original.permissions
                        && permissions == original.permissions
                });
            if !valid {
                context.revoke(&mut self.resources);
            }
        }
        for (id, reply) in &mut self.replies {
            if reply.is_ok()
                && self
                    .reply_contexts
                    .get(id)
                    .is_some_and(|c| !c.guard.load(Ordering::Acquire))
            {
                *reply = Err(error(
                    "cancelled",
                    "Context changed before response delivery",
                ));
            }
        }
        self.reply_contexts.retain(|id, context| {
            Instant::now() < context.expires
                || self.replies.iter().any(|(pending, _)| pending == id)
        });
        if !live {
            self.resources.assets.clear();
        }
    }
    fn queue(&mut self, id: String, result: Reply) {
        // Account for the full response envelope, not just answer text.
        let frame = match &result {
            Ok(v) => json!({"version":1,"type":"response","id":id,"ok":true,"result":v}),
            Err(e) => {
                json!({"version":1,"type":"response","id":id,"ok":false,"error":{"code":e.code,"message":e.message}})
            }
        };
        let result = if serde_json::to_vec(&frame).map_or(true, |v| v.len() > 65536) {
            Err(error("resource_limit", "Host reply exceeds 64 KiB"))
        } else {
            result
        };
        self.replies.push_back((id, result));
    }
    pub fn request(
        &mut self,
        id: String,
        method: &str,
        p: Value,
        state: Option<&State>,
        permissions: Permissions,
        cfg: Option<&neo_llm::Config>,
        global_busy: bool,
        single_board: bool,
    ) {
        if method == "jobs.cancel" {
            let result = (|| {
                let request = text(&p, "request_id", 256)?;
                let job = text(&p, "job_id", 256)?;
                if let Some(t) = &mut self.task {
                    if t.id == request && t.job == job {
                        t.cancels.push(id.clone());
                        t.revoke(error(
                            "cancelled",
                            "Cancelled; already transmitted data cannot be recalled",
                        ));
                        return Ok(None);
                    }
                }
                if self.completed.iter().any(|(r, j)| r == request && j == job) {
                    // An idempotent cancellation for an old job must not cancel a new one.
                    if let Some(context) = self.reply_contexts.get_mut(request) {
                        context.revoke(&mut self.resources);
                    }
                    for (id, reply) in &mut self.replies {
                        if id == request {
                            *reply = Err(error("cancelled", "Cancelled before delivery"));
                        }
                    }
                    return Ok(Some(json!({"cancelled":true})));
                }
                Err(error("job_not_found", "Unknown request/job association"))
            })();
            match result {
                Ok(None) => {}
                Ok(Some(v)) => self.queue(id, Ok(v)),
                Err(e) => self.queue(id, Err(e)),
            }
            return;
        }
        if matches!(method, "resources.read" | "resources.release") {
            let reply = self.resources.request(method, &p);
            self.queue(id, reply);
            return;
        }
        let result = (|| {
            if !matches!(method, "host.ask_agent" | "host.capture_region") {
                return Err(error("method_not_found", "Host method is not implemented"));
            }
            if global_busy || self.busy() {
                return Err(error("busy", "Another host task is active"));
            }
            let s = state
                .filter(|s| s.connected && s.configured && !s.closed && !s.close_pending)
                .ok_or_else(invalid)?;
            if p["user_authorized"] != true
                || p["document_id"].as_str() != Some(&s.document_id)
                || p["page_id"].as_str() != Some(&s.page_id)
                || p["revision"].as_u64() != Some(s.revision)
                || s.permissions != permissions
            {
                return Err(invalid());
            }
            let job = text(&p, "job_id", 256)?.to_owned();
            let capture = method == "host.capture_region";
            let consent = if capture {
                if !single_board {
                    return Err(error(
                        "busy",
                        "Close other boards before capturing; multi-board capture is not supported",
                    ));
                }
                if !permissions.capture_allowed()
                    || !s.has_window
                    || p["windows_hidden_confirmed"] != true
                {
                    return Err(error(
                        "permission_denied",
                        "Capture requires confirmed hidden windows and capture permission",
                    ));
                }
                None
            } else {
                if !permissions.agent_allowed() {
                    return Err(error("permission_denied", "Agent service is unavailable"));
                }
                let cfg = cfg.ok_or_else(invalid)?.clone();
                let prompt = text(&p, "prompt", 16384.min(cfg.context_tokens))?.to_owned();
                let write_allowed = p["write_back"].as_bool().ok_or_else(invalid)?;
                let assets = p["asset_refs"]
                    .as_array()
                    .filter(|a| a.len() <= 4)
                    .ok_or_else(invalid)?;
                let mut refs = Vec::new();
                for a in assets {
                    let a = a.as_str().filter(|a| token(a)).ok_or_else(invalid)?;
                    if refs.iter().any(|r| r == a) {
                        return Err(invalid());
                    }
                    refs.push(a.to_owned());
                }
                Some(Consent {
                    prompt,
                    assets: refs,
                    cfg,
                    write_allowed,
                    write_back: false,
                    structured_edit: false,
                    vision_confirmed: false,
                })
            };
            self.task = Some(Task {
                id: id.clone(),
                job,
                original: s.clone(),
                deadline: Instant::now() + TIMEOUT,
                consent,
                phase: if capture {
                    Phase::CaptureWait
                } else {
                    Phase::Consent
                },
                cancelled: None,
                cancels: Vec::new(),
                reply_guard: Arc::new(AtomicBool::new(true)),
            });
            Ok(())
        })();
        if let Err(e) = result {
            self.queue(id, Err(e));
        }
    }
    pub fn authorize(&mut self, allow: bool) {
        let Some(t) = &mut self.task else { return };
        if !matches!(t.phase, Phase::Consent) {
            return;
        }
        if !allow {
            t.revoke(error("permission_denied", "User denied Agent access"));
            return;
        }
        let c = t.consent.as_mut().unwrap();
        if t.cancelled.is_some() || (!c.assets.is_empty() && !c.vision_confirmed) {
            return;
        }
        c.structured_edit &= c.write_allowed && c.write_back;
        t.phase = Phase::Reading(Download {
            snapshot: c.structured_edit.then(|| Snapshot {
                objects: Vec::new(),
                offset: 0,
                total: None,
                bytes: 2,
                reading: None,
            }),
            request_id: None,
            images: Vec::new(),
            bytes: Vec::new(),
            total: None,
        });
    }
    pub fn capture_permitted(
        &mut self,
        result: Result<(), String>,
        connection: &dyn Connection,
        ctx: &egui::Context,
        state: Option<&State>,
        permissions: Permissions,
    ) {
        self.check(
            state,
            permissions,
            connection.live().load(Ordering::Acquire),
        );
        // An early UI permit is not remembered. A later confirmed runtime state AND
        // a fresh UI barrier permit are required; the outbound host flag alone is not state.
        if result.is_ok() && !self.capture_ready(state, permissions) {
            return;
        }
        let Some(t) = &mut self.task else { return };
        if !matches!(t.phase, Phase::CaptureWait) || t.cancelled.is_some() {
            return;
        }
        #[cfg(test)]
        let started = result.and_then(|()| {
            self.fake_capture
                .take()
                .ok_or_else(|| "Test capture not injected".into())
        });
        #[cfg(test)]
        let _ = (connection, ctx);
        #[cfg(not(test))]
        let started = result.and_then(|()| CaptureHandle::start(connection.live(), ctx.clone()));
        match started {
            Ok(worker) => t.phase = Phase::Capture(worker),
            Err(message) => t.revoke(error("capture_failed", &message)),
        }
    }
    pub fn response(&mut self, id: &str, result: Reply) -> bool {
        let Some(t) = &mut self.task else {
            return false;
        };
        let Phase::Reading(d) = &mut t.phase else {
            return false;
        };
        if d.request_id.as_deref() != Some(id) {
            return false;
        }
        d.request_id = None;
        if t.cancelled.is_some() {
            return true;
        }
        let c = t.consent.as_ref().unwrap();
        if d.images.len() == c.assets.len() {
            let checked = d
                .snapshot
                .as_mut()
                .ok_or_else(invalid)
                .and_then(|s| s.response(result, &t.original));
            if let Err(e) = checked {
                t.revoke(e);
            }
            return true;
        }
        let checked = result.and_then(|v| {
            let total = v["total_bytes"]
                .as_u64()
                .filter(|n| *n > 0 && *n <= PNG_MAX as u64)
                .ok_or_else(invalid)? as usize;
            let bytes = v["bytes"]
                .as_array()
                .filter(|b| !b.is_empty() && b.len() <= 8192)
                .ok_or_else(invalid)?;
            let next = d.bytes.len().checked_add(bytes.len()).ok_or_else(invalid)?;
            if v["asset_ref"].as_str() != Some(&c.assets[d.images.len()])
                || v["offset"].as_u64() != Some(d.bytes.len() as u64)
                || next > total
                || v["next_offset"].as_u64() != Some(next as u64)
                || v["eof"].as_bool() != Some(next == total)
                || d.total.is_some_and(|n| n != total)
                || d.images.iter().map(Vec::len).sum::<usize>() + total > TOTAL_MAX
            {
                return Err(invalid());
            }
            let bytes: Result<Vec<u8>, _> = bytes
                .iter()
                .map(|b| {
                    b.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or_else(invalid)
                })
                .collect();
            d.bytes.extend(bytes?);
            d.total = Some(total);
            if next == total {
                if !d.bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(invalid());
                }
                d.images.push(std::mem::take(&mut d.bytes));
                d.total = None;
            }
            Ok(())
        });
        if let Err(e) = checked {
            t.revoke(e);
        }
        true
    }
    pub fn poll(&mut self, connection: &mut dyn Connection, kind: BoardKind, ctx: &egui::Context) {
        self.resources.expire();
        let mut finished = None;
        if let Some(t) = &mut self.task {
            if let Some(e) = &t.cancelled {
                let cleaned = match &t.phase {
                    Phase::Agent(w) => w.is_finished(),
                    Phase::Capture(w) => w.is_finished(),
                    #[cfg(test)]
                    Phase::FakeAgent { finished, .. } => finished.load(Ordering::Acquire),
                    _ => true,
                };
                if cleaned {
                    finished = Some(Err(e.clone()));
                }
            } else {
                match &mut t.phase {
                    Phase::Reading(d) if d.request_id.is_none() => {
                        let c = t.consent.as_ref().unwrap();
                        if d.images.len() == c.assets.len()
                            && d.snapshot.as_ref().is_some_and(|s| !s.complete())
                        {
                            let snapshot = d.snapshot.as_ref().unwrap();
                            let (method, params) = if let Some(read) = &snapshot.reading {
                                (
                                    "objects.read",
                                    json!({
                                        "document_id": t.original.document_id,
                                        "page_id": t.original.page_id,
                                        "expected_revision": t.original.revision,
                                        "object_id": read.id,
                                        "offset": read.bytes.len(),
                                        "length": OBJECT_CHUNK_BYTES,
                                    }),
                                )
                            } else {
                                (
                                    "objects.list",
                                    json!({
                                        "document_id": t.original.document_id,
                                        "page_id": t.original.page_id,
                                        "expected_revision": t.original.revision,
                                        "offset": snapshot.offset,
                                        "limit": OBJECT_PAGE_LIMIT,
                                        "max_bytes": OBJECT_PAGE_BYTES,
                                    }),
                                )
                            };
                            match connection.request(method, params) {
                                Ok(id) => d.request_id = Some(id),
                                Err(e) => finished = Some(Err(error("object_read_failed", &e))),
                            }
                        } else if d.images.len() == c.assets.len() {
                            let request = AgentRequest {
                                edit_objects: d.snapshot.take().map(|s| s.objects),
                                prompt: c.prompt.clone(),
                                images: std::mem::take(&mut d.images),
                                write_back: c.write_allowed && c.write_back,
                                vision_confirmed: c.vision_confirmed,
                                object_prefix: format!(
                                    "neo-{}",
                                    NEXT_ASSET.fetch_add(1, Ordering::Relaxed)
                                ),
                                kind,
                            };
                            #[cfg(test)]
                            if let Some(requests) = &mut self.agent_requests {
                                requests.push(request);
                                t.phase = Phase::FakeAgent {
                                    finished: Arc::new(AtomicBool::new(false)),
                                    result: Ok(json!({"answer":"mock", "operations":[]})),
                                };
                                return;
                            }
                            match AgentHandle::start(
                                c.cfg.clone(),
                                request,
                                connection.live(),
                                ctx.clone(),
                            ) {
                                Ok(w) => t.phase = Phase::Agent(w),
                                Err(e) => finished = Some(Err(error("agent_failed", &e))),
                            }
                        } else {
                            match connection.request("resources.read", json!({"asset_ref":c.assets[d.images.len()],"offset":d.bytes.len(),"length":8192})) {
                                Ok(id) => d.request_id = Some(id),
                                Err(e) => finished = Some(Err(error("resource_read_failed", &e))),
                            }
                        }
                    }
                    #[cfg(test)]
                    Phase::FakeAgent {
                        finished: done,
                        result,
                    } => {
                        if done.load(Ordering::Acquire) {
                            finished = Some(result.clone());
                        }
                    }
                    Phase::Agent(w) => {
                        if let Some(result) = w.poll() {
                            finished = Some(result.map_err(|e| error("agent_failed", &e)));
                        }
                    }
                    Phase::Capture(w) => {
                        if let Some(result) = w.poll() {
                            finished = Some(
                                result
                                    .map_err(|e| error("capture_failed", &e))
                                    .and_then(|bytes| self.resources.insert(bytes)),
                            );
                        }
                    }
                    _ => {}
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if let Some(result) = finished {
            let t = self.task.take().unwrap();
            self.reply_contexts.insert(
                t.id.clone(),
                ReplyContext {
                    original: t.original.clone(),
                    asset_ref: if t.capture() {
                        result
                            .as_ref()
                            .ok()
                            .and_then(|v| v["asset_ref"].as_str())
                            .map(str::to_owned)
                    } else {
                        None
                    },
                    guard: t.reply_guard.clone(),
                    expires: Instant::now() + TIMEOUT,
                },
            );
            self.completed.push_back((t.id.clone(), t.job));
            if self.completed.len() > 128 {
                if let Some((id, _)) = self.completed.pop_front() {
                    if let Some(mut context) = self.reply_contexts.remove(&id) {
                        context.revoke(&mut self.resources);
                    }
                }
            }
            self.queue(t.id, result);
            for id in t.cancels {
                self.queue(id, Ok(json!({"cancelled":true})));
            }
        }
        while let Some((id, reply)) = self.replies.front() {
            if !connection.live().load(Ordering::Acquire) {
                self.replies.clear();
                self.reply_contexts.clear();
                self.resources.assets.clear();
                break;
            }
            let sent = if reply.is_ok() {
                if let Some(context) = self.reply_contexts.get(id) {
                    connection.guarded_reply(id, reply.clone(), context.guard.clone())
                } else {
                    connection.reply(id, reply.clone())
                }
            } else {
                connection.reply(id, reply.clone())
            };
            if sent.is_err() {
                ctx.request_repaint_after(Duration::from_millis(16));
                break;
            }
            self.replies.pop_front();
        }
    }
}

/// Only metadata is rendered; no thumbnails/resource reads happen in this panel.
pub(super) fn panel(host: &mut Host, ctx: &egui::Context) {
    use crate::i18n::tr;
    let mut decision = None;
    if let Some(c) = host.consent() {
        egui::Window::new(tr("画板 Agent 授权")).id(egui::Id::new("drawing-host-consent"))
            .collapsible(false).resizable(true).show(ctx, |ui| {
                ui.label(tr("以下问题和授权图片将发送到远端模型服务，可能产生费用。不会发送聊天记录或其他页面。"));
                                ui.label(tr("图片可能缩小或转码。取消无法撤回已上传的数据。"));
                ui.label(format!("{}: {}", tr("服务地址"), c.cfg.base_url));
                ui.label(format!("{}: {}", tr("模型"), c.cfg.model));
                ui.label(format!("{}: {}", tr("授权图片数量"), c.assets.len()));
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| { ui.label(&c.prompt); });
                ui.add_enabled_ui(c.write_allowed, |ui| { ui.checkbox(&mut c.write_back, tr("允许回答写回原页面")); });
                                if !c.write_allowed || !c.write_back { c.structured_edit = false; }
                                ui.add_enabled_ui(c.write_allowed && c.write_back, |ui| {
                                    ui.checkbox(&mut c.structured_edit, tr("允许读取当前页并新增、修改或删除支持的图形对象"));
                                });
                                if c.structured_edit {
                                    ui.label(tr("当前页支持的图形及文字将发送给模型。手写对象不发送、不修改；图片仅按另行授权发送。不会读取其他页面。"));
                                }
                if !c.assets.is_empty() { ui.checkbox(&mut c.vision_confirmed, tr("我确认此模型支持图片输入，并同意发送这些图片")); }
                ui.horizontal(|ui| {
                    if ui.add_enabled(c.assets.is_empty() || c.vision_confirmed, egui::Button::new(tr("允许发送"))).clicked() { decision = Some(true); }
                    if ui.button(tr("拒绝发送")).clicked() { decision = Some(false); }
                });
            });
    } else if host.busy() && !host.capture_active() {
        egui::Window::new(tr("画板 Agent 处理中"))
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(tr("取消会阻止回答写回，但无法撤回已经上传的数据。"));
                if ui.button(tr("取消画板任务")).clicked() {
                    decision = Some(false);
                }
            });
    }
    match decision {
        Some(true) => host.authorize(true),
        Some(false) => host.cancel(),
        None => {}
    }
}

#[cfg(test)]
#[path = "drawing_host_tests.rs"]
mod tests;
