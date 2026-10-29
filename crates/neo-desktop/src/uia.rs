//! UI Automation 会话。调用方必须先初始化 COM（MTA）；
//! 这里只负责创建 `CUIAutomation8` 并设置连接/事务超时。

#[cfg(windows)]
pub const DEFAULT_CONNECTION_TIMEOUT_MS: u32 = 2_000;
#[cfg(windows)]
pub const DEFAULT_TRANSACTION_TIMEOUT_MS: u32 = 5_000;

/// 创建一个带超时的 UIA 客户端。
///
/// `CUIAutomation8` 实现 `IUIAutomation2`，可用超时避免某个窗口的
/// provider 卡住时把整次遍历拖死。旧系统或接口不可用时退回 v1，
/// 此时超时由调用方的窗口/总预算兜底。
#[cfg(windows)]
pub fn create_uia() -> windows::core::Result<windows::Win32::UI::Accessibility::IUIAutomation> {
    create_uia_with_timeouts(
        DEFAULT_CONNECTION_TIMEOUT_MS,
        DEFAULT_TRANSACTION_TIMEOUT_MS,
    )
}

#[cfg(windows)]
pub fn create_uia_with_timeouts(
    connection_ms: u32,
    transaction_ms: u32,
) -> windows::core::Result<windows::Win32::UI::Accessibility::IUIAutomation> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Accessibility::{CUIAutomation8, IUIAutomation, IUIAutomation2};

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
    if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
        unsafe {
            // 设置失败不影响使用；保持 v1 行为，由调用方预算兜底。
            let _ = automation2.SetConnectionTimeout(connection_ms.max(1));
            let _ = automation2.SetTransactionTimeout(transaction_ms.max(1));
        }
    }
    Ok(automation)
}
