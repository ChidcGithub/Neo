//! 桌面自动化底层的错误类型。上层负责把它映射成 `ToolError` 或 MCP 错误。

use std::fmt;

#[derive(Clone, Debug)]
pub enum DesktopError {
    /// 参数不合法（空快捷键、越界坐标等），输入未被注入。
    BadArguments(String),
    /// 当前平台或会话不支持该能力。
    Unsupported(String),
    /// 目标不存在（窗口/元素/进程）。
    NotFound(String),
    /// 权限、UIPI 或策略拒绝。
    NotAllowed(String),
    /// 超时；可能已发生部分副作用。
    Timeout(String),
    /// 目标在快照与动作之间变化。
    Conflict(String),
    /// 系统调用或 IO 失败。
    Io(String),
    /// 内部不变量被破坏。
    Internal(String),
}

impl DesktopError {
    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::BadArguments(message.into())
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::NotAllowed(message.into())
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::Timeout(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict(message.into())
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// 不应把平台原始错误直接展示给模型；上层用它决定错误分类。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::BadArguments(_) => "bad_arguments",
            Self::Unsupported(_) => "unsupported",
            Self::NotFound(_) => "not_found",
            Self::NotAllowed(_) => "not_allowed",
            Self::Timeout(_) => "timeout",
            Self::Conflict(_) => "conflict",
            Self::Io(_) => "io",
            Self::Internal(_) => "internal",
        }
    }
}

impl fmt::Display for DesktopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArguments(message)
            | Self::Unsupported(message)
            | Self::NotFound(message)
            | Self::NotAllowed(message)
            | Self::Timeout(message)
            | Self::Conflict(message)
            | Self::Io(message)
            | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DesktopError {}

pub type Result<T> = std::result::Result<T, DesktopError>;
