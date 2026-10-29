//! 工作区围栏由 `neo-desktop` 提供；这里保留 `neo-tools` 的公开路径与测试。

pub use neo_desktop::{normalize, Scope};

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
