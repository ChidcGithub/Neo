//! 仅测试内存通道、取消及事件生命周期；不调用 start、不创建 AudioTap。

use super::*;

#[test]
fn generated_errors_use_current_language_but_raw_errors_are_untouched() {
    use crate::i18n::{with_language, Language};
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let (mut audio, mut worker) = pair();
            for _ in 0..FRAME_CAPACITY {
                assert!(worker.frame(vec![0.0]));
            }
            assert!(!worker.frame(vec![0.0]));
            assert_eq!(audio.try_recv(), Some(Event::Error(tr(BUFFER_FULL).into())));
            let (mut audio, worker) = pair();
            drop(worker);
            assert_eq!(audio.try_recv(), Some(Event::Error(tr(WORKER_ENDED).into())));
            let (mut audio, mut worker) = pair();
            let raw = "设备原始错误 {error}: E_DEVICE_42";
            worker.fail(raw.into());
            assert_eq!(audio.try_recv(), Some(Event::Error(raw.into())));
        });
    }
}

fn pair() -> (ManualAudio, WorkerEvents) {
    ManualAudio::channel(egui::Context::default())
}

#[test]
fn empty_live_source_is_not_a_stream_error() {
    let (mut audio, _worker) = pair();
    assert_eq!(audio.try_recv(), None);
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn frames_preserve_samples_and_order() {
    let (mut audio, mut worker) = pair();
    assert!(worker.frame(vec![0.25, -0.5]));
    assert!(worker.frame(vec![0.75]));
    assert_eq!(audio.try_recv(), Some(Event::Frame(vec![0.25, -0.5])));
    assert_eq!(audio.try_recv(), Some(Event::Frame(vec![0.75])));
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn full_frame_queue_reports_terminal_error_without_draining() {
    let (mut audio, mut worker) = pair();
    for _ in 0..FRAME_CAPACITY {
        assert!(worker.frame(vec![0.0]));
    }
    assert!(!worker.frame(vec![1.0]));
    assert_eq!(audio.try_recv(), Some(Event::Error(BUFFER_FULL.into())));
    assert_eq!(audio.try_recv(), None);
    assert!(!worker.frame(vec![2.0]));
    drop(worker);
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn explicit_error_is_reliable_even_when_frames_are_full() {
    let (mut audio, mut worker) = pair();
    for _ in 0..FRAME_CAPACITY {
        assert!(worker.frame(vec![0.0]));
    }
    worker.fail(STREAM_ENDED.into());
    worker.fail("later error must not replace first".into());
    drop(worker);
    assert_eq!(audio.try_recv(), Some(Event::Error(STREAM_ENDED.into())));
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn startup_error_is_delivered_once() {
    let (mut audio, mut worker) = pair();
    worker.fail("打开手动听写麦克风失败：没有设备".into());
    drop(worker);
    assert_eq!(
        audio.try_recv(),
        Some(Event::Error("打开手动听写麦克风失败：没有设备".into()))
    );
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn unexpected_worker_exit_is_not_an_empty_live_source() {
    let (mut audio, worker) = pair();
    drop(worker);
    assert_eq!(audio.try_recv(), Some(Event::Error(WORKER_ENDED.into())));
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn unexpected_worker_exit_is_reported_with_full_frame_queue() {
    let (mut audio, mut worker) = pair();
    for _ in 0..FRAME_CAPACITY {
        assert!(worker.frame(vec![0.0]));
    }
    drop(worker);
    assert_eq!(audio.try_recv(), Some(Event::Error(WORKER_ENDED.into())));
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn cancellation_suppresses_queued_and_late_events() {
    let (mut audio, mut worker) = pair();
    assert!(worker.frame(vec![0.0]));
    audio.cancelled.store(true, Ordering::Release);
    assert_eq!(audio.try_recv(), None);
    assert!(!worker.frame(vec![1.0]));
    worker.fail("late startup failure".into());
    assert!(matches!(
        audio.terminal.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn cancellation_suppresses_an_already_queued_error() {
    let (mut audio, mut worker) = pair();
    worker.fail(STREAM_ENDED.into());
    audio.cancelled.store(true, Ordering::Release);
    assert_eq!(audio.try_recv(), None);
}

#[test]
fn drop_cancels_while_worker_is_still_owned() {
    let (audio, mut worker) = pair();
    drop(audio);
    assert!(worker.is_cancelled());
    assert!(!worker.frame(vec![0.0]));
    worker.fail("late error".into());
}

#[test]
fn old_worker_cannot_publish_to_a_new_session() {
    let (old_audio, mut old_worker) = pair();
    drop(old_audio);
    let (mut new_audio, mut new_worker) = pair();
    assert!(!old_worker.frame(vec![1.0]));
    old_worker.fail("old failure".into());
    drop(old_worker);
    assert_eq!(new_audio.try_recv(), None);
    assert!(new_worker.frame(vec![2.0]));
    assert_eq!(new_audio.try_recv(), Some(Event::Frame(vec![2.0])));
}
