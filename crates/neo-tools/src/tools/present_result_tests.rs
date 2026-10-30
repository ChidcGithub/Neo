use super::*;
use serde_json::json;

fn args(value: serde_json::Value) -> Args<'static> {
    let tool = crate::find("present_result").unwrap();
    let value: &'static serde_json::Value = Box::leak(Box::new(value));
    Args::new(tool, value)
}

#[test]
fn parses_title_and_markdown_content() {
    let parsed = card(&args(json!({
        "title": "勾股定理证明",
        "content": "## 结论\n\n$a^2+b^2=c^2$"
    })))
    .unwrap();
    assert_eq!(parsed.title, "勾股定理证明");
    assert!(parsed.content.contains("$a^2+b^2=c^2$"));

    let multiline = card(&args(json!({
        "title": "第一行\n第二行",
        "content": "正文"
    })))
    .unwrap();
    assert_eq!(multiline.title, "第一行");
}

#[test]
fn preview_reads_like_a_sentence() {
    assert_eq!(
        preview(&args(json!({"title": "本周学习计划", "content": "x"}))),
        "展示：本周学习计划"
    );
}

#[test]
fn missing_or_blank_fields_are_rejected() {
    assert_eq!(
        card(&args(json!({"title": " ", "content": "x"})))
            .unwrap_err()
            .kind,
        ErrorKind::BadArguments
    );
    assert_eq!(
        card(&args(json!({"title": "x", "content": " "})))
            .unwrap_err()
            .kind,
        ErrorKind::BadArguments
    );
}

#[test]
fn oversized_content_is_rejected_with_hint() {
    let error = card(&args(json!({
        "title": "长结果",
        "content": "x".repeat(MAX_CONTENT_CHARS + 1)
    })))
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::BadArguments);
    assert!(error.hint.unwrap().contains("精简"));
}

#[test]
fn unknown_arguments_are_rejected() {
    assert_eq!(
        card(&args(json!({
            "title": "x",
            "content": "y",
            "font_size": 20
        })))
        .unwrap_err()
        .kind,
        ErrorKind::BadArguments
    );
}

#[test]
fn presented_outcome_does_not_echo_the_body() {
    let card = Card {
        title: "结果".into(),
        content: "很长的正文".repeat(100),
    };
    let outcome = presented(&card);
    assert!(outcome.is_ok());
    assert_eq!(outcome.data["title"], "结果");
    assert_eq!(outcome.data["chars"], card.content.chars().count());
    assert!(!outcome.to_model_json(usize::MAX).contains(&card.content));
}

#[test]
fn direct_run_reports_missing_frontend() {
    let scope = Scope::new(std::env::temp_dir());
    let outcome = run(&scope, &args(json!({"title": "结果", "content": "正文"})));
    let error = outcome.error.expect("未接管时应失败");
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.hint.unwrap().contains("写在回答里"));
}
