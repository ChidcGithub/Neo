//! Opt-in floating control. The real app owns the handle; constructing app/test
//! state must not start it. No setting, main-window or persistence dependencies.
#[cfg(all(windows, not(test)))]
use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::Receiver,
    Arc,
};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // OpenDrawing/OpenBlackboard are produced by the native menu callback in production
pub enum Action {
    Wake,
    OpenDrawing,
    OpenBlackboard,
    /// The worker exited unexpectedly. Report once; do not retry every frame.
    Unavailable,
}

pub struct FloatingHandle {
    shared: Arc<Shared>,
    actions: Receiver<Action>,
    thread: Option<JoinHandle<()>>,
    unavailable_reported: AtomicBool,
}

struct Shared {
    stop: AtomicBool,
    visible: AtomicBool,
    hidden: AtomicBool,
    capture_allowed: AtomicBool,
    capture_revision: AtomicU64,
    dead: AtomicBool,
}

impl Shared {
    fn new(capture_allowed: bool) -> Self {
        Self {
            stop: AtomicBool::new(false),
            visible: AtomicBool::new(true),
            hidden: AtomicBool::new(false),
            capture_allowed: AtomicBool::new(capture_allowed),
            capture_revision: AtomicU64::new(0),
            dead: AtomicBool::new(false),
        }
    }

    fn interactive(&self) -> bool {
        self.visible.load(Ordering::Acquire)
            && !self.stop.load(Ordering::Acquire)
            && !self.dead.load(Ordering::Acquire)
    }

    fn may_capture(&self, revision: u64) -> bool {
        self.interactive()
            && self.capture_allowed.load(Ordering::Acquire)
            && self.capture_revision.load(Ordering::Acquire) == revision
    }
}

// A revision also invalidates an in-flight image across a false -> true toggle.
fn refresh_capture(shared: &Shared, revision: &mut u64, glass: &mut Option<Vec<u32>>) -> bool {
    let current = shared.capture_revision.load(Ordering::Acquire);
    let changed = current != *revision;
    *revision = current;
    let clear = changed || !shared.capture_allowed.load(Ordering::Acquire);
    let had_glass = glass.is_some();
    if clear {
        clear_glass(glass);
    }
    changed || (clear && had_glass)
}

fn clear_glass(glass: &mut Option<Vec<u32>>) {
    if let Some(mut pixels) = glass.take() {
        pixels.fill(0);
    }
}

fn finish_or_detach(thread: JoinHandle<()>) {
    // Never wait on a graphics driver or DwmFlush from the app/UI thread.
    if thread.is_finished() {
        let _ = thread.join();
    }
}

impl FloatingHandle {
    /// Starts visible. Only call from real app assembly, never a state constructor.
    /// Windows 10 2004+ is the supported native platform.
    /// Pass false in safe mode: no desktop sampling or compositor flush.
    pub fn start(ctx: egui::Context, capture_allowed: bool) -> Result<Self, String> {
        #[cfg(all(windows, not(test)))]
        {
            let shared = Arc::new(Shared::new(capture_allowed));
            let (send, actions) = mpsc::sync_channel(8);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let worker_shared = shared.clone();
            let thread = std::thread::Builder::new()
                .name("neo-floating".into())
                .spawn(move || native::run(worker_shared, ctx, send, ready_tx))
                .map_err(|e| format!("start floating thread: {e}"))?;
            match ready_rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Ok(())) => Ok(Self {
                    shared,
                    actions,
                    thread: Some(thread),
                    unavailable_reported: AtomicBool::new(false),
                }),
                result => {
                    shared.stop.store(true, Ordering::Release);
                    shared.visible.store(false, Ordering::Release);
                    finish_or_detach(thread);
                    Err(match result {
                        Ok(Err(error)) => error,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            "floating startup timed out after 2 seconds".into()
                        }
                        _ => "floating thread ended during initialization".into(),
                    })
                }
            }
        }
        #[cfg(any(not(windows), test))]
        {
            let _ = (ctx, capture_allowed);
            Err("native floating control is disabled on this platform or in tests".into())
        }
    }

    /// Disables interaction immediately; poll is_hidden() for native hide completion.
    pub fn set_visible(&self, visible: bool) {
        if visible {
            self.shared.hidden.store(false, Ordering::Release);
        }
        self.shared.visible.store(visible, Ordering::Release);
    }

    /// True only after the worker has actually hidden the HWND. A dead/stalled
    /// worker is not implicitly acknowledged; callers must bound barrier waits.
    pub fn is_hidden(&self) -> bool {
        !self.shared.visible.load(Ordering::Acquire) && self.shared.hidden.load(Ordering::Acquire)
    }

    /// Revocation prevents new captures and invalidates any in-flight result.
    /// The worker clears cached glass and redraws gray on its next iteration.
    /// An OS capture already executing cannot be cancelled by this setter.
    pub fn set_capture_allowed(&self, allowed: bool) {
        if self.shared.capture_allowed.swap(allowed, Ordering::AcqRel) != allowed {
            self.shared.capture_revision.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub fn try_recv(&self) -> Option<Action> {
        if self.shared.dead.load(Ordering::Acquire) {
            return (!self.unavailable_reported.swap(true, Ordering::AcqRel))
                .then_some(Action::Unavailable);
        }
        while let Ok(action) = self.actions.try_recv() {
            if self.shared.interactive() {
                return Some(action);
            }
        }
        None
    }
}

impl Drop for FloatingHandle {
    fn drop(&mut self) {
        self.shared.visible.store(false, Ordering::Release);
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            finish_or_detach(thread);
        }
    }
}

#[cfg(windows)]
#[path = "floating_native.rs"]
mod native;

const HOLD_MS: u64 = 500;
const SLOP: f32 = 6.0;
const EXTENT: f32 = 180.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Point {
    x: f32,
    y: f32,
}

impl Point {
    fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presentation {
    Render,
    Move,
    Idle,
}

fn presentation(dirty: bool, origin: Point, presented: Option<Point>) -> Presentation {
    if dirty || presented.is_none() {
        Presentation::Render
    } else if presented != Some(origin) {
        Presentation::Move
    } else {
        Presentation::Idle
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Main,
    Close,
    Ink,
    Board,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Self::Ink => crate::i18n::tr("画板"),
            Self::Board => crate::i18n::tr("黑板"),
            Self::Main | Self::Close => "",
        }
    }
}

fn refresh_language(previous: &mut crate::i18n::Language) -> bool {
    let current = crate::i18n::language();
    let changed = *previous != current;
    *previous = current;
    changed
}

#[derive(Clone, Copy, Debug)]
struct Circle {
    center: Point,
    radius: f32,
    target: Target,
}

fn circles(menu: bool) -> impl Iterator<Item = Circle> {
    [
        Circle {
            center: Point { x: 90.0, y: 90.0 },
            radius: 28.0,
            target: Target::Main,
        },
        Circle {
            center: Point { x: 90.0, y: 28.0 },
            radius: 21.0,
            target: Target::Close,
        },
        Circle {
            center: Point {
                x: 143.694,
                y: 121.0,
            },
            radius: 21.0,
            target: Target::Ink,
        },
        Circle {
            center: Point {
                x: 36.306,
                y: 121.0,
            },
            radius: 21.0,
            target: Target::Board,
        },
    ]
    .into_iter()
    .take(if menu { 4 } else { 1 })
}

/// Frame-rate independent, reversible menu transition; idle requires no redraw.
#[derive(Default)]
struct MenuAnimation {
    progress: f32,
}

impl MenuAnimation {
    fn advance(&mut self, open: bool, seconds: f32) -> bool {
        let previous = self.progress;
        let step = seconds.max(0.0) / 0.18;
        self.progress = (self.progress + if open { step } else { -step }).clamp(0.0, 1.0);
        self.progress != previous
    }

    fn amount(&self) -> f32 {
        let p = self.progress;
        p * p * (3.0 - 2.0 * p)
    }

    fn active(&self, open: bool) -> bool {
        self.progress != if open { 1.0 } else { 0.0 }
    }
}

fn animated_circles(amount: f32) -> impl Iterator<Item = Circle> {
    circles(amount > 0.0).map(move |mut circle| {
        if circle.target != Target::Main {
            let travel = (50.0 + 12.0 * amount) / 62.0;
            circle.center.x = 90.0 + (circle.center.x - 90.0) * travel;
            circle.center.y = 90.0 + (circle.center.y - 90.0) * travel;
            circle.radius *= 0.75 + 0.25 * amount;
        }
        circle
    })
}

fn circle_coverage(point: Point, circle: Circle, scale: f32) -> f32 {
    let distance = point.distance(circle.center);
    if (distance - circle.radius).abs() * scale > 0.75 {
        return if distance < circle.radius { 1.0 } else { 0.0 };
    }
    let mut covered = 0;
    for y in 0..4 {
        for x in 0..4 {
            let sample = Point {
                x: point.x + ((x as f32 + 0.5) / 4.0 - 0.5) / scale,
                y: point.y + ((y as f32 + 0.5) / 4.0 - 0.5) / scale,
            };
            covered += usize::from(sample.distance(circle.center) <= circle.radius);
        }
    }
    covered as f32 / 16.0
}

fn hit(point: Point, menu: bool) -> Option<Target> {
    circles(menu)
        .find(|circle| point.distance(circle.center) <= circle.radius)
        .map(|circle| circle.target)
}

/// Full menu footprint stays inside the monitor work area, including negative
/// virtual-desktop coordinates. Tiny work areas shrink rather than overflow.
fn fit_scale(dpi: u32, width: i32, height: i32) -> f32 {
    (dpi.max(48) as f32 / 96.0)
        .min(4.0)
        .min(width.max(1) as f32 / EXTENT)
        .min(height.max(1) as f32 / EXTENT)
}

fn clamp_origin(point: Point, left: i32, top: i32, right: i32, bottom: i32, size: i32) -> Point {
    Point {
        x: point.x.clamp(left as f32, (right - size).max(left) as f32),
        y: point.y.clamp(top as f32, (bottom - size).max(top) as f32),
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    Press {
        target: Target,
        origin: Point,
        at: u64,
        had_menu: bool,
        long: bool,
    },
    Drag {
        last: Point,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Effect {
    None,
    Wake,
    OpenDrawing,
    OpenBlackboard,
    MenuChanged,
    Drag(Point),
}

struct Gesture {
    phase: Phase,
    menu: bool,
}

impl Default for Gesture {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            menu: false,
        }
    }
}

impl Gesture {
    fn active(&self) -> bool {
        !matches!(self.phase, Phase::Idle)
    }

    fn cancel(&mut self) {
        *self = Self::default();
    }

    fn down(&mut self, target: Target, point: Point, now: u64) {
        if !self.active() {
            self.phase = Phase::Press {
                target,
                origin: point,
                at: now,
                had_menu: self.menu,
                long: false,
            };
        }
    }

    /// Points are screen coordinates in DIPs at the press-time scale. Caller
    /// polls release outside the no-activate window too; release never wakes
    /// after a hold, drag, cancelled press or a menu-dismiss click.
    fn update(&mut self, point: Point, held: bool, over: Option<Target>, now: u64) -> Effect {
        let phase = self.phase;
        if !held {
            self.phase = Phase::Idle;
        }
        match phase {
            Phase::Idle | Phase::Cancelled => Effect::None,
            Phase::Drag { last } => {
                if held {
                    self.phase = Phase::Drag { last: point };
                    Effect::Drag(Point {
                        x: point.x - last.x,
                        y: point.y - last.y,
                    })
                } else {
                    Effect::None
                }
            }
            Phase::Press {
                target,
                origin,
                at,
                had_menu,
                long,
            } => {
                let elapsed = now.saturating_sub(at);
                let moved = point.distance(origin) > SLOP;
                if !held {
                    if moved || long || elapsed >= HOLD_MS || over != Some(target) {
                        return Effect::None;
                    }
                    return match target {
                        Target::Main if had_menu => {
                            self.menu = false;
                            Effect::MenuChanged
                        }
                        Target::Main => Effect::Wake,
                        Target::Close => {
                            self.menu = false;
                            Effect::MenuChanged
                        }
                        Target::Ink | Target::Board => {
                            self.menu = false;
                            if target == Target::Ink {
                                Effect::OpenDrawing
                            } else {
                                Effect::OpenBlackboard
                            }
                        }
                    };
                }
                if target != Target::Main {
                    if moved {
                        self.phase = Phase::Cancelled;
                    }
                    return Effect::None;
                }
                if moved {
                    self.menu = false;
                    self.phase = Phase::Drag { last: point };
                    return Effect::Drag(Point {
                        x: point.x - origin.x,
                        y: point.y - origin.y,
                    });
                }
                if elapsed >= HOLD_MS && !long {
                    self.phase = Phase::Press {
                        target,
                        origin,
                        at,
                        had_menu,
                        long: true,
                    };
                    self.menu = true;
                    return Effect::MenuChanged;
                }
                Effect::None
            }
        }
    }
}

#[cfg(test)]
#[path = "floating_tests.rs"]
mod tests;
