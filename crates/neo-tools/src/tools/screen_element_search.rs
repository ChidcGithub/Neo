//! Search the most recently enumerated screen elements without re-running UIA.
use serde_json::{json, Value};

use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

use super::screen_uia::{self, ScreenElement};

pub static PARAMS: &[Param] = &[
    Param::opt_text("query", "按钮或控件名称中的关键词；不填则返回全部候选。"),
    Param::opt_text("role", "控件类型过滤，例如 Button、Edit、MenuItem。"),
    Param::opt_text("position", "位置过滤：top、bottom、left、right、center，或 top_left/top_right/bottom_left/bottom_right。"),
    Param::opt_int("limit", "最多返回多少个候选。", 10, 1, 50),
    Param::flag("refresh", "true = 先重新枚举当前焦点窗口；默认使用最近一次 screen_elements 缓存。"),
];

pub fn preview(args: &Args) -> String {
    let query = args.opt_str("query").ok().unwrap_or_default();
    if query.trim().is_empty() {
        "搜索屏幕元素".to_owned()
    } else {
        format!("搜索屏幕元素：{}", query.trim())
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    match act(scope, args) {
        Ok(value) => value,
        Err(error) => Outcome::fail("screen_element_search", error),
    }
}

fn act(_scope: &Scope, args: &Args) -> Result<Outcome, ToolError> {
    let query = args.opt_str("query")?;
    let role = args.opt_str("role")?;
    let position = args.opt_str("position")?;
    let limit = args.opt_int("limit")? as usize;
    let refresh = args.flag("refresh")?;

    let refreshed = refresh || screen_uia::cache_snapshot().is_none();
    if refreshed {
        let elements = screen_uia::enumerate_mode(None, false, 600, 800)?;
        screen_uia::cache_store(&elements);
    }
    let elements = screen_uia::cache_snapshot().ok_or_else(|| {
        ToolError::not_found("没有可搜索的屏幕元素")
            .with_hint("先运行 screen_elements，或给 refresh=true")
    })?;

    let matches: Vec<&ScreenElement> = elements
        .iter()
        .filter(|element| matches_element(element, &query, &role, &position))
        .take(limit)
        .collect();

    let items: Vec<Value> = matches
        .iter()
        .map(|element| {
            let [x, y, w, h] = element.rect;
            let (cx, cy) = element.center();
            json!({
                "element_id": element.id,
                "role": element.role,
                "name": element.name,
                "window": element.window,
                "position": {
                    "x": x, "y": y, "width": w, "height": h,
                    "center_x": cx, "center_y": cy,
                    "screen_region": region_name(element.rect),
                },
                "intent_hint": intent_hint(element.role, &element.name),
                "action": "将 element_id 传给 click；不要自行换算坐标",
            })
        })
        .collect();

    Ok(Outcome::ok(
        "screen_element_search",
        format!("找到 {} 个屏幕元素候选", items.len()),
        json!({
            "query": query,
            "role": role,
            "position_filter": position,
            "matched": items.len(),
            "total_candidates": elements.len(),
            "refreshed_focused_window": refreshed,
            "elements": items,
            "next": if items.is_empty() {
                "换一个关键词、去掉 role/position，或用 screenshot 查看视觉界面"
            } else {
                "确认候选后，把对应 element_id 传给 click；搜索结果来自最近一次 UIA 枚举（如 refreshed_focused_window=true，则刚刷新了焦点窗口）"
            },
        }),
    ))
}

fn matches_element(element: &ScreenElement, query: &str, role: &str, position: &str) -> bool {
    let query = query.trim().to_ascii_lowercase();
    let role = role.trim().to_ascii_lowercase();
    let position = position.trim().to_ascii_lowercase();
    (query.is_empty()
        || element.name.to_ascii_lowercase().contains(&query)
        || element.window.to_ascii_lowercase().contains(&query))
        && (role.is_empty() || element.role.to_ascii_lowercase() == role)
        && (position.is_empty() || region_matches(element.rect, &position))
}

fn region_name(rect: [i32; 4]) -> &'static str {
    let x = rect[0] + rect[2] / 2;
    let y = rect[1] + rect[3] / 2;
    let vs = super::screen::virtual_screen();
    let horizontal = if x < vs.x + vs.width / 3 {
        "left"
    } else if x >= vs.x + vs.width * 2 / 3 {
        "right"
    } else {
        "center"
    };
    let vertical = if y < vs.y + vs.height / 3 {
        "top"
    } else if y >= vs.y + vs.height * 2 / 3 {
        "bottom"
    } else {
        "middle"
    };
    match (vertical, horizontal) {
        ("top", "left") => "top_left",
        ("top", "right") => "top_right",
        ("bottom", "left") => "bottom_left",
        ("bottom", "right") => "bottom_right",
        ("top", _) => "top",
        ("bottom", _) => "bottom",
        (_, "left") => "left",
        (_, "right") => "right",
        _ => "center",
    }
}

fn region_matches(rect: [i32; 4], wanted: &str) -> bool {
    let actual = region_name(rect);
    actual == wanted
        || (wanted == "middle" && actual == "center")
        || actual.starts_with(&format!("{wanted}_"))
}

fn intent_hint(role: &str, name: &str) -> String {
    let action = match role {
        "Button" | "SplitButton" => "执行",
        "Hyperlink" => "打开链接或跳转到",
        "MenuItem" => "选择菜单项",
        "TabItem" => "切换到",
        "Edit" => "输入或编辑",
        "CheckBox" => "切换选项",
        _ => "操作",
    };
    format!("可能用于{action}“{name}”；需结合截图确认")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_names_are_spatial() {
        let vs = super::super::screen::virtual_screen();
        assert_eq!(region_name([vs.x, vs.y, 1, 1]), "top_left");
    }

    #[test]
    fn matching_uses_name_role_and_position() {
        let element = ScreenElement {
            id: 1,
            role: "Button",
            name: "保存设置".into(),
            window: "Neo".into(),
            rect: [0, 0, 80, 40],
        };
        assert!(matches_element(&element, "保存", "button", "top_left"));
        assert!(!matches_element(&element, "删除", "", ""));
    }
}
