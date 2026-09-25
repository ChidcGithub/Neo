//! # neo-tools
//!
//! Neo 可调用的工具集。**纯逻辑、无 UI、无网络** —— 上层（`neo-app`）负责
//! 弹确认框与渲染，本层只负责「给定参数、在工作区内、把事做完并把结果说清楚」。
//!
//! ## 1. 六个工具与它们的边界
//!
//! | 工具 | 用途 | 风险 | 边界（什么时候**不该**用它） |
//! |---|---|---|---|
//! | [`read_file`](tools::read_file) | 读取文本文件内容给模型看 | Read | 图片走 `view_image`，Office 文档走 `read_document` |
//! | [`read_document`](tools::read_document) | 提取 Word、PPT 和文本的正文并按字符分页 | Read | 图片走 `view_image`，格式局限与提取截断见 warning |
//! | [`view_image`](tools::view_image) | 查看图片：格式、尺寸、可选原始数据 | Read | 不解码像素、不做识别；要看画面内容交给多模态模型 |
//! | [`write_file`](tools::write_file) | 写入／覆盖整个文件 | Write | 只改文件的一小段要用 `edit_file`（避免整文件重写） |
//! | [`edit_file`](tools::edit_file) | 精确替换文件里的一段文本 | Write | 新建文件用 `write_file`；要重排全文用 `write_file` |
//! | [`open_file`](tools::open_file) | 在用户机器上打开文件／定位它 | Open | 只「交给系统」，不读内容、不解析；给模型读内容用 `read_file` |
//! | [`powershell`](tools::powershell) | 在工作区内执行 PowerShell 命令（可后台） | Exec | 能用专用文件/文档/图像工具表达的，就不要用 shell 绕（可审计性差） |
//!
//! 一句话记法：**读文本 `read_file`、看图 `view_image`、给人看 `open_file`、
//! 整写 `write_file`、点改 `edit_file`、其余 `powershell`。**
//!
//! ## 2. 命名与参数风格（强制统一）
//!
//! - 工具名：`snake_case` 动词短语，动词在前（`read_` / `write_` / `edit_` / `open_`）；
//! - 参数名：全小写 `snake_case`，**路径一律叫 `path`**，内容一律叫 `content`；
//! - 所有参数都在 [`Param`] 里声明一次，JSON Schema 由它生成 ——
//!   文档、schema、取值校验三处不可能漂移；
//! - 布尔参数默认 `false` 且语义为「更危险的那一侧」（如 `overwrite`）；
//! - 整数参数都声明上下限，越界即 [`ErrorKind::BadArguments`]，不会静默截断。
//!
//! ## 3. 调用契约（一轮 agent loop）
//!
//! ```text
//! 模型 → ToolCall{ name, args }
//!        ↓
//!   Policy::decide        ── Deny   → Outcome::fail(NotAllowed) 回给模型
//!        ↓ Confirm                  ── 弹窗等用户点「允许」
//!        ↓ Allow
//!   tools::dispatch(scope, name, args) → Outcome
//!        ↓
//! 把 Outcome::to_model_json 作为 role=tool 的消息回灌 → 模型继续
//! ```
//!
//! ## 4. 安全约束（本层强制，不依赖上层自觉）
//!
//! 1. **工作区围栏**：所有 `path` 经 [`Scope::resolve`] 解析，`..` 逃逸与
//!    工作区外的绝对路径一律 [`ErrorKind::NotAllowed`]；已存在的路径还会做一次
//!    `canonicalize` 复查，挡住符号链接绕行。
//! 2. **体积上限**：读 1 MiB / 写 1 MiB / 图片 8 MiB / 命令输出 64 KiB（超限截断并标记）。
//! 3. **时间上限**：`powershell` 前台模式默认 **500 s**、上限 1800 s，超时杀进程并返回
//!    [`ErrorKind::Timeout`]；`background: true` 则立即返回、不等结果（长驻进程用）。
//! 4. **不猜**：参数缺失、类型不对、`old_string` 不唯一，一律报错，
//!    绝不"尽力而为"地改错地方。
//! 5. **只读模式**：`Policy.read_only` 打开时，Write/Exec 工具直接被拒。
//!
//! ## 5. 返回结构
//!
//! 成功与失败**同一个外壳**，靠 `ok` 区分 —— 模型只学一种形状：
//!
//! ```json
//! { "ok": true,  "tool": "read_file", "summary": "读取 src/a.rs（42 行 / 1.2 KB）",
//!   "data": { "path": "src/a.rs", "lines": 42, "content": "…" } }
//! { "ok": false, "tool": "edit_file",
//!   "error": { "kind": "not_unique", "message": "…", "hint": "…" } }
//! ```
//!
//! `summary` 给人看（UI 卡片一行），`data` / `error` 给模型看（结构化）。

pub mod documents;
pub mod policy;
pub mod present;
pub mod result;
pub mod scope;
pub mod spec;
pub mod tools;

pub use policy::{Decision, Policy, Risk};
pub use result::{Args, ErrorKind, Outcome, ToolError};
pub use scope::Scope;
pub use spec::{find, openai_tools, registry, tool_declarations, Param, Tool, Ty};

/// 所有工具名（顺序即 registry 顺序），UI 与测试共用。
pub fn tool_names() -> Vec<&'static str> {
    registry().iter().map(|t| t.name).collect()
}

/// 按名字取工具并执行。
///
/// **这是唯一的执行入口**：上层不要绕过它直接调 [`tools`] 里的函数，
/// 否则围栏与参数校验都会漏掉。
pub fn dispatch(scope: &Scope, name: &str, args: &serde_json::Value) -> Outcome {
    match find(name) {
        Some(tool) => (tool.run)(scope, &Args::new(tool, args)),
        None => Outcome::fail(
            "unknown",
            ToolError::bad_args(format!("没有名为 `{name}` 的工具"))
                .with_hint(format!("可用工具：{}", tool_names().join(", "))),
        ),
    }
}

/// 把结果压成回灌给模型的字符串（已按统一上限截断）。
///
/// 上层只该用这个函数生成 `role = "tool"` 的消息内容，别自己 `to_string` ——
/// 否则一个 `powershell` 的长回显就能把上下文吃光。
pub fn to_model_message(outcome: &Outcome) -> String {
    outcome.to_model_json(tools::limits::MODEL_JSON_CHARS)
}
