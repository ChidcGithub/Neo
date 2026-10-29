//! 诊断模块由 `neo-desktop` 提供；这里保留 `neo-tools` 的测试与公开路径。

pub use neo_desktop::diagnostic::*;

#[cfg(test)]
#[path = "diagnostic_tests.rs"]
mod tests;
