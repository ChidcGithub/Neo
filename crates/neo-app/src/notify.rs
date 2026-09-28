//! Windows 原生消息通知（WinRT toast）：任务完成时弹系统通知。
//!
//! 未打包（unpackaged）进程要能发 toast，关键是给进程设一个
//! AppUserModelID（`SetCurrentProcessExplicitAppUserModelID`），并用同一个
//! AUMID 创建 notifier。失败（通知被系统策略关掉、极端精简的系统镜像等）
//! 就地降级为一条日志，绝不挡主流程。

/// 进程级 AUMID（通知中心的归属名靠它）。
#[cfg(windows)]
const AUMID: &str = "Neo.Classroom";

/// 发一条系统 toast。返回 Err 时调用方记日志即可。
#[cfg(windows)]
fn toast(title: &str, body: &str) -> Result<(), String> {
    use windows::core::{w, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    // 幂等：每次设同一个值，开销可忽略。
    unsafe {
        SetCurrentProcessExplicitAppUserModelID(w!("Neo.Classroom"))
            .map_err(|e| format!("设置 AUMID 失败：{e}"))?;
    }
    // toast XML 是字面拼接的，标题/正文都来自动态内容 —— 必须转义。
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\">\
         <text>{}</text><text>{}</text></binding></visual></toast>",
        esc(title),
        esc(body)
    );
    let doc = XmlDocument::new().map_err(|e| format!("XmlDocument：{e}"))?;
    doc.LoadXml(&HSTRING::from(xml))
        .map_err(|e| format!("toast XML 解析失败：{e}"))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
        .map_err(|e| format!("创建通知器失败：{e}"))?;
    let toast =
        ToastNotification::CreateToastNotification(&doc).map_err(|e| format!("创建通知：{e}"))?;
    notifier.Show(&toast).map_err(|e| format!("弹出通知：{e}"))?;
    Ok(())
}

#[cfg(not(windows))]
fn toast(_title: &str, _body: &str) -> Result<(), String> {
    Err("非 Windows：无系统通知".into())
}

/// 一轮任务执行完成。`line` 取最后一条回复的首行（已裁好长度）。
pub fn task_done(line: &str) {
    let body = if line.is_empty() {
        "本轮任务已完成。".to_owned()
    } else {
        line.to_owned()
    };
    if let Err(e) = toast("Neo · 任务完成", &body) {
        eprintln!("[neo] 完成通知失败（忽略）: {e}");
    }
}
