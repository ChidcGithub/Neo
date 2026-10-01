use super::*;
use egui::pos2;

fn bar() -> Rect {
    Rect::from_min_size(pos2(100.0, 50.0), Vec2::new(220.0, 32.0))
}

#[test]
fn clicking_inside_the_row_keeps_the_confirmation() {
    // 点在行内（包括空白处和删除键上）→ 不取消
    assert!(!should_cancel(true, Some(bar().center()), bar(), false));
    assert!(!should_cancel(true, Some(pos2(101.0, 51.0)), bar(), false));
}

#[test]
fn clicking_anywhere_else_cancels() {
    // 上、下、左、右四个方向都算"别处"
    for pos in [
        pos2(100.0, 20.0),  // 侧栏上方
        pos2(100.0, 400.0), // 侧栏下方
        pos2(5.0, 60.0),    // 左侧边缘
        pos2(900.0, 60.0),  // 主区（会话区）
    ] {
        assert!(
            should_cancel(true, Some(pos), bar(), false),
            "{pos:?} 应当被当作「点别处」"
        );
    }
}

#[test]
fn no_click_means_no_cancel() {
    // 只是移动鼠标/什么都没点 → 保持确认态
    assert!(!should_cancel(false, Some(pos2(900.0, 60.0)), bar(), false));
    assert!(!should_cancel(false, None, bar(), false));
    // 有点击但拿不到坐标（极罕见）→ 保守地不取消
    assert!(!should_cancel(true, None, bar(), false));
}

#[test]
fn escape_always_cancels() {
    assert!(should_cancel(false, None, bar(), true));
    assert!(should_cancel(true, Some(bar().center()), bar(), true));
}
