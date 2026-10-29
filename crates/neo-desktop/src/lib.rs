//! Neo 的 Windows 桌面自动化底层。
//!
//! 只提供能力，不决定权限：上层 `neo-tools` 负责 `Policy`、确认框与
//! `Outcome` 结构。坐标统一使用虚拟桌面物理像素。

pub mod app;
pub mod clipboard;
pub mod diagnostic;
pub mod display;
pub mod error;
pub mod input;
pub mod screen;
pub mod screen_uia;
mod scope;
mod tool_error;
#[cfg(windows)]
pub mod uia;
pub mod window;

pub use error::{DesktopError, Result};
pub use scope::{normalize, Scope};
pub use tool_error::{ErrorKind, ToolError};

#[track_caller]
pub fn cancelled_error() -> ToolError {
    ToolError::not_allowed("工具调用已取消；已发生的操作不会回滚")
}
