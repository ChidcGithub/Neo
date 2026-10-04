use super::*;
use crate::i18n::with_language;
use crate::state::{ChatMessage, ToolMeta, ToolState};

#[test]
fn localized_stop_and_cancel_keep_prompts_and_tool_feedback_identical() {
    let mut baseline = None;
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let mut state = AppState::default();
            state
                .messages
                .push(ChatMessage::new(Role::User, "原始用户内容 {tool}"));
            let mut assistant = ChatMessage::new(Role::Assistant, "原始回答");
            assistant.tool_calls.push(neo_llm::ToolCall {
                id: "call".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            });
            state.messages.push(assistant);
            let mut tool = ToolMeta::restored("read_file");
            tool.call_id = "call".into();
            tool.state = ToolState::AwaitingConfirm;
            state
                .messages
                .push(ChatMessage::tool_result(tool, String::new()));
            let mut partial = ChatMessage::new(Role::Assistant, "未完成回答");
            partial.streaming = true;
            state.messages.push(partial);
            state.cancel();
            assert_eq!(
                state.messages[2].meta,
                tf("{tool} · 已取消", &[("tool", "read_file".into())])
            );
            assert_eq!(state.messages[3].meta, tr("已停止"));
            assert_eq!(state.messages[3].content, "未完成回答");
            assert!(state.messages[2]
                .content
                .contains("用户取消了这次调用；已请求停止，已发生的副作用不会自动回滚"));
            let unchanged = (
                state.system_prompt(),
                state.messages[2].content.clone(),
                format!("{:?}", state.api_messages(usize::MAX)),
            );
            if let Some(expected) = &baseline {
                assert_eq!(&unchanged, expected);
            } else {
                baseline = Some(unchanged);
            }
        });
    }
}

#[test]
fn localized_question_and_denial_metadata_do_not_change_model_results() {
    let mut baseline = None;
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let mut state = AppState::default();
            for name in ["ask_user", "ask_user", "write_file"] {
                let mut tool = ToolMeta::restored(name);
                tool.state = ToolState::AwaitingConfirm;
                state
                    .messages
                    .push(ChatMessage::tool_result(tool, String::new()));
            }
            let answer = "用户答案 {tool}".to_owned();
            state.answer_question(0, Some(answer.clone()));
            state.answer_question(1, None);
            state.deny_tool(2);
            assert_eq!(
                state.messages[0].meta,
                tf(
                    "{tool} · 已回答：{answer}",
                    &[("tool", "ask_user".into()), ("answer", answer)]
                )
            );
            assert_eq!(
                state.messages[1].meta,
                tf("{tool} · 已跳过", &[("tool", "ask_user".into())])
            );
            assert_eq!(
                state.messages[2].meta,
                tf("{tool} · 已拒绝", &[("tool", "write_file".into())])
            );
            let results: Vec<_> = state.messages.iter().map(|m| m.content.clone()).collect();
            if let Some(expected) = &baseline {
                assert_eq!(&results, expected);
            } else {
                baseline = Some(results);
            }
        });
    }
}

#[test]
fn localized_compaction_errors_and_cancellation_preserve_history() {
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let force = [neo_llm::Msg::rejected("test budget")];
            let mut state = AppState::default();
            let mut orphan = ToolMeta::restored("read_file");
            orphan.call_id = "orphan".into();
            state.messages.push(ChatMessage::tool_result(
                orphan,
                "original tool result".into(),
            ));
            state.messages.push(ChatMessage::new(Role::User, "latest"));
            assert_eq!(
                state.compaction_plan(&force).err().as_deref(),
                Some(tr("历史工具结果缺少配对调用，未执行摘要"))
            );
            let mut invalid = ChatMessage::new(Role::Assistant, "original answer");
            invalid.tool_calls.push(neo_llm::ToolCall {
                id: String::new(),
                name: "read_file".into(),
                arguments: "{}".into(),
            });
            state.messages[0] = invalid;
            assert_eq!(
                state.compaction_plan(&force).err().as_deref(),
                Some(tr("历史工具调用重复或为空，未执行摘要"))
            );
            state.messages[0] = ChatMessage::new(Role::User, "x".repeat(40_000));
            state.context_tokens = 32 * 1024;
            assert_eq!(
                state.compaction_plan(&force).err().as_deref(),
                Some(tr(
                    "旧历史无法在当前接口预算内完整摘要；请调高预算或新建会话，历史未删除"
                ))
            );
            let before: Vec<_> = state.messages.iter().map(|m| m.content.clone()).collect();
            let (_tx, rx) = std::sync::mpsc::channel();
            state.start_compaction(
                crate::state::CompactionPlan {
                    checkpoint: crate::state::ContextCheckpoint {
                        covered: 0,
                        keep_user: None,
                        fingerprint: 0,
                        summary: String::new(),
                    },
                    messages: Vec::new(),
                },
                neo_llm::Stream::new_for_test(rx),
            );
            state.cancel();
            assert_eq!(
                state.compaction_status.as_deref(),
                Some(tr("历史压缩已取消，原聊天记录保留"))
            );
            assert_eq!(
                state
                    .messages
                    .iter()
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>(),
                before
            );
        });
    }
}

#[test]
fn localized_disconnected_stream_error_is_not_added_to_model_history() {
    let mut baseline = None;
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let mut state = AppState::default();
            state.draft = "unchanged question".into();
            assert!(state.submit());
            let (tx, rx) = std::sync::mpsc::channel();
            drop(tx);
            state.start_generation(StreamSource::Real(Box::new(neo_llm::Stream::new_for_test(
                rx,
            ))));
            assert!(!state.pump());
            assert_eq!(
                state.messages.last().unwrap().error.as_deref(),
                Some(tr("连接异常中断（流线程退出），请重试"))
            );
            let wire = format!("{:?}", state.api_messages(usize::MAX));
            if let Some(expected) = &baseline {
                assert_eq!(&wire, expected);
            } else {
                baseline = Some(wire);
            }
        });
    }
}

#[test]
fn language_setting_roundtrip_and_invalid_default() {
    with_language(Language::ZhCn, || {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        assert_eq!(app.state.language, Language::ZhCn);
        for language in [Language::EnUs, Language::ZhCn] {
            app.state.language = language;
            app.sync_language(&ctx);
            assert_eq!(i18n::language(), language);
            assert!(app.persist_preferences());
            assert_eq!(app.saved_language, language);
            assert_eq!(
                app.store
                    .as_ref()
                    .unwrap()
                    .setting("language")
                    .unwrap()
                    .as_deref(),
                Some(language.code())
            );
            app.state.language = if language == Language::ZhCn {
                Language::EnUs
            } else {
                Language::ZhCn
            };
            app.load_settings();
            assert_eq!(app.state.language, language);
        }
        app.store
            .as_ref()
            .unwrap()
            .set_setting("language", "bad")
            .unwrap();
        app.load_settings();
        assert_eq!(app.state.language, Language::ZhCn);
        assert!(app.tray.is_none() && app.tray_labels.is_none());
    });
}

#[test]
fn language_switch_updates_title_without_tray_and_failed_save_retries() {
    with_language(Language::ZhCn, || {
        let ctx = egui::Context::default();
        let mut app = NeoApp::install(&ctx);
        let store = app.store.take();
        ctx.begin_pass(egui::RawInput::default());
        app.state.language = Language::EnUs;
        app.sync_language(&ctx);
        let mut output = ctx.end_pass();
        output.textures_delta.clear();
        assert_eq!(i18n::language(), Language::EnUs);
        assert!(output.viewport_output.values().flat_map(|v| &v.commands).any(|command| {
            matches!(command, egui::ViewportCommand::Title(title) if title == "Neo — Classroom AI Assistant")
        }));
        assert!(!app.persist_preferences());
        assert_eq!(app.saved_language, Language::ZhCn);
        assert!(app.state.preferences_unsaved);
        assert_eq!(app.applied_language, Some(Language::EnUs));
        app.store = store;
        assert!(app.persist_preferences());
        assert_eq!(app.saved_language, Language::EnUs);
        assert!(!app.state.preferences_unsaved);
        assert!(app.tray.is_none() && app.tray_labels.is_none());
    });
}

#[test]
fn startup_language_read_is_readonly_and_does_not_create_missing_database() {
    with_language(Language::ZhCn, || {
        let path = test_db_path();
        let missing = path.with_file_name("missing.db");
        assert!(
            crate::startup::early_settings(missing.clone(), std::time::Duration::from_secs(2))
                .is_err()
        );
        assert!(!missing.exists());
        let store = Store::open(&path).unwrap();
        store.set_setting("language", "en-US").unwrap();
        for (code, expected) in [
            ("en-US", Language::EnUs),
            ("zh-CN", Language::ZhCn),
            ("invalid", Language::ZhCn),
        ] {
            store.set_setting("language", code).unwrap();
            let (_, language) =
                crate::startup::early_settings(path.clone(), std::time::Duration::from_secs(2))
                    .unwrap();
            assert_eq!(language, expected);
            // Worker only returns values: it cannot mutate this thread's selection.
            assert_eq!(i18n::language(), Language::ZhCn);
            assert_eq!(store.setting("language").unwrap().as_deref(), Some(code));
        }
    });
}

#[test]
fn startup_missing_settings_table_uses_safe_defaults_without_writing() {
    with_language(Language::EnUs, || {
        let path = test_db_path().with_file_name("empty-settings.db");
        // SQLite accepts an existing empty file as an empty database. Read-only
        // startup must not initialize its header or create the settings table.
        std::fs::write(&path, []).unwrap();
        let result =
            crate::startup::early_settings(path.clone(), std::time::Duration::from_secs(2))
                .unwrap();
        assert_eq!(result, (1, Language::ZhCn));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        assert_eq!(i18n::language(), Language::EnUs);
        std::fs::remove_file(path).unwrap();
    });
}

#[test]
fn state_default_follows_language_without_translating_user_titles_or_prompts() {
    for language in [Language::ZhCn, Language::EnUs] {
        with_language(language, || {
            let mut state = AppState::default();
            assert_eq!(state.language, language);
            assert_eq!(state.model_display(), tr("未选择模型"));
            assert_eq!(
                state.current_title(),
                if language == Language::ZhCn {
                    "新对话"
                } else {
                    "New conversation"
                }
            );
            state.messages.push(crate::state::ChatMessage::new(
                Role::User,
                "用户内容 stays unchanged",
            ));
            assert_eq!(state.current_title(), "用户内容 stays unchanged");
            assert!(state.system_prompt().contains("默认简体中文"));
        });
    }
}
