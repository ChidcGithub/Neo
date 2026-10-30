//! 工具调用的**展示模型** —— 纯逻辑，无 UI。
//!
//! 规则照搬上游 Harness 的 `tool-call-model`（`dsh-client-ui-tool`）：
//!
//! 1. **工具名 → 变体**（`search / read / bash / write / edit / code / screen / present / others`）。
//!    行标题就是变体名，而不是工具自己起的名字 —— 一行工具调用的信息密度
//!    靠"我读了一个文件 + 哪个文件"这两件事就够，工具名反倒没人看。
//! 2. **摘要从参数里按变体取键**（见 [`Variant::summary_keys`]），
//!    取不到就退到"参数里第一个非空字符串"，再退到原始参数的同一行。
//!    例：`bash` 优先用 `description`（模型自己写的一句话），没有才用 `command`。
//! 3. **文件类变体**（read/write/edit）额外给出可打开的文件路径。
//!
//! 状态（running / ok / error / stopped）不在这里算 —— 它来自上层对
//! "这次调用结算了没有、成没成"的判断，见 `neo-app` 的 `ToolState`。

use serde_json::Value;

/// 工具调用在界面上被归成的变体。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    Search,
    Read,
    Bash,
    Write,
    Edit,
    Code,
    /// 屏幕交互（截屏 / 点击 / 拖动）。
    ///
    /// 这一类比"读文件、执行命令"都更接近"直接操作这台机器"，
    /// 单独一类才好认——`screenshot` 在题述里是"看屏幕"，
    /// 与"读一个文件"并不是一回事。
    Screen,
    /// 结果展示（屏幕中央的结果卡）。
    Present,
    /// 认不出来的工具 —— 标题退回工具名本身。
    Others,
}

impl Variant {
    /// 工具名 → 变体。表里没有的一律 `Others`（与上游一致：不猜）。
    pub fn of(tool_name: &str) -> Self {
        match tool_name {
            // ---- 上游原表 ----
            "bash" | "pwsh" => Variant::Bash,
            "read" | "web_fetch" | "cordis_inspect" => Variant::Read,
            "web_search" | "grep" | "glob" => Variant::Search,
            "write" => Variant::Write,
            "edit" => Variant::Edit,
            "run_code" | "cordis_mount" => Variant::Code,
            // ---- Neo 自己的工具 ----
            "read_file" | "read_document" | "view_image" => Variant::Read,
            "write_file" => Variant::Write,
            "edit_file" => Variant::Edit,
            "powershell" => Variant::Bash,
            "screenshot" | "screen_elements" | "screen_element_search" | "desktop_snapshot"
            | "click" | "drag" | "type_text" | "scroll" | "shortcut" | "move_mouse"
            | "app_control" | "wait" => Variant::Screen,
            "present_result" => Variant::Present,
            _ => Variant::Others,
        }
    }

    /// 摘要优先取哪些参数键（顺序即优先级）。
    pub fn summary_keys(self) -> &'static [&'static str] {
        match self {
            // 模型自己写的一句话比命令本身更适合当摘要。
            Variant::Bash => &["description", "command"],
            Variant::Read => &["path", "file_path", "url"],
            Variant::Search => &["query", "pattern", "url"],
            Variant::Write | Variant::Edit => &["path", "file_path"],
            Variant::Code => &["description"],
            // 屏幕工具的参数大多是坐标（整数），没有合适的字符串键 ——
            // 摘要会退到"原始参数那一行"，那正好就是 `{"x":100,"y":200}` 这种，
            // 对"点了哪儿"来说反而是最清楚的说法。
            Variant::Screen => &["description", "text", "keys", "path"],
            Variant::Present => &["title"],
            Variant::Others => &[],
        }
    }

    /// 这个变体的摘要是不是一个"可打开的路径"。
    pub fn is_file_variant(self) -> bool {
        matches!(self, Variant::Read | Variant::Write | Variant::Edit)
    }

    /// 英文函数式名称，用于工具调用行（`Search(...)`）。
    pub fn call_name(self) -> &'static str {
        match self {
            Variant::Search => "Search",
            Variant::Read => "Read",
            Variant::Bash => "Bash",
            Variant::Write => "Write",
            Variant::Edit => "Edit",
            Variant::Screen => "Screen",
            Variant::Present => "Present",
            Variant::Code => "Code",
            Variant::Others => "Tool",
        }
    }

    /// 中文行标题。
    ///
    /// 上游用 `Search` / `Read` / `Bash` … 这批字面量，并在源码里注明
    /// "design literals, not translatable copy"。Neo 面向中文课堂，所以
    /// **结构照搬、词换中文** —— 学生扫一眼要能认出"这是在读文件还是在跑命令"。
    pub fn title(self) -> &'static str {
        match self {
            Variant::Search => "搜索",
            Variant::Read => "读取",
            Variant::Bash => "执行",
            Variant::Write => "写入",
            Variant::Edit => "修改",
            Variant::Screen => "屏幕",
            Variant::Present => "展示",
            Variant::Code => "运行代码",
            Variant::Others => "工具",
        }
    }
}

/// 从参数里派生一行摘要。
///
/// 三级回退（与上游一致）：变体键表 → 参数里第一个非空字符串 → 原始文本首行。
pub fn summary(variant: Variant, args: &Value) -> String {
    if let Some(obj) = args.as_object() {
        for key in variant.summary_keys() {
            if let Some(s) = obj.get(*key).and_then(Value::as_str) {
                if !s.is_empty() {
                    return first_line(s);
                }
            }
        }
        for v in obj.values() {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return first_line(s);
                }
            }
        }
    }
    if args.is_string() {
        return first_line(args.as_str().unwrap_or_default());
    }
    String::new()
}

/// 一行调用形式：`Search(query="…", open_browser=false)`。
///
/// 参数按工具声明顺序排列（模型最常写的在前），最多三个；字符串只留首行
/// 和前 28 个字符，数组/对象折叠成 `[…]` / `{…}`，保证整行可读。
pub fn call_signature(variant: Variant, tool_name: &str, args: &Value) -> String {
    let name = if variant == Variant::Others {
        tool_name
    } else {
        variant.call_name()
    };
    let Some(obj) = args.as_object() else {
        return format!("{name}()");
    };
    let mut keys: Vec<&str> = Vec::new();
    if let Some(tool) = crate::find(tool_name) {
        for param in tool.params {
            if obj.get(param.name).is_some_and(|value| !value.is_null()) {
                keys.push(param.name);
            }
        }
    }
    for key in obj.keys() {
        if !keys.contains(&key.as_str()) && !obj[key].is_null() {
            keys.push(key);
        }
    }
    const MAX_ARGS: usize = 3;
    let total = keys.len();
    let shown: Vec<String> = keys
        .into_iter()
        .take(MAX_ARGS)
        .map(|key| format!("{key}={}", compact_value(&obj[key])))
        .collect();
    if shown.is_empty() {
        format!("{name}()")
    } else {
        let more = if total > shown.len() { "…" } else { "" };
        format!("{name}({}{more})", shown.join(", "))
    }
}

fn compact_value(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let line = first_line(text).replace('"', "'");
            let count = line.chars().count();
            let mut shown: String = line.chars().take(28).collect();
            if count > 28 {
                shown.push('…');
            }
            format!("\"{shown}\"")
        }
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_owned(),
        Value::Array(_) => "[…]".to_owned(),
        Value::Object(_) => "{…}".to_owned(),
    }
}

/// 文件类变体的目标路径（用于"点开文件"）。
pub fn file_path(variant: Variant, args: &Value) -> Option<String> {
    if !variant.is_file_variant() {
        return None;
    }
    let obj = args.as_object()?;
    for key in ["path", "file_path"] {
        if let Some(s) = obj.get(key).and_then(Value::as_str) {
            if !s.is_empty() {
                return Some(first_line(s));
            }
        }
    }
    None
}

/// `Others` 变体的标题：上游写成 `工具名 · 摘要`，这里把"工具名"交给调用方拼。
pub fn others_title(tool_name: &str) -> String {
    tool_name.to_owned()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_owned()
}

#[cfg(test)]
#[path = "present_tests.rs"]
mod tests;
