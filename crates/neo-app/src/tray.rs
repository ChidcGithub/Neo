//! System tray: icon, menu, and event polling.
//!
//! The tray has no winit wake-up source, so [`Tray::poll`] is driven by the
//! app's low-frequency tick (including while the main window is hidden).

use std::time::{Duration, Instant};

use crate::brand;
use crate::diagnostics::{record, Level};
use crate::i18n::tr;

/// Interval between the two wordmark styles (`neo` and `neo + spark`).
const ROTATE: Duration = Duration::from_secs(3);

/// A user action collected from one tray poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    Quit,
}

/// Owns the notification-area icon and its menu.
pub struct Tray {
    icon: Option<tray_icon::TrayIcon>,
    labels: Option<(tray_icon::menu::MenuItem, tray_icon::menu::MenuItem)>,
    show_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
    /// Whether the current bitmap is the light-on-dark variant.
    white: bool,
    /// Whether the current bitmap is the `neo + spark` style.
    spark: bool,
    rotated_at: Instant,
}

impl Default for Tray {
    fn default() -> Self {
        Self::new()
    }
}

impl Tray {
    pub fn new() -> Self {
        Self {
            icon: None,
            labels: None,
            show_id: tray_icon::menu::MenuId::new("neo-tray-show"),
            quit_id: tray_icon::menu::MenuId::new("neo-tray-quit"),
            white: true,
            spark: false,
            rotated_at: Instant::now(),
        }
    }

    /// Whether a real tray icon was installed. Off-screen tests never call
    /// [`Self::start`], so this also distinguishes the real client.
    pub fn is_active(&self) -> bool {
        self.icon.is_some()
    }

    /// Install the icon and menu. Only call this from the real client; a
    /// failure must not block startup, it only disables close-to-tray.
    pub fn start(&mut self, dark: bool) {
        use tray_icon::menu::{Menu, MenuItem};
        use tray_icon::TrayIconBuilder;

        if self.icon.is_some() {
            return;
        }
        let menu = Menu::new();
        let show = MenuItem::with_id(self.show_id.clone(), tr("显示主界面"), true, None);
        let quit = MenuItem::with_id(self.quit_id.clone(), tr("退出 Neo"), true, None);
        if menu.append(&show).is_err() || menu.append(&quit).is_err() {
            record(Level::Error, "tray", "托盘菜单创建失败");
            return;
        }
        self.white = dark;
        self.spark = false;
        self.rotated_at = Instant::now();
        let icon = match self.icon_bitmap() {
            Some(icon) => icon,
            None => return,
        };
        match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(tr("Neo — 教室大屏 AI 助手"))
            .with_icon(icon)
            .build()
        {
            Ok(icon) => {
                self.icon = Some(icon);
                self.labels = Some((show, quit));
            }
            Err(_) => record(Level::Error, "tray", "系统托盘创建失败"),
        }
    }

    /// Drop the icon; the process is exiting.
    pub fn close(&mut self) {
        self.icon = None;
        self.labels = None;
    }

    /// Switch the bitmap when the app theme changes.
    pub fn apply_theme(&mut self, dark: bool) {
        if self.white == dark {
            return;
        }
        self.white = dark;
        self.refresh_icon();
    }

    /// Re-apply localized menu labels and tooltip.
    pub fn refresh_language(&self) {
        if let Some((show, quit)) = &self.labels {
            show.set_text(tr("显示主界面"));
            quit.set_text(tr("退出 Neo"));
        }
        if let Some(icon) = &self.icon {
            let _ = icon.set_tooltip(Some(tr("Neo — 教室大屏 AI 助手")));
        }
    }

    /// Rotate the wordmark and collect at most one pending action.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<TrayAction> {
        self.icon.as_ref()?;
        if self.rotated_at.elapsed() >= ROTATE {
            self.spark = !self.spark;
            self.rotated_at = Instant::now();
            self.refresh_icon();
            ctx.request_repaint_after(ROTATE);
        }

        let mut show = false;
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            // Left-button down is one step quicker than a double click.
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Down,
                ..
            } = event
            {
                show = true;
            }
        }
        while let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if event.id == self.quit_id {
                return Some(TrayAction::Quit);
            }
            if event.id == self.show_id {
                show = true;
            }
        }
        show.then_some(TrayAction::ShowWindow)
    }

    fn icon_bitmap(&self) -> Option<tray_icon::Icon> {
        let size = brand::tray_size();
        let (rgba, width, height) = if self.spark {
            brand::tray_spark_rgba(size, self.white)
        } else {
            brand::tray_rgba(size, self.white)
        };
        match tray_icon::Icon::from_rgba(rgba, width, height) {
            Ok(icon) => Some(icon),
            Err(_) => {
                record(Level::Error, "tray", "托盘图标创建失败");
                None
            }
        }
    }

    fn refresh_icon(&self) {
        let Some(icon) = &self.icon else { return };
        if let Some(bitmap) = self.icon_bitmap() {
            let _ = icon.set_icon(Some(bitmap));
        }
    }
}
