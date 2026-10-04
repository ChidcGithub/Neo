//! Text-only checks: never send a system notification.
use super::*;
use crate::i18n::{tr, with_language, Language};

#[test]
fn notification_defaults_follow_language_and_preserve_reply_text() {
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            assert_eq!(task_done_text(""), (tr("Neo · 任务完成"), tr("本轮任务已完成。")));
            let reply = "原始回复 <&> {error}";
            assert_eq!(task_done_text(reply), (tr("Neo · 任务完成"), reply));
        });
    }
}
