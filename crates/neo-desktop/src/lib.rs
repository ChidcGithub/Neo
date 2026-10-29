//! Neo 的 Windows 桌面自动化底层。
//!
//! 只提供能力，不决定权限：上层 `neo-tools` 负责 `Policy`、确认框与
//! `Outcome` 结构。坐标统一使用虚拟桌面物理像素。

pub mod clipboard;
pub mod display;
pub mod error;
pub mod input;
#[cfg(windows)]
pub mod uia;

pub use error::{DesktopError, Result};
