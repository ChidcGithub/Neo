use super::protocol::{self, Envelope, Pending, Session};
use super::*;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread;

const MAX_WORKERS: usize = 4;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

/// Retained by the supervisor AND every pipe thread, even after child exit.
/// A broken peer/inherited pipe therefore consumes capacity instead of permitting
/// unlimited retries to accumulate detached threads. No timeout implies a kill.
pub(super) struct WorkerPermit;

impl WorkerPermit {
    pub(super) fn acquire() -> Result<Arc<Self>, String> {
        WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_WORKERS).then_some(n + 1)
            })
            .map_err(|_| {
                "runtime process/cleanup limit reached; wait for existing runtimes to exit"
                    .to_string()
            })?;
        Ok(Arc::new(Self))
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) struct Output {
    pub events: SyncSender<Event>,
    pub failure: SyncSender<String>,
    pub shared: Arc<Shared>,
    pub ctx: egui::Context,
}

impl Output {
    fn emit(&self, event: Event) -> Result<(), String> {
        self.events
            .try_send(event)
            .map_err(|_| "runtime event queue full or consumer dropped".to_string())?;
        self.ctx.request_repaint();
        Ok(())
    }

    fn fail(&self, message: String) {
        self.shared.usable.store(false, Ordering::Release);
        let _ = self.failure.try_send(message);
        self.ctx.request_repaint();
    }

    fn exited(&self) {
        self.shared.usable.store(false, Ordering::Release);
        self.shared.alive.store(false, Ordering::Release);
        self.shared.exited.store(true, Ordering::Release);
        self.ctx.request_repaint();
    }
}

/// Bounded byte framing, including a possible trailing CR. Never read_line/read_until.
/// A bad/oversized line is consumed before returning an error, so framing is preserved.
pub(super) fn read_frame<R: BufRead>(
    reader: &mut R,
) -> io::Result<Option<Result<Vec<u8>, String>>> {
    let mut bytes = Vec::with_capacity(MAX_LINE + 1);
    let mut oversized = false;
    let mut any = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            if !any {
                return Ok(None);
            }
            // A CR only counts as framing if followed by LF.
            return Ok(Some(if oversized || bytes.len() > MAX_LINE {
                Err("line_too_long".into())
            } else {
                Ok(bytes)
            }));
        }
        any = true;
        let end = chunk.iter().position(|b| *b == b'\n');
        let count = end.unwrap_or(chunk.len());
        if !oversized {
            if bytes.len() + count > MAX_LINE + 1 {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&chunk[..count]);
            }
        }
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Some(if oversized || bytes.len() > MAX_LINE {
                Err("line_too_long".into())
            } else {
                Ok(bytes)
            }));
        }
    }
}

enum Input {
    Frame(Vec<u8>),
    Eof,
}

pub(super) struct WriteFrame {
    pub bytes: Vec<u8>,
    pub deadline: Option<Instant>,
    guard: Option<ReplyGuard>,
}

impl WriteFrame {
    pub(super) fn write_to(
        mut self,
        writer: &mut impl Write,
        stopping: &AtomicBool,
    ) -> io::Result<()> {
        // Select one complete frame before the first write; never switch on a partial write.
        if let Some(guard) = &self.guard {
            guard.check(&mut self.bytes);
        }
        write_frame(writer, &self.bytes, stopping)
    }
}

fn enqueue(
    writer: &SyncSender<WriteFrame>,
    bytes: Vec<u8>,
    deadline: Option<Instant>,
) -> Result<(), String> {
    enqueue_guarded(writer, bytes, deadline, None)
}

fn enqueue_guarded(
    writer: &SyncSender<WriteFrame>,
    mut bytes: Vec<u8>,
    deadline: Option<Instant>,
    guard: Option<ReplyGuard>,
) -> Result<(), String> {
    if let Some(guard) = &guard {
        guard.check(&mut bytes);
    }
    writer
        .try_send(WriteFrame {
            bytes,
            deadline,
            guard,
        })
        .map_err(|_| "runtime stdin queue full or disconnected".into())
}

impl Session {
    pub(super) fn command(
        &mut self,
        command: super::Command,
        writer: &SyncSender<WriteFrame>,
        out: &Output,
    ) -> Result<(), String> {
        if !out.shared.usable.load(Ordering::Acquire) {
            return Err("runtime disconnected".into());
        }
        match command {
            super::Command::Request(request) => self.accept(request, writer, out),
            super::Command::HostReply { id, bytes, guard } => {
                let mut host = out
                    .shared
                    .host_requests
                    .lock()
                    .map_err(|_| "host request lock poisoned")?;
                if host.ids.get(&id) != Some(&true) || host.inflight == 0 {
                    return Err("invalid queued host response".into());
                }
                // A final reply must not silently expire while waiting behind another frame.
                enqueue_guarded(writer, bytes, None, Some(guard))?;
                host.inflight -= 1;
                Ok(())
            }
        }
    }

    pub(super) fn accept(
        &mut self,
        request: Request,
        writer: &SyncSender<WriteFrame>,
        out: &Output,
    ) -> Result<(), String> {
        let error = if Instant::now() >= request.deadline {
            Some(RpcError::local(
                "timeout",
                "request expired before dispatch; not proof of cancellation",
            ))
        } else if !self.configured {
            Some(RpcError::local(
                "not_configured",
                "wait for Ready before requesting",
            ))
        } else if !self
            .methods
            .as_ref()
            .is_some_and(|m| m.contains(&request.method))
        {
            Some(RpcError::local(
                "method_not_found",
                "method was not advertised by ready.methods",
            ))
        } else if self.pending.len() >= QUEUE {
            Some(RpcError::local(
                "busy",
                "runtime pending request limit reached",
            ))
        } else {
            None
        };
        if let Some(error) = error {
            return out.emit(Event::Response {
                id: request.id,
                method: request.method,
                result: Err(error),
            });
        }
        enqueue(
            writer,
            protocol::request_frame(&request.id, &request.method, &request.params)?,
            Some(request.deadline),
        )?;
        self.pending.insert(
            request.id.clone(),
            Pending {
                request,
                handshake: false,
            },
        );
        Ok(())
    }

    pub(super) fn frame(
        &mut self,
        frame: &[u8],
        writer: &SyncSender<WriteFrame>,
        out: &Output,
    ) -> Result<(), String> {
        match protocol::decode(frame)? {
            Envelope::Request { id, method, params } => {
                if !self.configured {
                    return Err("runtime host request before Ready".into());
                }
                let mut host = out
                    .shared
                    .host_requests
                    .lock()
                    .map_err(|_| "host request lock poisoned")?;
                if host.ids.contains_key(&id) {
                    return Err("duplicate runtime request ID".into());
                }
                if host.ids.len() >= HOST_ID_BUDGET {
                    return Err("runtime host request ID budget exhausted".into());
                }
                if host.inflight >= QUEUE {
                    return Err("runtime host request inflight limit reached".into());
                }
                host.ids.insert(id.clone(), false);
                host.inflight += 1;
                // No service work or response is awaited on the reader/dispatcher path.
                out.emit(Event::HostRequest { id, method, params })?;
            }
            Envelope::Event { name, data } => match name.as_str() {
                "ready" => {
                    if self.methods.is_some() {
                        return Err("duplicate ready event".into());
                    }
                    self.methods = Some(protocol::ready(&data, self.kind)?);
                    let request = Request {
                        id: format!("neo:{}:0", self.generation),
                        method: "configure".into(),
                        params: self.permissions.params(),
                        deadline: Instant::now() + REQUEST_TIMEOUT,
                    };
                    enqueue(
                        writer,
                        protocol::request_frame(&request.id, &request.method, &request.params)?,
                        Some(request.deadline),
                    )?;
                    self.pending.insert(
                        request.id.clone(),
                        Pending {
                            request,
                            handshake: true,
                        },
                    );
                }
                "state_changed" | "document_changed" => {
                    let state = State::parse(&data, self.kind)?;
                    self.closed = state.closed;
                    if self.configured {
                        out.emit(Event::StateChanged(state))?;
                    }
                }
                "protocol_error" => {
                    return Err(format!("runtime reported protocol_error: {}", data["code"]))
                }
                // window.requested is for the runtime's native adapter, never acked here.
                _ => {}
            },
            Envelope::Response { id, result } => {
                if !id.starts_with(&format!("neo:{}:", self.generation)) {
                    return Ok(());
                }
                let Some(pending) = self.pending.remove(&id) else {
                    return Ok(());
                };
                if Instant::now() >= pending.request.deadline {
                    return self.expired(pending, out);
                }
                if pending.handshake {
                    let result = result
                        .map_err(|e| format!("configure failed: {}: {}", e.code, e.message))?;
                    let state = State::parse(&result, self.kind)?;
                    if !state.configured || state.closed || state.permissions != self.permissions {
                        return Err(
                            "configure did not confirm requested restricted permissions".into()
                        );
                    }
                    self.configured = true;
                    out.emit(Event::Ready(state))?;
                } else {
                    if let Ok(value) = &result {
                        if let Some(state) =
                            protocol::state_result(&pending.request.method, value, self.kind)?
                        {
                            if pending.request.method == "configure"
                                && (!state.configured
                                    || value["permissions"] != pending.request.params)
                            {
                                return Err("configure response permissions mismatch".into());
                            }
                            if pending.request.method == "close" && !state.closed {
                                return Err("close response did not confirm closed state".into());
                            }
                            self.closed = state.closed;
                            out.emit(Event::StateChanged(state))?;
                        }
                    }
                    out.emit(Event::Response {
                        id,
                        method: pending.request.method,
                        result,
                    })?;
                }
            }
        }
        Ok(())
    }

    fn expired(&self, pending: Pending, out: &Output) -> Result<(), String> {
        if pending.handshake {
            return Err("configure timed out; runtime not ready".into());
        }
        out.emit(Event::Response {
            id: pending.request.id,
            method: pending.request.method,
            result: Err(RpcError::local(
                "timeout",
                "runtime response timed out; operation may still complete",
            )),
        })
    }

    pub(super) fn expire(&mut self, now: Instant, out: &Output) -> Result<(), String> {
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= p.request.deadline)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            let pending = self.pending.remove(&id).unwrap();
            self.expired(pending, out)?;
        }
        Ok(())
    }
}

pub(super) fn run(
    kind: BoardKind,
    path: PathBuf,
    generation: u64,
    permissions: Permissions,
    requests: Receiver<super::Command>,
    out: Output,
    permit: Arc<WorkerPermit>,
) {
    let mut command = Command::new(&path);
    command
        .args(["--gui", "--hosted"])
        .current_dir(path.parent().expect("validated executable parent"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: suppress console, not runtime GUI.
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            out.fail(format!("cannot spawn hosted runtime: {e}"));
            out.exited();
            return;
        }
    };
    let stdin = DisconnectWriter {
        inner: child.stdin.take().unwrap(),
        live: out.shared.usable.clone(),
    };
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (input_tx, input) = mpsc::sync_channel(QUEUE);
    let (write_tx, writes) = mpsc::sync_channel::<WriteFrame>(QUEUE);
    let (fault_tx, faults) = mpsc::sync_channel::<String>(1);
    let stopping = Arc::new(AtomicBool::new(false));

    let read_permit = permit.clone();
    let read_fault = fault_tx.clone();
    let read_stop = stopping.clone();
    let read_live = out.shared.usable.clone();
    let reader = thread::Builder::new()
        .name(format!("neo-board-stdout-{generation}"))
        .spawn(move || {
            let _permit = read_permit;
            let mut reader = BufReader::with_capacity(8192, stdout);
            loop {
                if read_stop.load(Ordering::Acquire) {
                    let _ = io::copy(&mut reader, &mut io::sink());
                    return;
                }
                let next = match read_frame(&mut reader) {
                    Ok(Some(Ok(frame))) => Input::Frame(frame),
                    Ok(Some(Err(error))) => {
                        read_live.store(false, Ordering::Release);
                        let _ = read_fault.try_send(error);
                        break;
                    }
                    Ok(None) => {
                        read_live.store(false, Ordering::Release);
                        Input::Eof
                    }
                    Err(e) => {
                        read_live.store(false, Ordering::Release);
                        let _ = read_fault.try_send(format!("runtime stdout: {e}"));
                        break;
                    }
                };
                let eof = matches!(next, Input::Eof);
                // Bounded backpressure with a deadline-free try_send: flooding is a fatal
                // protocol connection failure rather than unbounded memory or reader deadlock.
                if input_tx.try_send(next).is_err() {
                    read_live.store(false, Ordering::Release);
                    let _ = read_fault.try_send("runtime stdout queue full or disconnected".into());
                    break;
                }
                if eof {
                    return;
                }
            }
            let _ = io::copy(&mut reader, &mut io::sink());
        });
    let log_permit = permit.clone();
    let logger = thread::Builder::new()
        .name(format!("neo-board-stderr-{generation}"))
        .spawn(move || {
            let _permit = log_permit;
            // Deliberately discard: no parsing, retention, paths in telemetry or uploads.
            let _ = io::copy(&mut BufReader::with_capacity(8192, stderr), &mut io::sink());
        });
    let write_permit = permit.clone();
    let write_stop = stopping.clone();
    let writer = thread::Builder::new()
        .name(format!("neo-board-stdin-{generation}"))
        .spawn(move || {
            let _permit = write_permit;
            let mut stdin = stdin;
            while !write_stop.load(Ordering::Acquire) {
                match writes.recv_timeout(Duration::from_millis(20)) {
                    Ok(frame) => {
                        if write_stop.load(Ordering::Acquire) {
                            break;
                        }
                        if frame
                            .deadline
                            .is_some_and(|deadline| Instant::now() >= deadline)
                        {
                            continue;
                        }
                        if let Err(e) = frame.write_to(&mut stdin, &write_stop) {
                            let _ = fault_tx.try_send(format!("runtime stdin: {e}"));
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            // The only stdin owner closes the pipe here, never on the UI thread.
        });

    let mut session = Session {
        kind,
        generation,
        permissions,
        methods: None,
        configured: false,
        closed: false,
        pending: Default::default(),
    };
    let ready_deadline = Instant::now() + READY_TIMEOUT;
    let mut observed_exit = None;
    let io_started = reader.is_ok() && logger.is_ok() && writer.is_ok();
    let result = if !io_started {
        Err("cannot start runtime I/O threads".to_string())
    } else {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
            loop {
                if out.shared.stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                if let Ok(error) = faults.try_recv() {
                    return Err(error);
                }
                if session.methods.is_none() && Instant::now() >= ready_deadline {
                    return Err("runtime ready timed out (10s)".into());
                }
                // Limit batches so timeouts and Drop cannot starve under a busy peer.
                for _ in 0..QUEUE {
                    match input.try_recv() {
                        Ok(Input::Frame(bytes)) => session.frame(&bytes, &write_tx, &out)?,
                        Ok(Input::Eof) | Err(TryRecvError::Disconnected) => {
                            if session.closed && session.pending.is_empty() {
                                return Ok(());
                            }
                            return Err(
                                "runtime stdout disconnected; outstanding requests invalidated"
                                    .into(),
                            );
                        }
                        Err(TryRecvError::Empty) => break,
                    }
                }
                session.expire(Instant::now(), &out)?;
                for _ in 0..QUEUE {
                    match requests.try_recv() {
                        Ok(command) => session.command(command, &write_tx, &out)?,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return Ok(()),
                    }
                }
                // Give the reader time to deliver final responses, but don't let a
                // descendant retaining stdout hide an already-exited child forever.
                if child
                    .try_wait()
                    .map_err(|e| format!("runtime wait: {e}"))?
                    .is_some()
                {
                    out.shared.usable.store(false, Ordering::Release);
                    let since = observed_exit.get_or_insert_with(Instant::now);
                    if since.elapsed() >= Duration::from_millis(100) {
                        if session.closed && session.pending.is_empty() {
                            return Ok(());
                        }
                        return Err("runtime exited; outstanding requests invalidated".into());
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        }))
        .unwrap_or_else(|_| {
            Err("runtime protocol worker panicked; disconnecting and reaping".into())
        })
    };
    if let Err(error) = result {
        out.fail(error);
    }
    out.shared.usable.store(false, Ordering::Release);
    session.pending.clear();
    stopping.store(true, Ordering::Release);
    drop(write_tx);
    drop(input);
    drop(requests);

    let mut writer = writer.ok();
    let mut reaped = false;
    let mut wait_error_reported = false;
    // Reap independently of writer completion: cancellation can fail, or a descendant
    // can retain stdin. Never let that suppress notification of an actual child exit.
    loop {
        if let Some(handle) = writer.as_ref() {
            if handle.is_finished() {
                let _ = writer.take().unwrap().join();
            } else {
                #[cfg(windows)]
                cancel_writer(handle);
            }
        }
        if !reaped {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        out.fail(format!("runtime exited with {status}"));
                    }
                    out.exited();
                    reaped = true;
                }
                Ok(None) => {}
                Err(e) => {
                    if !wait_error_reported {
                        out.fail(format!("cannot reap runtime (will retry): {e}"));
                        wait_error_reported = true;
                    }
                }
            }
        }
        if reaped && writer.is_none() {
            break;
        }
        // A refusing/dirty GUI may stay alive indefinitely. One bounded supervisor
        // retains ownership and retries; it never spawns replacement cleanup threads.
        thread::sleep(Duration::from_millis(20));
    }
    // Reader handles are detached, but keep the permit until the pipe actually closes.
    drop(permit);
}

// Drop invalidates the token before the owned stdin is closed, including write failure,
// panic and thread-spawn failure. Workers need not wait for the UI to consume Failed.
pub(super) struct DisconnectWriter<W: Write> {
    pub inner: W,
    pub live: Arc<AtomicBool>,
}

impl<W: Write> Write for DisconnectWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.live.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let result = self.inner.write(bytes);
        if matches!(&result, Ok(0))
            || result
                .as_ref()
                .is_err_and(|e| e.kind() != io::ErrorKind::Interrupted)
        {
            self.live.store(false, Ordering::Release);
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.live.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let result = self.inner.flush();
        if result.is_err() {
            self.live.store(false, Ordering::Release);
        }
        result
    }
}

impl<W: Write> Drop for DisconnectWriter<W> {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
    }
}

fn write_frame(writer: &mut impl Write, mut bytes: &[u8], stopping: &AtomicBool) -> io::Result<()> {
    // Unlike write_all, check shutdown before retrying Interrupted (including a
    // cancelled synchronous write). Otherwise cancellation may re-enter the pipe.
    while !bytes.is_empty() {
        if stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    if !stopping.load(Ordering::Acquire) {
        writer.flush()?;
    }
    Ok(())
}

#[cfg(windows)]
fn cancel_writer(writer: &thread::JoinHandle<()>) {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CancelSynchronousIo(thread: *mut std::ffi::c_void) -> i32;
    }
    // JoinHandle owns a live thread handle for the duration of this call.
    unsafe {
        CancelSynchronousIo(writer.as_raw_handle());
    }
}
