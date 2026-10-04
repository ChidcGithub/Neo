//! Explicit local voice commands only; never infer commands from conversation text.
use crate::drawing_runtime::BoardKind;

pub(crate) fn board_command(text: &str) -> Option<BoardKind> {
    if text.len() > 128 {
        return None;
    }
    let text = text.trim().trim_end_matches(['。', '.', '！', '!']).trim();
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    match normalized.as_str() {
        "打开黑板" | "open blackboard" | "open the blackboard" => Some(BoardKind::Blackboard),
        "打开画板" | "打开白板" | "open drawing" | "open whiteboard" | "open the whiteboard" => {
            Some(BoardKind::Drawing)
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "drawing_commands_tests.rs"]
mod tests;
