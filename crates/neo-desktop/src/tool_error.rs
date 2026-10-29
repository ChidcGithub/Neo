//! 工具错误分类与结构化错误。由 `neo-tools` re-export，供桌面层与工具层共用。

use serde_json::{json, Value};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::diagnostic::{self, ErrorTrace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// 参数缺失、类型不对、越界。
    BadArguments,
    /// 目标不存在。
    NotFound,
    /// 工作区外 / 只读模式 / 策略拒绝。
    NotAllowed,
    /// 内容或输出超过体积上限。
    TooLarge,
    /// `old_string` 命中多处，无法确定改哪里。
    NotUnique,
    /// 已存在且未允许覆盖。
    Conflict,
    /// 编码不是 UTF-8 等不支持的情况。
    Unsupported,
    /// 超时（`bash` 的命令超时、`web_search` 的 HTTP 超时）。
    Timeout,
    /// 其它 I/O 错误。
    Io,
    /// 工具自身实现出了问题（不该出现）。
    Internal,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::BadArguments => "bad_arguments",
            ErrorKind::NotFound => "not_found",
            ErrorKind::NotAllowed => "not_allowed",
            ErrorKind::TooLarge => "too_large",
            ErrorKind::NotUnique => "not_unique",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Io => "io",
            ErrorKind::Internal => "internal",
        }
    }
}

/// 一次失败的结构化说明。
///
/// `Display` exposes only the public message. `Debug` redacts diagnostics, but
/// public message/hint fields are not secret-safe. Raw sources are not retained:
/// `Error::source()` is `None`; opt-in source strings live only in `diagnostic`.
#[derive(Clone)]
pub struct ToolError {
    pub kind: ErrorKind,
    /// 发生了什么（面向模型与用户，一句话）。
    pub message: String,
    /// 下一步该怎么办。**必须可执行**，不写"请重试"这类废话。
    pub hint: Option<String>,
    /// Local-only trace. Deliberately excluded from model JSON and `Debug`.
    pub diagnostic: Option<Arc<ErrorTrace>>,
}

impl ToolError {
    #[track_caller]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let mut error = Self {
            kind,
            message: message.into(),
            hint: None,
            diagnostic: None,
        };
        // String-only providers (including UIA) still have an actual error message;
        // keep it in opt-in details without guessing an upstream source chain.
        error.diagnostic = ErrorTrace::capture(Some(&error));
        error
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Replace diagnostic causes without recapturing the creation stack/site.
    /// No-op when disabled or when this error was created without a trace.
    pub fn with_source(mut self, error: &(dyn Error + 'static)) -> Self {
        if diagnostic::is_enabled() {
            if let Some(trace) = &mut self.diagnostic {
                Arc::make_mut(trace).replace_causes(error);
            }
        }
        self
    }

    #[track_caller]
    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadArguments, message)
    }

    #[track_caller]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    #[track_caller]
    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotAllowed, message)
    }

    #[track_caller]
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "kind": self.kind.as_str(), "message": self.message });
        if let Some(h) = &self.hint {
            v["hint"] = json!(h);
        }
        v
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl fmt::Debug for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("hint", &self.hint)
            .field("diagnostic", &self.diagnostic.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl Error for ToolError {}
