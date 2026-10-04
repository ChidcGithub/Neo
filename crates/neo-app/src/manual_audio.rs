//! 悬浮按钮按需使用的独立麦克风源；不读取唤醒设置、不创建 WakeEngine。
//! 仅 `start` 开启采集；调用方在 Idle 不持有实例，结束/取消时直接丢弃。

use crate::i18n::tr;
#[cfg(not(test))]
use crate::i18n::tf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
#[cfg(not(test))]
use std::time::Duration;

const FRAME_CAPACITY: usize = 8;
#[cfg(not(test))]
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const BUFFER_FULL: &str = "手动听写音频缓冲已满，音频不完整，请重新开始听写";
#[cfg(not(test))]
const AUDIO_MISSING: &str = "麦克风采集缓冲过载，音频不完整，请重新开始听写";
const STREAM_ENDED: &str = "麦克风连接中断，请重新开始听写";
const WORKER_ENDED: &str = "手动听写采集线程意外退出，请重新开始听写";

#[derive(Debug, PartialEq)]
pub enum Event {
    /// 16 kHz、单声道、80 ms（1280 样本）的 f32 音频。
    Frame(Vec<f32>),
    /// 终止错误，仅上报一次；收到后应丢弃本次采集实例。
    Error(String),
}

pub struct ManualAudio {
    frames: mpsc::Receiver<Vec<f32>>,
    terminal: mpsc::Receiver<String>,
    cancelled: Arc<AtomicBool>,
    finished: bool,
}

impl ManualAudio {
    /// 只创建后台线程，不等待麦克风建流。
    /// 返回的 Err 仅表示线程创建失败；设备启动失败通过 Event::Error 上报。
    #[cfg(not(test))]
    pub fn start(ctx: egui::Context) -> Result<Self, String> {
        let (audio, events) = Self::channel(ctx);
        std::thread::Builder::new()
            .name("neo-manual-audio".into())
            .spawn(move || capture(events))
            .map_err(|e| tf("无法启动手动听写采集线程：{error}", &[("error", e.to_string())]))?;
        Ok(audio)
    }

    fn channel(ctx: egui::Context) -> (Self, WorkerEvents) {
        let (frame_tx, frames) = mpsc::sync_channel(FRAME_CAPACITY);
        // 终止错误不与帧争抢容量；唯一发送者最多发送一次，永不阻塞 worker。
        let (terminal_tx, terminal) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let events = WorkerEvents {
            frames: frame_tx,
            terminal: Some(terminal_tx),
            cancelled: cancelled.clone(),
            ctx,
        };
        (
            Self {
                frames,
                terminal,
                cancelled,
                finished: false,
            },
            events,
        )
    }

    /// 非阻塞接收。错误优先于积压帧；错误之后永久返回 None。
    /// 活跃期间 None 表示暂无事件，并不表示断流；断流总会上报 Error。
    pub fn try_recv(&mut self) -> Option<Event> {
        if self.finished || self.cancelled.load(Ordering::Acquire) {
            return None;
        }
        let event = match self.terminal.try_recv() {
            Ok(error) => Event::Error(error),
            Err(_) => match self.frames.try_recv() {
                Ok(frame) => Event::Frame(frame),
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // worker 可能在第一次检查之后退出；优先保留其具体错误。
                    Event::Error(
                        self.terminal
                            .try_recv()
                            .unwrap_or_else(|_| tr(WORKER_ENDED).into()),
                    )
                }
            },
        };
        if self.cancelled.load(Ordering::Acquire) {
            return None;
        }
        if matches!(&event, Event::Error(_)) {
            self.finished = true;
        }
        Some(event)
    }
}

impl Drop for ManualAudio {
    fn drop(&mut self) {
        // 不持有 AudioTap / JoinHandle，不等待设备握手、驱动析构或尾帧。
        self.cancelled.store(true, Ordering::Release);
    }
}

struct WorkerEvents {
    frames: mpsc::SyncSender<Vec<f32>>,
    terminal: Option<mpsc::SyncSender<String>>,
    cancelled: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl WorkerEvents {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn repaint(&self) {
        if !self.is_cancelled() {
            self.ctx.request_repaint();
        }
    }

    fn frame(&mut self, frame: Vec<f32>) -> bool {
        if self.is_cancelled() || self.terminal.is_none() {
            return false;
        }
        match self.frames.try_send(frame) {
            Ok(()) => {
                self.repaint();
                !self.is_cancelled()
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.fail(tr(BUFFER_FULL).into());
                false
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        }
    }

    fn fail(&mut self, error: String) {
        let Some(terminal) = self.terminal.take() else {
            return;
        };
        if !self.is_cancelled() && terminal.try_send(error).is_ok() {
            self.repaint();
        }
    }
}

impl Drop for WorkerEvents {
    fn drop(&mut self) {
        // 覆盖意外返回和 panic 展开；先发布错误/唤醒 UI，再释放帧发送端。
        self.fail(tr(WORKER_ENDED).into());
    }
}

#[cfg(not(test))]
fn capture(mut events: WorkerEvents) {
    if events.is_cancelled() {
        return;
    }
    let tap = match neo_wake::AudioTap::start_with_stop(
        Arc::new(AtomicBool::new(false)),
        events.cancelled.clone(),
    ) {
        Ok(tap) => tap,
        Err(error) => {
            events.fail(tf("打开手动听写麦克风失败：{error}", &[("error", error.to_string())]));
            return;
        }
    };
    while !events.is_cancelled() {
        let frame = tap.next_frame(POLL_INTERVAL);
        if events.is_cancelled() {
            break;
        }
        if tap.has_missing_audio() {
            events.fail(tr(AUDIO_MISSING).into());
            break;
        }
        if let Some(frame) = frame {
            if !events.frame(frame) {
                break;
            }
        } else if !tap.is_alive() {
            events.fail(tr(STREAM_ENDED).into());
            break;
        }
    }
    // AudioTap 的创建、消费和 Drop 都在 worker；底层采集线程自行释放流。
    // 取消不排尾音，也不调用会等待的 stop_capture。
}

#[cfg(test)]
#[path = "manual_audio_tests.rs"]
mod tests;
