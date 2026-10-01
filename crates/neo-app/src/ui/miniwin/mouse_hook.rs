//! 只观察、不拦截的低级鼠标 hook。回调不做 UI、分配、日志或阻塞锁操作。
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Click {
    pub sequence: u64,
    pub position: egui::Pos2,
}

/// 单写者 mailbox：奇数表示写入中，偶数版本同时作为事件序号。
/// 只保留最近一次真实按下，模型事件不能覆盖它；同毫秒点击也有不同序号。
#[derive(Default)]
pub(super) struct ClickSlot {
    version: AtomicU64,
    position: AtomicU64,
}

impl ClickSlot {
    fn record(&self, left_down: bool, flags: u32, extra: usize, x: i32, y: i32) {
        if !left_down || !physical_mouse(flags, extra) { return; }
        let version = self.version.load(Ordering::SeqCst);
        self.version.store(version.wrapping_add(1), Ordering::SeqCst);
        self.position.store((x as u32 as u64) << 32 | y as u32 as u64, Ordering::SeqCst);
        self.version.store(version.wrapping_add(2), Ordering::SeqCst);
    }

    pub(super) fn latest(&self) -> Option<Click> {
        // 有界读取；遇到正在写入的回调就下帧重读，不阻塞任一线程。
        for _ in 0..3 {
            let before = self.version.load(Ordering::SeqCst);
            if before == 0 || before & 1 != 0 { continue; }
            let position = self.position.load(Ordering::SeqCst);
            if before == self.version.load(Ordering::SeqCst) {
                return Some(Click {
                    sequence: before / 2,
                    position: egui::pos2((position >> 32) as u32 as i32 as f32, position as u32 as i32 as f32),
                });
            }
        }
        None
    }

    #[cfg(test)]
    pub(super) fn mock_down(&self, flags: u32, extra: usize, x: i32, y: i32) {
        self.record(true, flags, extra, x, y);
    }
}

fn physical_mouse(flags: u32, extra: usize) -> bool {
    // MSLLHOOKSTRUCT: LLMHF_INJECTED = 1, LLMHF_LOWER_IL_INJECTED = 2。
    // Windows 将触屏提升为鼠标时可带 injected 标志；MI_WP_SIGNATURE + 0x80
    // 是触屏兼容签名（低 7 位为接触 ID）。不能豁免低完整性注入。
    // dwExtraInfo 可被程序伪造：这是触屏兼容规则，不是安全认证边界。
    let touch = extra & 0xffff_ff00 == 0xff51_5700 && extra & 0x80 != 0;
    flags & 2 == 0 && (flags & 1 == 0 || touch)
}

pub(super) struct MouseHook {
    slot: Arc<ClickSlot>,
    stop: Arc<AtomicBool>,
    tid: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl MouseHook {
    pub(super) fn latest(&self) -> Option<Click> { self.slot.latest() }

    pub(super) fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    #[cfg(all(windows, not(test)))]
    pub(super) fn start() -> Result<Self, String> {
        let slot = Arc::new(ClickSlot::default());
        let stop = Arc::new(AtomicBool::new(false));
        let tid = Arc::new(AtomicU32::new(0));
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let (worker_slot, worker_stop, worker_tid) = (slot.clone(), stop.clone(), tid.clone());
        let thread = std::thread::Builder::new().name("neo-mouse-hook".into())
            .spawn(move || native::run(worker_slot, worker_stop, worker_tid, tx))
            .map_err(|error| format!("无法启动鼠标监听线程：{error}"))?;
        let hook = Self { slot, stop, tid, thread: Some(thread) };
        match rx.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(Ok(())) => Ok(hook),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(format!("鼠标 hook 初始化未完成：{error}")),
        }
        // 失败/超时也经 Drop 发出 stop；迟到的安装由工作线程 guard 卸载。
    }
}

impl Drop for MouseHook {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let tid = self.tid.load(Ordering::Acquire);
        #[cfg(all(windows, not(test)))]
        if tid != 0 {
            // 队列在 tid 发布前创建；投递失败时消息泵最多 50ms 后检查 stop。
            unsafe { windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                tid, windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT, 0, 0); }
        }
        #[cfg(any(not(windows), test))]
        let _ = tid;
        if let Some(thread) = self.thread.take() {
            // UI 不等待尚未结束的线程。Arc 数据和 hook guard 由工作线程持有，
            // 即使 OS 调用卡住也不会悬垂；恢复后检查 stop 并自行卸载。
            if thread.is_finished() { let _ = thread.join(); }
        }
    }
}

#[cfg(all(windows, not(test)))]
mod native {
    use super::*;
    use std::cell::RefCell;
    use windows_sys::Win32::{Foundation::*, System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId}, UI::WindowsAndMessaging::*};

    thread_local! {
        static SLOT: RefCell<Option<Arc<ClickSlot>>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn callback(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && wp == WM_LBUTTONDOWN as usize && lp != 0 {
            let event = unsafe { &*(lp as *const MSLLHOOKSTRUCT) };
            let _ = SLOT.try_with(|slot| {
                if let Ok(slot) = slot.try_borrow() {
                    if let Some(slot) = slot.as_ref() {
                        slot.record(true, event.flags, event.dwExtraInfo, event.pt.x, event.pt.y);
                    }
                }
            });
        }
        // 包括负 code、注入、移动、松开在内，始终沿 hook 链传递。
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wp, lp) }
    }

    struct Installed(HHOOK);
    impl Drop for Installed {
        fn drop(&mut self) {
            if unsafe { UnhookWindowsHookEx(self.0) } == 0 {
                eprintln!("鼠标 hook 卸载失败：{}", unsafe { GetLastError() });
            }
            SLOT.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(super) fn run(slot: Arc<ClickSlot>, stop: Arc<AtomicBool>, tid: Arc<AtomicU32>, ready: std::sync::mpsc::SyncSender<Result<(), String>>) {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE); }
        tid.store(unsafe { GetCurrentThreadId() }, Ordering::Release);
        if stop.load(Ordering::Acquire) { return; }
        SLOT.with(|local| *local.borrow_mut() = Some(slot));
        let handle = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(callback), GetModuleHandleW(std::ptr::null()), 0) };
        if handle.is_null() {
            let error = unsafe { GetLastError() };
            SLOT.with(|local| *local.borrow_mut() = None);
            let _ = ready.send(Err(format!("无法安装 WH_MOUSE_LL：{error}")));
            return;
        }
        let _installed = Installed(handle);
        if stop.load(Ordering::Acquire) || ready.send(Ok(())).is_err() { return; }
        while !stop.load(Ordering::Acquire) {
            // 有界批次防止消息洪水饿死 stop 检查；PeekMessage 会派发 LL 回调。
            for _ in 0..64 {
                if stop.load(Ordering::Acquire) { return; }
                if unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 { break; }
                if msg.message == WM_QUIT { return; }
                unsafe { TranslateMessage(&msg); DispatchMessageW(&msg); }
            }
            if unsafe { MsgWaitForMultipleObjectsEx(0, std::ptr::null(), 50, QS_ALLINPUT, MWMO_INPUTAVAILABLE) } == WAIT_FAILED {
                eprintln!("鼠标 hook 消息泵失败：{}", unsafe { GetLastError() });
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "mouse_hook_tests.rs"]
mod tests;
