use super::*;

#[test]
fn explicit_board_commands_accept_stt_punctuation_and_language() {
    for text in [
        "打开画板",
        "打开白板。",
        " Open   Whiteboard! ",
        "open the whiteboard",
        "open drawing",
    ] {
        assert_eq!(board_command(text), Some(BoardKind::Drawing), "{text}");
    }
    for text in ["打开黑板！", "OPEN BLACKBOARD.", "open the blackboard"] {
        assert_eq!(board_command(text), Some(BoardKind::Blackboard), "{text}");
    }
}

#[test]
fn conversation_quotes_negations_and_combined_commands_are_not_intercepted() {
    for text in [
        "",
        "不要打开黑板",
        "怎么打开黑板",
        "打开黑板并计算",
        "他说打开黑板",
        "\"打开黑板\"",
        "do not open blackboard",
        "open whiteboard and draw a circle",
        "打开\n黑板",
    ] {
        assert_eq!(board_command(text), None, "{text}");
    }
    assert_eq!(board_command(&"打开黑板".repeat(100)), None);
}
