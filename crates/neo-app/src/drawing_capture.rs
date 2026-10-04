//! In-memory, user-selected capture. No upload, files, clipboard or model access.
//!
//! Integration: declare `mod drawing_capture;` in the app. Before `start`, the
//! manager MUST have confirmed Neo and every other board are hidden, and the
//! requesting runtime must have acknowledged `windows_hidden_confirmed`. Keep
//! those hide leases until `is_finished()` / `poll()` acknowledges actual exit.
//! `live` belongs to that connection: set it false permanently on disconnect.
//! This module deliberately cannot infer window visibility from a delay.
//!
//! Windows requires windows-sys feature `Win32_Graphics_Dwm` in addition to the
//! app's existing Gdi, HiDpi, LibraryLoader, Input and WindowsAndMessaging features.
//! The 60-second deadline is cooperative: an in-flight OS capture/DwmFlush is
//! never forcibly killed; its output is discarded if cancellation/expiry wins.

use neo_tools::tools::screen::{Rect, Shot};
use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(all(windows, not(test)))]
#[path = "drawing_capture_native.rs"]
mod native;
#[cfg(test)]
#[path = "drawing_capture_tests.rs"]
mod tests;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_SIDE: i32 = 8192;
const MAX_RGBA: usize = 32 * 1024 * 1024;
const MAX_PNG: usize = 8 * 1024 * 1024;
const CANCELLED: &str = "截图已取消";
type CaptureResult = Result<Vec<u8>, String>;

// The global owns the JoinHandle even after the client drops its handle. A new
// flight can only reap an actually finished thread, not a worker's early flag.
#[cfg(all(windows, not(test)))]
static FLIGHT: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

struct Worker {
    thread: Option<JoinHandle<()>>,
    reaped: bool,
}
struct Shared {
    live: Arc<AtomicBool>,
    cancel: AtomicBool,
    deadline: Instant,
    ctx: egui::Context,
    worker: Mutex<Worker>,
    result: Mutex<Option<CaptureResult>>,
}
impl Shared {
    fn new(live: Arc<AtomicBool>, ctx: egui::Context) -> Self {
        Self {
            live,
            cancel: AtomicBool::new(false),
            deadline: Instant::now() + TIMEOUT,
            ctx,
            worker: Mutex::new(Worker {
                thread: None,
                reaped: false,
            }),
            result: Mutex::new(None),
        }
    }

    fn check(&self) -> Result<(), String> {
        if !self.live.load(Ordering::Acquire) {
            self.cancel.store(true, Ordering::Release);
        }
        if self.cancel.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        if Instant::now() >= self.deadline {
            self.cancel.store(true, Ordering::Release);
            return Err("截图超时（60 秒）".into());
        }
        Ok(())
    }

    fn publish(&self, result: CaptureResult) {
        let mut slot = self.result.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(self.check().and(result));
        self.ctx.request_repaint();
    }

    fn finished(&self) -> bool {
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if worker.reaped {
            return true;
        }
        if !worker.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            return false;
        }
        // is_finished guarantees join does not wait for native cleanup.
        let panicked = worker.thread.take().unwrap().join().is_err();
        if panicked {
            self.publish(Err("截图工作线程异常退出".into()));
        }
        worker.reaped = true;
        true
    }
}

pub struct CaptureHandle {
    shared: Arc<Shared>,
}
impl CaptureHandle {
    /// Caller must already hold confirmed hide leases; see module contract.
    /// Test builds always reject this entry point without creating a GUI thread.
    pub fn start(live: Arc<AtomicBool>, ctx: egui::Context) -> Result<Self, String> {
        #[cfg(all(windows, not(test)))]
        {
            Self::spawn(live, ctx, &FLIGHT, native::run)
        }
        #[cfg(any(not(windows), test))]
        {
            let _ = (live, ctx);
            Err("此构建禁用真实框选截图".into())
        }
    }

    fn spawn(
        live: Arc<AtomicBool>,
        ctx: egui::Context,
        flight: &Mutex<Option<Arc<Shared>>>,
        run: impl FnOnce(&Shared) -> CaptureResult + Send + 'static,
    ) -> Result<Self, String> {
        let mut flight = flight.lock().unwrap_or_else(|e| e.into_inner());
        if flight.as_ref().is_some_and(|old| !old.finished()) {
            return Err("已有截图线程尚未退出".into());
        }
        *flight = None;
        let shared = Arc::new(Shared::new(live, ctx));
        shared.check()?;
        let task = shared.clone();
        let thread = thread::Builder::new()
            .name("drawing-capture".into())
            .spawn(move || {
                let result = task.check().and_then(|()| run(&task));
                task.publish(result);
            })
            .map_err(|e| format!("无法启动截图线程：{e}"))?;
        shared
            .worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .thread = Some(thread);
        *flight = Some(shared.clone());
        Ok(Self { shared })
    }

    /// Signals only: do not restore windows or acknowledge cancellation yet.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release);
        // Also discard a completed-but-unconsumed PNG immediately.
        let mut slot = self.shared.result.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            *slot = Some(Err(CANCELLED.into()));
        }
        self.shared.ctx.request_repaint();
    }

    /// Returns the result once, and only after the worker has actually exited.
    /// Loss of connection/cancellation also suppresses an already queued PNG.
    pub fn poll(&mut self) -> Option<CaptureResult> {
        if !self.is_finished() {
            return None;
        }
        let mut slot = self.shared.result.lock().unwrap_or_else(|e| e.into_inner());
        slot.take().map(|result| self.shared.check().and(result))
    }

    /// True only after native cleanup AND actual worker termination, even on error.
    pub fn is_finished(&self) -> bool {
        if !self.shared.live.load(Ordering::Acquire) {
            self.cancel();
        }
        let finished = self.shared.finished();
        if !finished {
            // Publication can request a frame just before thread termination.
            self.shared
                .ctx
                .request_repaint_after(Duration::from_millis(16));
        }
        finished
    }

    /// Manager tests can explicitly acknowledge cleanup without any desktop APIs.
    /// `finish` represents native exit, not merely a cancel request.
    #[cfg(test)]
    pub(crate) fn fake(live: Arc<AtomicBool>, ctx: egui::Context) -> (Self, FakeCapture) {
        let shared = Arc::new(Shared::new(live, ctx));
        (
            Self {
                shared: shared.clone(),
            },
            FakeCapture { shared },
        )
    }
}
impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
pub(crate) struct FakeCapture {
    shared: Arc<Shared>,
}
#[cfg(test)]
impl FakeCapture {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.shared.check().is_err()
    }
    pub(crate) fn finish(self, result: CaptureResult) {
        self.shared.publish(result);
        self.shared
            .worker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reaped = true;
        self.shared.ctx.request_repaint();
    }
}

fn rgba_bytes(width: i32, height: i32) -> Result<usize, String> {
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
        return Err("框选宽高必须为 1…8192 物理像素".into());
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= MAX_RGBA)
        .ok_or_else(|| "框选 RGBA 超过 32 MiB，请缩小区域".into())
}

/// Physical, half-open ROI. Validate the actual selection, never a downscale.
fn selection(a: (i32, i32), b: (i32, i32), desktop: Rect) -> Result<Rect, String> {
    let right = desktop.x.checked_add(desktop.width).ok_or("桌面坐标溢出")?;
    let bottom = desktop
        .y
        .checked_add(desktop.height)
        .ok_or("桌面坐标溢出")?;
    if desktop.width <= 0 || desktop.height <= 0 {
        return Err("桌面尺寸无效".into());
    }
    for (x, y) in [a, b] {
        if x < desktop.x || x > right || y < desktop.y || y > bottom {
            return Err("框选超出虚拟桌面".into());
        }
    }
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    let width = a.0.max(b.0).checked_sub(x).ok_or("框选坐标溢出")?;
    let height = a.1.max(b.1).checked_sub(y).ok_or("框选坐标溢出")?;
    rgba_bytes(width, height)?;
    Ok(Rect {
        x,
        y,
        width,
        height,
    })
}

struct PngSink<'a> {
    bytes: Vec<u8>,
    shared: &'a Shared,
}
impl Write for PngSink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.shared.check().map_err(io::Error::other)?;
        if buf.len() > MAX_PNG.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("PNG 超过 8 MiB，请缩小区域"));
        }
        self.bytes
            .try_reserve_exact(buf.len())
            .map_err(io::Error::other)?;
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.shared.check().map_err(io::Error::other)
    }
}

fn encode(shot: Shot, rect: Rect, shared: &Shared) -> CaptureResult {
    use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};
    shared.check()?;
    let bytes = rgba_bytes(rect.width, rect.height)?;
    if shot.width != rect.width as u32
        || shot.height != rect.height as u32
        || shot.rgba.len() != bytes
    {
        return Err("截图尺寸或 RGBA 长度与框选不一致".into());
    }
    let mut sink = PngSink {
        bytes: Vec::new(),
        shared,
    };
    PngEncoder::new(&mut sink)
        .write_image(
            &shot.rgba,
            shot.width,
            shot.height,
            ExtendedColorType::Rgba8,
        )
        .map_err(|e| format!("PNG 编码失败：{e}"))?;
    shared.check()?;
    Ok(sink.bytes)
}
