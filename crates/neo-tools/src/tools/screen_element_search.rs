//! 在有界快照内搜索控件；刷新时可先按窗口和区域缩小枚举范围。
use serde_json::{json, Value};
use crate::result::{Args, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;
use super::screen_uia::{self, ScreenElement};
use super::screen::Rect;

pub static PARAMS: &[Param] = &[
    Param::opt_text("query", "按钮或控件名称中的关键词；不填则返回全部候选。"),
    Param::opt_text("role", "控件类型过滤，例如 Button、Edit、MenuItem。"),
    Param::opt_text("position", "位置过滤：top、bottom、left、right、center、middle 或四个角。"),
    Param::opt_text("window", "窗口标题包含过滤；refresh=true 时在遍历控件前过滤窗口。"),
    Param::opt_text("window_id", "overview 返回的精确窗口标识。"),
    Param::opt_int("x", "局部区域左上角，需给全 x/y/width/height。", 0, -32768, 32768),
    Param::opt_int("y", "局部区域左上角。", 0, -32768, 32768),
    Param::opt_int("width", "局部区域宽度，必须为正。", 0, 0, 65536),
    Param::opt_int("height", "局部区域高度，必须为正。", 0, 0, 65536),
    Param::opt_int("limit", "最多返回多少个候选，另有硬字符预算。", 10, 1, 50),
    Param::flag("refresh", "true = 刷新指定窗口（未指定时为焦点窗口）；false 只读缓存，失效时明确要求刷新。"),
];

pub fn preview(args: &Args) -> String {
    let query = args.opt_str("query").ok().unwrap_or_default();
    if query.trim().is_empty() { "搜索屏幕元素".to_owned() }
    else { format!("搜索屏幕元素：{}", query.trim()) }
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
    let position = args.opt_str("position")?.trim().to_ascii_lowercase();
    if !matches!(position.as_str(), "" | "top" | "bottom" | "left" | "right" | "center" | "middle" | "top_left" | "top_right" | "bottom_left" | "bottom_right") {
        return Err(ToolError::bad_args("position 不是支持的位置过滤值"));
    }
    let window = args.opt_str("window")?;
    let window_id = super::screen_elements::parse_window_id(&args.opt_str("window_id")?)?;
    let region = super::screen_elements::query_region(args)?;
    let limit = args.opt_int("limit")? as usize;
    let refresh = args.flag("refresh")?;
    let _interaction = screen_uia::INTERACTION.lock().unwrap();
    if refresh {
        screen_uia::cache_invalidate();
        let filter = Some(window.trim()).filter(|v| !v.is_empty());
        let spec = screen_uia::Query { window_id, keyword: query.trim(), region };
        let elements = screen_uia::enumerate_query(filter, filter.is_some() || window_id.is_some(), 600, 800, &spec)?;
        screen_uia::cache_store(&elements);
    }
    let (snapshot_id, elements) = screen_uia::cache_snapshot().ok_or_else(|| {
        ToolError::not_found("屏幕元素快照缺失或已失效")
            .with_hint("重新运行 screen_elements，或显式给 refresh=true；不会静默切换到其他窗口")
    })?;
    let vs = super::screen::virtual_screen();
    let matches: Vec<&ScreenElement> = elements.iter().filter(|element| {
        matches_element(element, &query, &role, &position, vs)
            && element.window.to_lowercase().contains(&window.trim().to_lowercase())
            && window_id.is_none_or(|id| element.identity.hwnd == id)
            && region.is_none_or(|r| screen_uia::intersects(element.rect, r))
    }).collect();
    let data = search_data(&snapshot_id, &matches, elements.len(), limit, refresh, vs);
    Ok(Outcome::ok("screen_element_search", format!("找到 {} 个屏幕元素候选", matches.len()), data))
}

const SEARCH_CHARS: usize = 12_000;
fn search_data(snapshot: &str, matches: &[&ScreenElement], total: usize, limit: usize, refreshed: bool, vs: Rect) -> Value {
    let mut data = json!({
        "snapshot_id": snapshot, "matched": matches.len(), "shown": 0,
        "total_candidates": total, "refreshed": refreshed, "truncated": false,
        "char_budget": SEARCH_CHARS, "elements": [],
        "next": "候选仅来自有界快照，不保证覆盖全部控件。确认唯一目标后，将本次 snapshot_id 和 element_id 一起传给 click；动作后必须刷新。"
    });
    for element in matches.iter().take(limit) {
        let name: String = element.name.chars().take(128).collect();
        let window: String = element.window.chars().take(128).collect();
        let item = json!({"element_id": element.id, "role": element.role, "name": name, "window": window,
            "window_id": element.identity.hwnd.to_string(), "rect": element.rect,
            "screen_region": region_name(element.rect, vs),
            "text_truncated": name != element.name || window != element.window});
        data["elements"].as_array_mut().unwrap().push(item);
        data["shown"] = json!(data["elements"].as_array().unwrap().len());
        if data.to_string().chars().count() > SEARCH_CHARS {
            data["elements"].as_array_mut().unwrap().pop();
            break;
        }
    }
    let shown = data["elements"].as_array().unwrap().len();
    data["shown"] = json!(shown);
    data["truncated"] = json!(shown < matches.len());
    data
}

fn matches_element(element: &ScreenElement, query: &str, role: &str, position: &str, vs: Rect) -> bool {
    let query = query.trim().to_lowercase();
    let role = role.trim().to_ascii_lowercase();
    (query.is_empty() || element.name.to_lowercase().contains(&query))
        && (role.is_empty() || element.role.to_ascii_lowercase() == role)
        && (position.is_empty() || region_matches(element.rect, position, vs))
}

fn region_name(rect: [i32; 4], vs: Rect) -> &'static str {
    let x = i64::from(rect[0]) + i64::from(rect[2]) / 2;
    let y = i64::from(rect[1]) + i64::from(rect[3]) / 2;
    let horizontal = if x < i64::from(vs.x) + i64::from(vs.width) / 3 { "left" }
        else if x >= i64::from(vs.x) + i64::from(vs.width) * 2 / 3 { "right" } else { "center" };
    let vertical = if y < i64::from(vs.y) + i64::from(vs.height) / 3 { "top" }
        else if y >= i64::from(vs.y) + i64::from(vs.height) * 2 / 3 { "bottom" } else { "middle" };
    match (vertical, horizontal) {
        ("top", "left") => "top_left", ("top", "right") => "top_right",
        ("bottom", "left") => "bottom_left", ("bottom", "right") => "bottom_right",
        ("top", _) => "top", ("bottom", _) => "bottom",
        (_, "left") => "left", (_, "right") => "right", _ => "center",
    }
}

fn region_matches(rect: [i32; 4], wanted: &str, vs: Rect) -> bool {
    let actual = region_name(rect, vs);
    actual == wanted || (wanted == "middle" && actual == "center")
        || actual.starts_with(&format!("{wanted}_")) || actual.ends_with(&format!("_{wanted}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn vs() -> Rect { Rect { x: -1920, y: 0, width: 3840, height: 1080 } }
    fn element() -> ScreenElement {
        ScreenElement { id: 1, role: "Button", name: "保存设置".into(), window: "Neo".into(),
            rect: [-1900, 0, 80, 40], identity: screen_uia::ElementIdentity::default() }
    }
    #[test]
    fn region_names_are_spatial() {
        assert_eq!(region_name([-1920, 0, 1, 1], vs()), "top_left");
        assert!(region_matches([-1920, 0, 1, 1], "left", vs()));
    }
    #[test]
    fn matching_uses_name_role_and_position() {
        assert!(matches_element(&element(), "保存", "button", "top_left", vs()));
        assert!(!matches_element(&element(), "删除", "", "", vs()));
    }
    #[test]
    fn omitted_filters_match_every_candidate_and_refresh_is_explicit() {
        let tool = crate::find("screen_element_search").unwrap();
        let value = json!({});
        let args = Args::new(tool, &value);
        assert!(!args.flag("refresh").unwrap());
        assert!(matches_element(&element(), &args.opt_str("query").unwrap(),
            &args.opt_str("role").unwrap(), &args.opt_str("position").unwrap(), vs()));
        assert_eq!(args.opt_int("limit").unwrap(), 10);
    }

    #[test]
    fn escaped_names_fit_hard_budget_and_keep_references() {
        let mut el = element();
        el.name = "\u{0000}\\\"中文".repeat(10_000);
        el.window = el.name.clone();
        let candidates = vec![&el; 50];
        let data = search_data("synthetic-snapshot", &candidates, 50, 50, false, vs());
        assert!(data.to_string().chars().count() <= SEARCH_CHARS);
        assert_eq!(data["truncated"], true);
        assert_eq!(data["snapshot_id"], "synthetic-snapshot");
        assert_eq!(data["elements"][0]["element_id"], 1);
        let out = Outcome::ok("screen_element_search", "合成测试", data);
        let model: Value = serde_json::from_str(&crate::to_model_message(&out)).unwrap();
        assert!(model.get("truncated").is_none());
    }
}
