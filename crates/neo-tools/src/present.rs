//! 工具调用的**展示模型** —— 纯逻辑，无 UI。
//!
//! 规则照搬上游 Harness 的 `tool-call-model`（`dsh-client-ui-tool`）：
//!
//! 1. **工具名 → 变体**（`search / read / bash / write / edit / code / others`）。
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
            "screenshot" | "click" | "drag" => Variant::Screen,
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
            Variant::Screen => &["description", "path"],
            Variant::Others => &[],
        }
    }

    /// 这个变体的摘要是不是一个"可打开的路径"。
    pub fn is_file_variant(self) -> bool {
        matches!(self, Variant::Read | Variant::Write | Variant::Edit)
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
