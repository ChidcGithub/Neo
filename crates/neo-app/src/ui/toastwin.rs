//! 独立轻提示：同一份绝对 deadline 队列在 overlay / deferred viewport 间迁移。

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Context, Pos2, Rect, Vec2, ViewportBuilder, ViewportCommand, ViewportId};
use neo_theme::{Theme, ThemeMode};
use neo_ui::{Design, ToastKind};

use super::miniwin::OFFSCREEN;

/// 保留最新三条；每条最多 512 个 Unicode 字符（含省略号）。
const MAX_TOASTS: usize = 3;
const MAX_CHARS: usize = 512;
const POLL: Duration = Duration::from_millis(100);
type Notice = (ToastKind, String, Instant);

fn viewport_id() -> ViewportId {
    ViewportId::from_hash_of("neo-toastwin")
}

fn bounded_text(text: String) -> String {
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(MAX_CHARS - 1).collect();
    if let Some(last) = chars.next() {
        result.push(if chars.next().is_some() { '…' } else { last });
    }
    result
}

#[derive(Default)]
struct Queue {
    entries: Vec<Notice>,
}

impl Queue {
    fn advance(&mut self, pending: &mut Vec<Notice>, now: Instant) -> bool {
        let old_len = self.entries.len();
        self.entries.retain(|(_, _, deadline)| *deadline > now);
        let mut changed = old_len != self.entries.len();
        // 倒序到量即停，不为即将被淘汰的旧文案分配副本；drain 仍消费全部输入。
        let mut incoming: Vec<_> = pending
            .drain(..)
            .rev()
            .filter(|(_, _, deadline)| *deadline > now)
            .take(MAX_TOASTS)
            .map(|(kind, text, deadline)| (kind, bounded_text(text), deadline))
            .collect();
        if !incoming.is_empty() {
            changed = true;
            incoming.reverse();
            self.entries.extend(incoming);
            let excess = self.entries.len().saturating_sub(MAX_TOASTS);
            self.entries.drain(..excess);
        }
        // 不保留输入 burst 留下的大容量缓冲。
        if pending.capacity() > MAX_TOASTS * 4 {
            *pending = Vec::new();
        }
        changed
    }
}

/// 坐标始终来自主屏，不借用可能位于副屏的主窗口 monitor_size / DPI。
#[cfg(all(windows, not(test)))]
fn primary_screen(_ctx: &Context) -> Rect {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let _dpi = neo_tools::tools::screen::physical_pixels().ok();
    let ppp = neo_tools::tools::screen::dpi_scale_at(0, 0).unwrap_or(1.0) as f32;
    let size = unsafe {
        Vec2::new(
            GetSystemMetrics(SM_CXSCREEN) as f32,
            GetSystemMetrics(SM_CYSCREEN) as f32,
        )
    };
    Rect::from_min_size(Pos2::ZERO, size / ppp.max(0.1))
}

#[cfg(any(not(windows), test))]
fn primary_screen(_ctx: &Context) -> Rect {
    // headless 测试绝不读 Win32；几何测试直接注入目标屏矩形。
    Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0))
}

fn stack_rect(screen: Rect, theme: Theme, count: usize) -> Rect {
    let m = theme.metrics;
    let margin = m
        .s(16.0)
        .min(screen.width() * 0.1)
        .min(screen.height() * 0.1);
    let width = m.s(560.0).min((screen.width() - margin * 2.0).max(1.0));
    // 字体栅格化会把每行高度向像素边界取整，预留三行各 2pt。
    let row = (theme.typo.label * 4.5 + m.s(20.0) + 6.0).max(m.s(40.0));
    let height = (row * count as f32 + m.s(10.0) * count.saturating_sub(1) as f32 + margin * 2.0)
        .min((screen.height() - margin * 2.0).max(1.0));
    Rect::from_min_size(
        Pos2::new(
            screen.center().x - width * 0.5,
            screen.bottom() - margin - height,
        ),
        Vec2::new(width, height),
    )
}

#[derive(Clone)]
struct Snapshot {
    entries: Vec<Notice>,
    theme: Theme,
    rect: Rect,
}

impl Snapshot {
    fn paint(&self, ui: &mut egui::Ui, now: Instant) {
        let d = Design::new(self.theme);
        let rect = ui.max_rect().shrink(d.m().s(12.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::bottom_up(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(rect));
                ui.spacing_mut().item_spacing.y = d.m().s(10.0);
                for (kind, text, deadline) in &self.entries {
                    // 主线程即使暂时未 tick，也绝不画已经过期的旧快照。
                    if *deadline > now {
                        neo_ui::feedback::toast_wrapped(ui, &d, *kind, text, rect.width());
                    }
                }
            },
        );
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Appearance {
    mode: ThemeMode,
    scale: f32,
    screen: Rect,
}

/// 每个 app 一只；必须在 logic-only 也会经过的 tick 调用，不依赖主窗可见性。
#[derive(Default)]
pub struct ToastWin {
    queue: Queue,
    appearance: Option<Appearance>,
    snapshot: Option<Arc<Snapshot>>,
    overlay_key: Option<usize>,
    fallback_started: bool,
    fallback_rect: Option<Rect>,
}

impl ToastWin {
    fn update(
        &mut self,
        pending: &mut Vec<Notice>,
        theme: Theme,
        screen: Rect,
        now: Instant,
    ) -> bool {
        let changed = self.queue.advance(pending, now);
        let appearance = Appearance {
            mode: theme.mode,
            scale: theme.metrics.scale(),
            screen,
        };
        let changed = changed || self.appearance != Some(appearance);
        if changed {
            self.appearance = Some(appearance);
            self.snapshot = (!self.queue.entries.is_empty()).then(|| {
                Arc::new(Snapshot {
                    entries: self.queue.entries.clone(),
                    theme,
                    rect: stack_rect(screen, theme, self.queue.entries.len()),
                })
            });
        }
        changed
    }

    fn builder(rect: Option<Rect>) -> ViewportBuilder {
        ViewportBuilder::default()
            .with_title("Neo 提示")
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_active(false)
            .with_transparent(true)
            .with_mouse_passthrough(true)
            .with_visible(true)
            .with_position(rect.map_or(OFFSCREEN, |r| r.min))
            .with_inner_size(rect.map_or(Vec2::splat(1.0), |r| r.size()))
    }

    fn register_fallback(&mut self, ctx: &Context, snapshot: Option<Arc<Snapshot>>) {
        let rect = snapshot.as_ref().map(|s| s.rect);
        if !self.fallback_started || self.fallback_rect != rect {
            ctx.send_viewport_cmd_to(
                viewport_id(),
                ViewportCommand::OuterPosition(rect.map_or(OFFSCREEN, |r| r.min)),
            );
            ctx.send_viewport_cmd_to(
                viewport_id(),
                ViewportCommand::InnerSize(rect.map_or(Vec2::splat(1.0), |r| r.size())),
            );
            self.fallback_rect = rect;
        }
        self.fallback_started = true;
        // 常驻休眠，不用 Visible(false) / Close：下次恢复不经历透明 surface 黑帧。
        let suspended = crate::app::desktop_viewport(ctx, viewport_id());
        ctx.show_viewport_deferred(viewport_id(), Self::builder(rect).with_visible(!suspended), move |ui, _| {
            if crate::app::desktop_suspended(ui.ctx()) { return; }
            let Some(snapshot) = &snapshot else { return };
            let now = Instant::now();
            snapshot.paint(ui, now);
            if snapshot.entries.iter().any(|(_, _, deadline)| *deadline <= now) {
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
            if snapshot
                .entries
                .iter()
                .all(|(_, _, deadline)| *deadline <= now)
            {
                ui.ctx()
                    .send_viewport_cmd_to(viewport_id(), ViewportCommand::OuterPosition(OFFSCREEN));
                ui.ctx().send_viewport_cmd_to(
                    viewport_id(),
                    ViewportCommand::InnerSize(Vec2::splat(1.0)),
                );
            } else {
                ui.ctx().request_repaint_after(POLL);
            }
        });
    }

    pub fn tick(
        &mut self,
        ctx: &Context,
        pending: &mut Vec<Notice>,
        theme: Theme,
        overlay: Option<&neo_overlay::OverlayHandle>,
    ) {
        let now = Instant::now();
        let changed = self.update(pending, theme, primary_screen(ctx), now);
        let overlay = overlay.filter(|layer| layer.is_alive());
        let key = overlay.map(|layer| layer as *const _ as usize);
        let backend_changed = self.overlay_key != key;
        self.overlay_key = key;
        if let Some(layer) = overlay {
            if changed || backend_changed {
                let card = self.snapshot.as_ref().map(|snapshot| {
                    let snapshot = snapshot.clone();
                    let rect = snapshot.rect;
                    let root = ctx.clone();
                    neo_overlay::Card::passive(
                        [rect.min.x, rect.min.y, rect.width(), rect.height()],
                        move |ui| {
                            if crate::app::desktop_suspended(&root) { return; }
                            let now = Instant::now();
                            snapshot.paint(ui, now);
                            if snapshot
                                .entries
                                .iter()
                                .any(|(_, _, deadline)| *deadline <= now)
                            {
                                // set_card 必须留给 tick；此处正持 overlay 卡片锁。
                                root.request_repaint_of(ViewportId::ROOT);
                            }
                        },
                    )
                });
                layer.set_card(neo_overlay::card_id::TOAST, card);
            }
            if self.fallback_started {
                self.register_fallback(ctx, None);
            }
        } else {
            self.register_fallback(ctx, self.snapshot.clone());
        }
        if let Some(deadline) = self
            .queue
            .entries
            .iter()
            .map(|(_, _, deadline)| *deadline)
            .min()
        {
            // 低频健康检查也让 overlay 失效后无需用户输入即可切到 fallback。
            ctx.request_repaint_after_for(deadline.saturating_duration_since(now).min(POLL), ViewportId::ROOT);
        }
    }
}

#[cfg(test)]
#[path = "toastwin_tests.rs"]
mod tests;
