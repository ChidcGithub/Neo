//! # neo-llm
//!
//! 模型接入层：OpenAI 兼容的 `/chat/completions` 流式客户端，默认指向 DeepSeek。
//!
//! ## 架构
//!
//! egui 跑在主线程，网络不能阻塞它。所以 [`start`] 在**独立线程**里做 HTTP，
//! 增量通过 [`mpsc::Receiver`] 回报；取消用一个 [`AtomicBool`]，
//! 读流消费线程每 100ms 检查取消；连接/阻塞读取不能被标志强制打断，
//! 底层 HTTP 请求受 15 秒连接超时、300 秒总超时约束。
//!
//! ```text
//! 主线程                          neo-llm 线程
//! ──────                          ────────────
//! start(cfg, msgs) ──────────────► POST /chat/completions (stream=true)
//! rx.try_iter() ◄───────────────── Delta{content, reasoning} × N
//!                                  ToolCall{index, id, name, args} × N
//!                                  Done{tool_calls}
//! cancel.store(true) ────────────► （消费侧轮询退出，底层请求受总超时约束）
//! ```
//!
//! ## 工具调用（function calling）
//!
//! 工具声明由 `neo-tools` 生成，这里只做**协议搬运**：
//!
//! - 请求侧：`tools: [{type:"function", function:{...}}]`；`assistant` 消息带
//!   `tool_calls`，执行结果作为 `role:"tool"` + `tool_call_id` 回灌；
//! - 响应侧：参数是**分片**流式下发的（`arguments` 会被切成好几段），
//!   所以这里原样上报 [`ToolCallFrag`]，聚合交给 [`assemble`]。
//!
//! 本层**不执行任何工具、不校验业务参数**；仅在流正常结束且工具参数为完整 JSON
//! 对象时报告可执行的完成事件，防止半截响应被执行。
//!
//! ## 为什么不用异步
//!
//! eframe 不是异步运行时，把 tokio 拖进来只为了一个 HTTP 请求得不偿失。
//! `reqwest::blocking` + 后台线程在这里是更小、更可控的方案。

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

/// 模型接入配置。
/// 思考模式的档位。
///
/// 对应官方 Thinking Mode 的两个**请求体顶层字段**（OpenAI 格式）：
///
/// | 档位 | `thinking` | `reasoning_effort` |
/// |---|---|---|
/// | [`Thinking::Model`] | 不发（服务端默认：开启 + `high`） | 不发 |
/// | [`Thinking::Off`] | `{"type":"disabled"}` | 不发 |
/// | [`Thinking::Low`] | `{"type":"enabled"}` | `"low"` |
/// | [`Thinking::High`] | `{"type":"enabled"}` | `"high"` |
/// | [`Thinking::Max`] | `{"type":"enabled"}` | `"max"` |
///
/// 对外只暴露 low/high/max 三档，**不制造"看起来更细其实一样"的选项**：
/// 官方给的映射里 `minimal`/`low` 都落到 `low`、`medium`/`high`/`xhigh` 全落到
/// `high`、`max`/`ultra` 全落到 `max`，中间档写出来也是同一个效果。
///
/// [`Thinking::Model`] 的意义是"**不表态**"：不发送这两个字段，完全交给服务端。
/// 这是默认值 —— 用户没动过设置时，行为跟加这个功能之前一模一样。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Thinking {
    /// 不发送控制字段，由服务端默认决定（当前默认：开启，强度 `high`）。
    #[default]
    Model,
    /// 明确关闭思考。
    Off,
    /// 开启，强度 `low`。
    Low,
    /// 开启，强度 `high`。
    High,
    /// 开启，强度 `max`。
    Max,
}

impl Thinking {
    /// 界面顺序即此顺序。
    pub const ALL: [Thinking; 5] = [
        Thinking::Model,
        Thinking::Off,
        Thinking::Low,
        Thinking::High,
        Thinking::Max,
    ];

    /// 界面上的一行字。
    pub fn label(self) -> &'static str {
        match self {
            Thinking::Model => "跟随模型",
            Thinking::Off => "关闭",
            Thinking::Low => "低",
            Thinking::High => "中",
            Thinking::Max => "高",
        }
    }

    /// 设置表的键值（落库用）。
    pub fn key(self) -> &'static str {
        match self {
            Thinking::Model => "model",
            Thinking::Off => "off",
            Thinking::Low => "low",
            Thinking::High => "high",
            Thinking::Max => "max",
        }
    }

    /// 从落库的值还原；认不出来一律回到 [`Thinking::Model`]（不表态最安全）。
    pub fn from_key(s: &str) -> Self {
        Thinking::ALL
            .into_iter()
            .find(|t| t.key() == s)
            .unwrap_or_default()
    }

    /// `thinking.type` 的取值；`None` 表示**这个字段整个不出现**。
    fn toggle(self) -> Option<&'static str> {
        match self {
            Thinking::Model => None,
            Thinking::Off => Some("disabled"),
            Thinking::Low | Thinking::High | Thinking::Max => Some("enabled"),
        }
    }

    /// `reasoning_effort` 的取值；只有明确开启时才发。
    fn effort(self) -> Option<&'static str> {
        match self {
            Thinking::Low => Some("low"),
            Thinking::High => Some("high"),
            Thinking::Max => Some("max"),
            Thinking::Model | Thinking::Off => None,
        }
    }

    /// 是否在思考（用于决定要不要把历史推理回传）。
    pub fn is_thinking(self) -> bool {
        matches!(self, Thinking::Low | Thinking::High | Thinking::Max)
    }

    /// 设置面板里的说明行。
    pub fn hint(self) -> &'static str {
        match self {
            Thinking::Model => "不发送控制字段，由服务端决定（默认开启、强度 high）",
            Thinking::Off => "不产出思考过程，响应更快、更省额度",
            Thinking::Low => "轻量思考：适合查资料、改格式这类简单活儿",
            Thinking::High => "标准思考：与不设置时服务端默认档等价",
            Thinking::Max => "最充分的思考：难题、多步推理，耗时与额度都更高",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// 形如 `https://api.deepseek.com`，**不带**路径与末尾斜杠。
    pub base_url: String,
    /// Bearer 令牌。
    pub api_key: String,
    /// 模型 id（如 `deepseek-chat`），不是展示名。
    pub model: String,
    /// 思考模式档位。默认「不表态」。
    pub thinking: Thinking,
    /// 本地保守估算上限；应按接口实际能力调整。
    pub context_tokens: usize,
}

impl Config {
    /// DeepSeek 官方默认配置。
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.deepseek.com".to_owned(),
            api_key: api_key.into(),
            model: "deepseek-chat".to_owned(),
            thinking: Thinking::default(),
            context_tokens: CONTEXT_TOKENS,
        }
    }

    /// 是否具备发起请求的最低条件。
    pub fn is_configured(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.base_url.trim().is_empty()
    }
}

/// 消息角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    /// 工具执行结果的回灌（必须带 `tool_call_id`）。
    Tool,
}

impl Role {
    /// 序列化用的小写名。
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

/// 模型请求的一次工具调用（已聚合）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// 协议要求的调用 id，回灌结果时用它配对。
    pub id: String,
    pub name: String,
    /// 参数字符串 —— **保留原文**，由上层解析（解析失败要说清楚是模型给错了）。
    pub arguments: String,
}

impl ToolCall {
    /// 解析参数。解析失败时返回可展示的原因（上层应包成 `bad_arguments` 工具错误）。
    pub fn parse_arguments(&self) -> Result<serde_json::Value, String> {
        let raw = self.arguments.trim();
        if raw.is_empty() {
            return Ok(serde_json::Value::Object(Default::default()));
        }
        serde_json::from_str(raw).map_err(|e| format!("参数不是合法 JSON：{e}"))
    }
}

/// 流式下发的工具调用分片。
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCallFrag {
    /// 同一次响应内的序号，聚合时按它归并。
    pub index: usize,
    /// 只有第一片带 id。
    pub id: Option<String>,
    /// 只有第一片带函数名。
    pub name: Option<String>,
    /// `arguments` 的分片，需要按序拼接。
    pub args: String,
}

/// 把分片按 `index` 聚合成完整调用。
///
/// 缺 id 的（少数服务端只给 index）补一个稳定的合成 id —— 它只需要在**同一次
/// 请求内**唯一，回灌时能配对即可。
pub fn assemble(frags: &[ToolCallFrag]) -> Vec<ToolCall> {
    let mut slots: Vec<ToolCall> = Vec::new();
    for f in frags {
        while slots.len() <= f.index {
            slots.push(ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        let slot = &mut slots[f.index];
        if let Some(id) = &f.id {
            if !id.is_empty() {
                slot.id = id.clone();
            }
        }
        if let Some(name) = &f.name {
            slot.name.push_str(name);
        }
        slot.arguments.push_str(&f.args);
    }
    // 先保留所有显式 ID，避免补齐时与后面的调用或其他合成 ID 碰撞。
    let mut used: std::collections::HashSet<String> = slots.iter()
        .filter(|c| !c.id.is_empty()).map(|c| c.id.clone()).collect();
    for (i, c) in slots.iter_mut().enumerate() {
        if c.id.is_empty() {
            let base = format!("call_{i}");
            c.id = base.clone();
            let mut suffix = 0;
            while !used.insert(c.id.clone()) {
                suffix += 1;
                c.id = format!("{base}_{suffix}");
            }
        }
    }
    slots.retain(|c| !c.name.is_empty());
    slots
}

/// 一条对话消息。
#[derive(Clone, Debug)]
pub struct Msg {
    pub role: Role,
    pub content: String,
    /// `role = assistant` 时，模型请求的工具调用。
    pub tool_calls: Vec<ToolCall>,
    /// `role = tool` 时，对应哪一次调用。
    pub tool_call_id: Option<String>,
    /// 随这条消息一起发给模型的图片。
    ///
    /// 元素是 **data URL**（`data:image/png;base64,…`）—— 官方 Vision 文档里
    /// 三种给图方式（base64 内联 / 外链 / Files API）中最省事的一种，本地图片
    /// 不用先上传。格式由**字节内容**判定，不看扩展名。
    ///
    /// 本客户端统一预算：PNG/JPEG 单张 ≤ 4 MiB、每边 ≤ 4096、≤ 4096×2160 像素，
    /// 整轮最多 4 张，每张预留 4096 个估算 token（非供应商精确计费）。
    /// 请求总 JSON ≤ 32 MiB；文本和图片仍共同受 32K 上下文预算约束。
    ///
    /// 只有多模态模型会真的"看"这张图；纯文本模型拿到的是 `content` 里的文字。
    pub images: Vec<String>,
    /// `role = assistant` 时，该轮产出的思考过程（`reasoning_content`）。
    ///
    /// **带 `tools` 的请求必须把它完整回传**，否则服务端返回 400 ——
    /// 官方原话："for requests carrying the `tools` parameter, the
    /// `reasoning_content` must be fully passed back to the API in all
    /// subsequent requests, even for turns where the model did not perform a
    /// tool call." 所以这里存着，由 [`build_wire`] 决定发不发。
    pub reasoning: Option<String>,
    // 应用层在复制超大输入前即可拒绝；共同入口绝不把错误占位发给模型。
    budget_error: Option<String>,
}

impl Msg {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
            reasoning: None,
            budget_error: None,
        }
    }

    /// 附上一张图（data URL）。多模态模型会直接看到它。
    pub fn with_image(mut self, data_url: impl Into<String>) -> Self {
        let url = data_url.into();
        if !url.is_empty() {
            self.images.push(url);
        }
        self
    }

    /// 一次附上多张图（调用方手里是 `Vec<String>` 时用它，省一层循环）。
    pub fn with_image_list(mut self, urls: &[String]) -> Self {
        for url in urls {
            if !url.is_empty() {
                self.images.push(url.clone());
            }
        }
        self
    }

    /// 附上该轮的思考过程（回传用）。
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        let text = reasoning.into();
        if !text.is_empty() {
            self.reasoning = Some(text);
        }
        self
    }

    /// 带上工具调用的 assistant 消息。
    pub fn assistant_with_tools(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            images: Vec::new(),
            reasoning: None,
            budget_error: None,
        }
    }

    /// 一次工具调用的结果回灌。
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            images: Vec::new(),
            reasoning: None,
            budget_error: None,
        }
    }
}

/// 默认一百万；不是供应商能力承诺，不按模型名称猜测。
pub const CONTEXT_TOKENS: usize = 1_000_000;
pub const MIN_CONTEXT_TOKENS: usize = 8192;
pub const MAX_CONTEXT_TOKENS: usize = 4_000_000;

pub fn restored_context_tokens(value: Option<&str>) -> usize {
    value.and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (MIN_CONTEXT_TOKENS..=MAX_CONTEXT_TOKENS).contains(n))
        .unwrap_or(CONTEXT_TOKENS)
}
pub const OUTPUT_TOKENS: usize = 4096;
const MIN_OUTPUT_TOKENS: usize = 1024;
const PROTOCOL_TOKENS: usize = 1024;
const TOOL_RESERVE: usize = 4096;
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGES: usize = 4;
const IMAGE_TOKENS: usize = 4096;
// 与 neo-tools::tools::view_image 保持一致；跨 crate 一致性由离线测试约束。
pub const MAX_MODEL_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MODEL_IMAGE_EDGE: u32 = 4096;
pub const MAX_MODEL_IMAGE_PIXELS: u64 = 4096 * 2160;

/// 不是模型精确 tokenizer：每个 UTF-8 字节计一个估算 token。
/// 中文通常每字计三份，ASCII 也不假定能压缩，避免随机串/代码被低估。
/// 不扫描或复制字符串；供应商的特殊协议和视觉计费仍可能不同。
pub fn estimate_text_tokens(text: &str) -> usize {
    text.len()
}

impl Msg {
    pub fn is_rejected(&self) -> bool {
        self.budget_error.is_some()
    }

    /// 保留错误而非把超限用户意图替换成可发送的短文本。
    pub fn rejected(reason: impl Into<String>) -> Self {
        let mut msg = Self::new(Role::User, "");
        msg.budget_error = Some(reason.into());
        msg
    }
}

// 只计数，不创建序列化大副本；转义后的真实字节也计入上限。
struct ByteCounter {
    bytes: usize,
    limit: usize,
}
impl std::io::Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("请求序列化字节超过预算"));
        }
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn serialized_size(value: &impl Serialize, limit: usize) -> Result<usize, String> {
    let mut counter = ByteCounter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| "请求序列化字节超过预算，尚未发送".to_owned())?;
    Ok(counter.bytes)
}

/// 不解码整张图或分配像素：在 base64 上按位置读取 PNG/JPEG 尺寸元数据。
pub fn validate_image(url: &str) -> Result<(), String> {
    let bad = || format!(
        "图片须为有效 PNG/JPEG data URL，单张不超过 {} 字节、{} 边长和 {} 像素",
        MAX_MODEL_IMAGE_BYTES, MAX_MODEL_IMAGE_EDGE, MAX_MODEL_IMAGE_PIXELS
    );
    let (mime, data) = url.split_once(',').ok_or_else(bad)?;
    if !matches!(mime, "data:image/png;base64" | "data:image/jpeg;base64")
        || data.len() > MAX_MODEL_IMAGE_BYTES.div_ceil(3) * 4
        || data.len() % 4 != 0
        || data.is_empty()
    {
        return Err(bad());
    }
    let digit = |b: u8| -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let padding = data.bytes().rev().take_while(|b| *b == b'=').count();
    if padding > 2
        || !data.as_bytes()[..data.len() - padding]
            .iter()
            .all(|b| digit(*b).is_some())
    {
        return Err(bad());
    }
    let len = data.len() / 4 * 3 - padding;
    if len > MAX_MODEL_IMAGE_BYTES {
        return Err(bad());
    }
    let byte = |i: usize| -> Option<u8> {
        if i >= len {
            return None;
        }
        let p = i / 3 * 4;
        let d = |j| digit(data.as_bytes()[p + j]).unwrap_or(0);
        Some(match i % 3 {
            0 => d(0) << 2 | d(1) >> 4,
            1 => d(1) << 4 | d(2) >> 2,
            _ => d(2) << 6 | d(3),
        })
    };
    let be16 = |i| -> Option<u32> { Some((byte(i)? as u32) << 8 | byte(i + 1)? as u32) };
    let dimensions = if mime == "data:image/png;base64" {
        let signature = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        if !signature
            .iter()
            .enumerate()
            .all(|(i, b)| byte(i) == Some(*b))
        {
            return Err(bad());
        }
        Some((
            (be16(16).ok_or_else(bad)? << 16) | be16(18).ok_or_else(bad)?,
            (be16(20).ok_or_else(bad)? << 16) | be16(22).ok_or_else(bad)?,
        ))
    } else {
        if byte(0) != Some(255) || byte(1) != Some(216) {
            return Err(bad());
        }
        let mut p = 2;
        let mut size = None;
        while p + 4 <= len {
            if byte(p) != Some(255) {
                return Err(bad());
            }
            while byte(p) == Some(255) {
                p += 1;
            }
            let marker = byte(p).ok_or_else(bad)?;
            p += 1;
            if marker == 0xda || marker == 0xd9 {
                break;
            }
            let n = be16(p).ok_or_else(bad)? as usize;
            if n < 2 || p + n > len {
                return Err(bad());
            }
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                if n < 8 {
                    return Err(bad());
                }
                size = Some((be16(p + 5).ok_or_else(bad)?, be16(p + 3).ok_or_else(bad)?));
                break;
            }
            p += n;
        }
        size
    };
    let (w, h) = dimensions.ok_or_else(bad)?;
    if w == 0 || h == 0 || w > MAX_MODEL_IMAGE_EDGE || h > MAX_MODEL_IMAGE_EDGE
        || u64::from(w) * u64::from(h) > MAX_MODEL_IMAGE_PIXELS {
        return Err(bad());
    }
    Ok(())
}

fn message_tokens(msg: &Msg, reasoning: bool) -> Result<usize, String> {
    if let Some(error) = &msg.budget_error {
        return Err(error.clone());
    }
    let mut tokens = 32usize.saturating_add(estimate_text_tokens(&msg.content));
    if reasoning {
        tokens = tokens.saturating_add(msg.reasoning.as_deref().map_or(0, estimate_text_tokens));
    }
    for call in &msg.tool_calls {
        tokens = tokens
            .saturating_add(32)
            .saturating_add(estimate_text_tokens(&call.id))
            .saturating_add(estimate_text_tokens(&call.name))
            .saturating_add(estimate_text_tokens(&call.arguments));
    }
    tokens = tokens.saturating_add(msg.tool_call_id.as_deref().map_or(0, estimate_text_tokens));
    if msg.images.len() > MAX_IMAGES {
        return Err("本轮图片超过 4 张，尚未发送".into());
    }
    if msg.role == Role::Tool && !msg.images.is_empty() {
        tokens = tokens.saturating_add(128).saturating_add(msg.tool_call_id.as_deref().map_or(0, str::len));
    }
    for url in &msg.images {
        validate_image(url)?;
        tokens = tokens.saturating_add(IMAGE_TOKENS);
    }
    Ok(tokens)
}

/// 校验完整上下文与工具配对；超限明确拒绝，不静默删除消息。
/// 摘要由调用方独立生成，所有 start 入口共享此预算检查。
pub fn budget_messages(
    config: &Config,
    messages: &mut [Msg],
    tools: &[serde_json::Value],
) -> Result<(), String> {
    for msg in messages.iter() {
        if let Some(e) = &msg.budget_error {
            return Err(e.clone());
        }
    }
    let used = context_usage(config, messages, tools)?;
    if used > config.context_tokens {
        return Err(format!("上下文超过配置的 {} 预算（已预留工具和输出），尚未发送；请压缩历史或减少最新输入/附件", config.context_tokens));
    }
    if messages.iter().map(|m| m.images.len()).sum::<usize>() > MAX_IMAGES {
        return Err("上下文图片超过 4 张，尚未发送；请压缩历史或减少图片".into());
    }
    let mut pending = std::collections::HashSet::new();
    for msg in messages.iter() {
        if msg.role == Role::Tool {
            if !msg.tool_calls.is_empty()
                || !msg
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|id| pending.remove(id))
            {
                return Err("工具结果缺少配对调用，尚未发送".into());
            }
        } else {
            if !pending.is_empty() {
                return Err("工具调用缺少完整结果，尚未发送".into());
            }
            if !msg.tool_calls.is_empty() && msg.role != Role::Assistant {
                return Err("工具调用角色错误，尚未发送".into());
            }
            for call in &msg.tool_calls {
                if call.id.is_empty() || !pending.insert(call.id.as_str()) {
                    return Err("工具调用 id 为空或重复，尚未发送".into());
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err("工具调用缺少完整结果，尚未发送".into());
    }
    Ok(())
}

pub fn context_usage(config: &Config, messages: &[Msg], tools: &[serde_json::Value]) -> Result<usize, String> {
    let schema = serialized_size(&tools, MAX_REQUEST_BYTES)?;
    let fixed = MIN_OUTPUT_TOKENS + PROTOCOL_TOKENS + TOOL_RESERVE.max(schema);
    messages.iter().try_fold(fixed, |sum, msg| {
        message_tokens(msg, !tools.is_empty() && config.thinking != Thinking::Off)
            .map(|n| sum.saturating_add(n))
    })
}

/// 流式过程中回报的事件。
#[derive(Debug)]
pub enum Event {
    /// 一段文本增量。`reasoning` 是思考过程（推理模型才有），通常多数块里为空。
    Delta { content: String, reasoning: String },
    /// 工具调用分片。聚合用 [`assemble`]。
    ToolCall(ToolCallFrag),
    /// 正常结束。
    ///
    /// `tool_calls == true` 表示模型停下来是为了等你执行工具：
    /// 上层应执行完、把结果作为 `role=tool` 消息回灌，再发一轮。
    Done { tool_calls: bool },
    /// 失败（网络 / 鉴权 / 服务端）。携带可展示给用户的说明。
    Failed(String),
}

/// 一次进行中的流式请求。
pub struct Stream {
    pub rx: Receiver<Event>,
    /// 请求取消；读流消费侧每 100ms 检查，阻塞 HTTP 最晚等总超时释放。
    pub cancel: Arc<AtomicBool>,
    /// 发送端已断开（线程结束）。用 Cell：`poll` 拿 &self 也能记账。
    finished: std::cell::Cell<bool>,
}

impl Stream {
    /// 非阻塞地取走目前所有事件。
    ///
    /// 返回空 `Vec` 只代表「此刻没有新事件」，**不代表流已结束** ——
    /// 请求刚发出、首个增量未到达时同样是空。以收到 [`Event::Done`] /
    /// [`Event::Failed`] 为结束标志；若两者都没等到而 [`Stream::is_finished`]
    /// 已成立，说明线程异常退出（panic），调用方应自行按失败收尾。
    pub fn poll(&self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.finished.set(true);
                    break;
                }
            }
        }
        out
    }

    /// 发送端已被 drop（流线程结束）。**正常结束**（Done/Failed 已发出）
    /// 与**异常结束**（线程 panic，什么也没发）都会置位 —— 区别只在
    /// 此前收没收到终止事件。
    pub fn is_finished(&self) -> bool {
        self.finished.get()
    }

    /// 测试用：手工组装一条流（事件由测试扮演；直接 drop 发送端即模拟
    /// 线程异常退出）。
    #[doc(hidden)]
    pub fn new_for_test(rx: Receiver<Event>) -> Stream {
        Stream {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            finished: std::cell::Cell::new(false),
        }
    }
}

/// 模型列表成功响应上限（UTF-8 JSON 字节），错误响应只读取 16 KiB。
pub const MAX_MODELS_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_MODELS_ERROR_BYTES: usize = 16 * 1024;
/// 最多接受的不同模型数；超限拒绝整次刷新，不悄悄截掉模型。
pub const MAX_MODELS: usize = 1024;
/// 模型 ID 的 UTF-8 字节上限（包含首尾空白）。
pub const MAX_MODEL_ID_BYTES: usize = 256;

/// 校验后才允许显示或存储 ID；不截断或删除其中字符，以免指向另一个模型。
pub fn normalize_model_id(id: &str) -> Option<&str> {
    if id.len() > MAX_MODEL_ID_BYTES
        || id.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return None;
    }
    let id = id.trim();
    (!id.is_empty()).then_some(id)
}

/// 从模型商拉取可用模型 id 列表。
///
/// OpenAI 兼容的 `GET /models`，返回 `{"data":[{"id":"..."}, ...]}`。
/// **阻塞**调用，请放到后台线程里（见 `neo-app` 的 `start_model_fetch`）。
pub fn list_models(config: &Config) -> Result<Vec<String>, String> {
    if !config.is_configured() {
        return Err("尚未配置接口地址或密钥".to_owned());
    }
    let url = format!("{}/models", config.base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "无法创建模型列表网络客户端".to_owned())?;
    let resp = client
        .get(&url)
        .bearer_auth(config.api_key.trim())
        .send()
        .map_err(|_| "模型列表请求失败，请检查接口地址、密钥或网络".to_owned())?;
    read_models_response(resp.status(), resp)
}

fn read_models_response(
    status: reqwest::StatusCode,
    reader: impl Read,
) -> Result<Vec<String>, String> {
    if !status.is_success() {
        // 不回显服务端错误正文：它可能包含密钥、完整模型响应或控制字符。
        let _ = std::io::copy(
            &mut reader.take(MAX_MODELS_ERROR_BYTES as u64),
            &mut std::io::sink(),
        );
        return Err(format!("模型列表接口返回 HTTP {}，请检查接口地址、密钥或服务状态", status.as_u16()));
    }
    let mut body = Vec::new();
    reader
        .take((MAX_MODELS_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| "读取模型列表响应失败".to_owned())?;
    if body.len() > MAX_MODELS_RESPONSE_BYTES {
        return Err("模型列表响应超过 1 MiB 上限，已停止读取".to_owned());
    }
    let body = std::str::from_utf8(&body)
        .map_err(|_| "模型列表响应不是有效 UTF-8".to_owned())?;
    parse_models(body)
}

/// 解析 `GET /models` 的响应体。缺失/不安全 ID 跳过，去重保留首次出现顺序。
/// 响应体或不同模型数超限拒绝整次刷新；不将响应正文带入错误信息。
pub fn parse_models(body: &str) -> Result<Vec<String>, String> {
    if body.len() > MAX_MODELS_RESPONSE_BYTES {
        return Err("模型列表响应超过 1 MiB 上限".to_owned());
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "模型列表响应不是合法 JSON".to_owned())?;
    let arr = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .ok_or_else(|| "响应里没有 models/data 数组".to_owned())?;
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in arr {
        let id = item["id"].as_str().or_else(|| item.as_str());
        if let Some(id) = id.and_then(normalize_model_id) {
            if seen.insert(id) {
                if out.len() == MAX_MODELS {
                    return Err(format!("模型列表超过 {MAX_MODELS} 个模型上限"));
                }
                out.push(id.to_owned());
            }
        }
    }
    if out.is_empty() {
        return Err("模型列表没有有效模型 ID".to_owned());
    }
    Ok(out)
}

/// 发起一次纯文本流式对话（不带工具）。
pub fn start(config: Config, messages: Vec<Msg>) -> Stream {
    start_with_tools(config, messages, Vec::new())
}

/// 发起一次流式对话，可携带工具声明。
///
/// 立即返回；HTTP 在后台线程进行。结束标志是收到 [`Event::Done`] /
/// [`Event::Failed`]（线程退出后发送端被 drop、通道断开是兜底），
/// **不要**用「poll 出空 Vec」判断结束 —— 那可能只是暂无新事件。
pub fn start_with_tools(
    config: Config,
    messages: Vec<Msg>,
    tools: Vec<serde_json::Value>,
) -> Stream {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));

    {
        let cancel = Arc::clone(&cancel);
        let tx_fallback = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("neo-llm-stream".to_owned())
            .spawn(move || {
                run(config, messages, tools, &tx, &cancel);
                // tx 在这里 drop，接收端据此得知流已结束。
            });
        if let Err(e) = spawned {
            // 线程起不来（句柄耗尽等）不能整应用 panic —— 别的线程创建点
            // 全部走降级。发一条失败事件走正常流错误路径（错误框提示）。
            let _ = tx_fallback.send(Event::Failed(format!("无法启动 LLM 线程：{e}")));
        }
    }

    Stream {
        rx,
        cancel,
        finished: std::cell::Cell::new(false),
    }
}

#[derive(Serialize)]
struct WireFunction<'a> {
    name: &'a str,
    /// 参数是**字符串**（不是对象）—— OpenAI 协议的既定形状。
    arguments: &'a str,
}

#[derive(Serialize)]
struct WireToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunction<'a>,
}

/// 消息正文。
///
/// 没有图时是**纯字符串**（与加多模态之前逐字节一致）；带图时才是
/// OpenAI 兼容的 **content blocks 数组**。不无条件用数组，是为了让
/// 纯文本这条最常走的路保持原样 —— 少一个字段形状就少一类 422。
#[derive(Serialize)]
#[serde(untagged)]
enum WireContent<'a> {
    Text(&'a str),
    Parts(Vec<WirePart<'a>>),
}

/// 一个 content block。`#[serde(tag = "type")]` 正好生成官方要的形状：
/// `{"type":"text","text":…}` / `{"type":"image_url","image_url":{"url":…}}`。
#[derive(Serialize)]
#[serde(tag = "type")]
enum WirePart<'a> {
    #[serde(rename = "text")]
    Text { text: &'a str },
    #[serde(rename = "image_url")]
    Image { image_url: WireImageUrl<'a> },
}

#[derive(Serialize)]
struct WireImageUrl<'a> {
    url: &'a str,
}

/// 一条消息的正文：无图给字符串，有图给 blocks。
fn wire_content(m: &Msg) -> WireContent<'_> {
    if m.images.is_empty() {
        return WireContent::Text(&m.content);
    }
    let mut parts = Vec::with_capacity(m.images.len() + 1);
    // 文字在前、图在后：官方示例就是这个顺序，也让"先说要干什么再看图"读起来顺。
    if !m.content.is_empty() {
        parts.push(WirePart::Text { text: &m.content });
    }
    for url in &m.images {
        parts.push(WirePart::Image {
            image_url: WireImageUrl { url },
        });
    }
    WireContent::Parts(parts)
}

#[derive(Serialize)]
struct WireMessage<'a> {
    role: &'a str,
    content: WireContent<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
    /// 历史推理。**只在带 `tools` 的请求里回传** —— 官方要求（否则 400），
    /// 而不带 `tools` 时服务端会忽略它，传了也没意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,
}

#[derive(Serialize)]
struct WireThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: Vec<WireMessage<'a>>,
    stream: bool,
    max_tokens: usize,
    /// 空数组时整个字段不出现 —— 不支持 function calling 的服务端不会因为
    /// 一个空 `tools` 而报错。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
    /// 思考开关：`{"type": "enabled" | "disabled"}`。
    /// 跟随模型时整个字段不出现 —— 由服务端默认决定。
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<WireThinking>,
    /// 思考强度：`"low" | "high" | "max"`。
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
}

/// 组装一次 `/chat/completions` 的请求体（JSON）。
///
/// 与发送路径共用字段编码，但不执行历史裁剪/预算校验，且会分配完整 JSON。
/// 仅供有界输入的测试与诊断；真正发送前由 start 的共同入口再校验。
pub fn request_body(
    config: &Config,
    messages: &[Msg],
    tools: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::to_value(build_wire(config, messages, tools)).unwrap_or(serde_json::Value::Null)
}

/// 组装请求体。
///
/// 抽成独立函数只为了一件事：**能被测试直接断言**。
/// `tools` 必须是「每元素一条工具」的扁平数组 —— 多包一层数组时服务端会返回
/// `422 ... tools[0][0].function: invalid type: map, expected unit`，
/// 这种错在真机上才炸，测试必须能提前拦住。
fn build_wire<'a>(
    config: &'a Config,
    messages: &'a [Msg],
    tools: Vec<serde_json::Value>,
) -> WireRequest<'a> {
    let has_tools = !tools.is_empty();
    let keep_reasoning = has_tools && config.thinking != Thinking::Off;
    let schema = serialized_size(&tools, MAX_REQUEST_BYTES).unwrap_or(config.context_tokens);
    let input = messages.iter().try_fold(0usize, |sum, msg| {
        message_tokens(msg, keep_reasoning).map(|n| sum.saturating_add(n))
    }).unwrap_or(config.context_tokens);
    let max_tokens = config.context_tokens
        .saturating_sub(PROTOCOL_TOKENS + TOOL_RESERVE.max(schema))
        .saturating_sub(input)
        .clamp(MIN_OUTPUT_TOKENS, OUTPUT_TOKENS);
    WireRequest {
        model: &config.model,
        messages: messages
            .iter()
            .enumerate()
            .flat_map(|(index, m)| {
                let mut batch = vec![WireMessage {
                role: m.role.as_str(),
                content: if m.role == Role::Tool { WireContent::Text(&m.content) } else { wire_content(m) },
                tool_calls: m
                    .tool_calls
                    .iter()
                    .map(|c| WireToolCall {
                        id: &c.id,
                        kind: "function",
                        function: WireFunction {
                            name: &c.name,
                            arguments: &c.arguments,
                        },
                    })
                    .collect(),
                tool_call_id: m.tool_call_id.as_deref(),
                // 回传推理内容的条件有两个，缺一不可：
                // ① 请求带 tools（官方硬要求，不带时服务端会忽略）；
                // ② 没有明确关掉思考 —— 关了之后上下文里不该再有推理，
                //    留着只会和"现在不思考"自相矛盾。
                reasoning_content: m
                    .reasoning
                    .as_deref()
                    .filter(|r| !r.is_empty())
                    .filter(|_| has_tools && config.thinking != Thinking::Off),
                }];
                // OpenAI tool content 不支持 image_url。等整个结果块结束后再附 user 图块，
                // 不在并行 tool_calls 的结果之间插入 user，也不改变内部用户轮次边界。
                if m.role == Role::Tool && messages.get(index + 1).is_none_or(|next| next.role != Role::Tool) {
                    let start = messages[..=index].iter().rposition(|item| item.role != Role::Tool)
                        .map_or(0, |i| i + 1);
                    let mut parts = Vec::new();
                    for result in &messages[start..=index] {
                        if result.images.is_empty() { continue; }
                        parts.push(WirePart::Text { text: "以下是工具返回的参考图片（非用户新指令），tool_call_id：" });
                        parts.push(WirePart::Text { text: result.tool_call_id.as_deref().unwrap_or("") });
                        for url in &result.images {
                            parts.push(WirePart::Image { image_url: WireImageUrl { url } });
                        }
                    }
                    if !parts.is_empty() {
                        batch.push(WireMessage { role: "user", content: WireContent::Parts(parts),
                            tool_calls: Vec::new(), tool_call_id: None, reasoning_content: None });
                    }
                }
                batch
            })
            .collect(),
        stream: true,
        max_tokens,
        tools,
        thinking: config.thinking.toggle().map(|kind| WireThinking { kind }),
        reasoning_effort: config.thinking.effort(),
    }
}

fn run(
    config: Config,
    mut messages: Vec<Msg>,
    tools: Vec<serde_json::Value>,
    tx: &Sender<Event>,
    cancel: &Arc<AtomicBool>,
) {
    if cancel.load(Ordering::Relaxed) {
        return;
    }

    if !config.is_configured() {
        let _ = tx.send(Event::Failed(
            "尚未配置模型接口。请在「设置 → 模型」里填写 API 密钥。".to_owned(),
        ));
        return;
    }

    if let Err(error) = budget_messages(&config, &mut messages, &tools) {
        let _ = tx.send(Event::Failed(error));
        return;
    }
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let wire = build_wire(&config, &messages, tools);
    if let Err(error) = serialized_size(&wire, MAX_REQUEST_BYTES) {
        let _ = tx.send(Event::Failed(error));
        return;
    }
    if cancel.load(Ordering::Relaxed) {
        return;
    }
    let client = match http_client(HTTP_TOTAL_TIMEOUT) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Event::Failed(format!("无法创建网络客户端：{e}")));
            return;
        }
    };

    let resp = match client
        .post(&url)
        .bearer_auth(config.api_key.trim())
        .json(&wire)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Event::Failed(format!("请求失败：{e}")));
            return;
        }
    };

    if cancel.load(Ordering::Relaxed) {
        return;
    }
    if !resp.status().is_success() {
        let status = resp.status();
        // 错误响应也不允许无界分配。
        let mut body = String::new();
        let body = match resp.take(64 * 1024).read_to_string(&mut body) {
            Ok(_) => body,
            Err(e) => {
                let _ = tx.send(Event::Failed(format!(
                    "接口返回 {status}，错误响应本身也读取失败：{e}"
                )));
                return;
            }
        };
        let hint = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| v["message"].as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| truncate(&body, 300));
        let _ = tx.send(Event::Failed(format!(
            "接口返回 {status}：{hint}（检查模型名与密钥，或到「设置 → 模型」调整）"
        )));
        return;
    }

    // `blocking::Response` 实现了 `Read`，逐行读 SSE。
    //
    // 读流阶段消费线程每 100ms 检查取消；阻塞读/发送请求不能被 AtomicBool
    // 强制打断，底层连接最晚在总超时到期后释放，不承诺连接阶段立即取消。
    let (line_tx, line_rx) = mpsc::channel::<LineEvent>();
    let reader_cancel = Arc::clone(cancel);
    let reader = std::thread::Builder::new()
        .name("neo-llm-reader".to_owned())
        .spawn(move || {
            read_lines(resp, &line_tx, &reader_cancel);
        });
    if let Err(e) = reader {
        let _ = tx.send(Event::Failed(format!("无法启动读取线程：{e}")));
        return;
    }
    consume_lines(line_rx, tx, cancel);
}

fn read_lines(reader: impl Read, line_tx: &Sender<LineEvent>, reader_cancel: &AtomicBool) {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut bytes = 0usize;
    let mut events = 0usize;
    loop {
        if reader_cancel.load(Ordering::Relaxed) {
            return;
        }
        line.clear();
        // 单行上限 1 MiB：畸形/恶意服务端发一个永不含换行的行时，
        // read_line 会无限往 String 里灌。
        let mut capped = (&mut reader).take(MAX_LINE_BYTES + 1);
        match capped.read_line(&mut line) {
            Ok(0) => {
                let _ = line_tx.send(LineEvent::Eof);
                return;
            }
            Ok(_) => {
                bytes = bytes.saturating_add(line.len());
                events += 1;
                // 在进入无界 channel 前限制总量，包括注释/空行/无效事件。
                if bytes > MAX_STREAM_BYTES || events > MAX_STREAM_EVENTS {
                    let _ = line_tx.send(LineEvent::Failed(
                        "响应累计字节或事件数超过预算，已中断".into(),
                    ));
                    return;
                }
                if line.len() as u64 > MAX_LINE_BYTES {
                    let _ = line_tx.send(LineEvent::Failed(
                        "响应行过长（疑似畸形流），已中断".to_owned(),
                    ));
                    return;
                }
                if line_tx
                    .send(LineEvent::Line(std::mem::take(&mut line)))
                    .is_err()
                {
                    return; // 主循环已走（取消），连接随 resp 的 drop 关闭
                }
            }
            Err(e) => {
                let _ = line_tx.send(LineEvent::Failed(format!("读取流失败：{e}")));
                return;
            }
        }
    }
}

fn consume_lines(line_rx: Receiver<LineEvent>, tx: &Sender<Event>, cancel: &AtomicBool) {
    let mut tool_frags = Vec::new();
    let mut completed = false;
    let mut last_data = Instant::now();
    let mut output_bytes = 0usize;
    let mut output_events = 0usize;
    let mut event_data = String::new();
    let mut has_data = false;
    let mut first_line = true;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let msg = match line_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(m) => m,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 思考型模型可以静默很久，但「五分钟一个字节都没有」基本等于
                // 连接死透了（正常服务端至少会发 keepalive 注释行）。
                if last_data.elapsed() > STALL_TIMEOUT {
                    let _ = tx.send(Event::Failed(
                        "连接超时：五分钟没有收到任何数据，请重试".to_owned(),
                    ));
                    return;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // 读线程异常退出
        };
        last_data = Instant::now();

        let eof = matches!(msg, LineEvent::Eof);
        let line = match msg {
            LineEvent::Line(l) => l,
            // 兼容省略末尾空行的服务端；仍须完整 JSON 和正常结束标志。
            LineEvent::Eof if has_data => String::new(),
            LineEvent::Eof => break,
            LineEvent::Failed(e) => {
                let _ = tx.send(Event::Failed(e));
                return;
            }
        };

        let line = if first_line {
            first_line = false;
            line.strip_prefix('\u{feff}').unwrap_or(&line)
        } else {
            &line
        };
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !line.is_empty() {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            if field == "data" {
                let value = value.strip_prefix(' ').unwrap_or(value);
                let added = value.len().saturating_add(usize::from(has_data));
                if added > MAX_EVENT_BYTES.saturating_sub(event_data.len()) {
                    let _ = tx.send(Event::Failed("响应事件过长（疑似畸形流），已中断".into()));
                    return;
                }
                if has_data {
                    event_data.push('\n');
                }
                event_data.push_str(value);
                has_data = true;
            }
            continue; // 注释 / 其他字段不触发 dispatch，也不打断 data 拼接。
        }
        if !has_data {
            continue;
        }
        if event_data == "[DONE]" {
            completed = true;
            break;
        }

        match parse_chunk(&event_data) {
            Ok(chunk) => {
                output_bytes = output_bytes
                    .saturating_add(chunk.content.len())
                    .saturating_add(chunk.reasoning.len());
                output_events = output_events.saturating_add(1 + chunk.tool_calls.len());
                for frag in &chunk.tool_calls {
                    output_bytes = output_bytes
                        .saturating_add(frag.args.len())
                        .saturating_add(frag.id.as_ref().map_or(0, String::len))
                        .saturating_add(frag.name.as_ref().map_or(0, String::len));
                }
                // 在 clone、聚合和发送给 UI 之前检查，失败后不产生可执行 Done。
                if output_bytes > MAX_OUTPUT_BYTES || output_events > MAX_STREAM_EVENTS {
                    let _ = tx.send(Event::Failed(
                        "响应正文/推理/工具参数累计超过预算，已中断".into(),
                    ));
                    return;
                }
                let has_text = !chunk.content.is_empty() || !chunk.reasoning.is_empty();
                let sent = if has_text {
                    tx.send(Event::Delta {
                        content: chunk.content,
                        reasoning: chunk.reasoning,
                    })
                } else {
                    Ok(())
                };
                if sent.is_err() {
                    return; // 接收端已关闭
                }
                for frag in chunk.tool_calls {
                    tool_frags.push(frag.clone());
                    if tx.send(Event::ToolCall(frag)).is_err() {
                        return;
                    }
                }
                if let Some(reason) = chunk.finish_reason {
                    if !matches!(reason.as_str(), "stop" | "tool_calls") {
                        let _ = tx.send(Event::Failed(format!("响应未正常完成：{reason}")));
                        return;
                    }
                    if reason == "tool_calls" && tool_frags.is_empty() {
                        let _ = tx.send(Event::Failed("工具响应缺少调用内容".to_owned()));
                        return;
                    }
                    // 有些兼容服务只发送 finish_reason，不发送 [DONE]。
                    completed = true;
                    break;
                }
            }
            Err(e) => {
                let _ = tx.send(Event::Failed(format!("响应解析失败：{e}")));
                return;
            }
        }
        event_data.clear();
        has_data = false;
        if eof {
            break;
        }
    }

    if !completed {
        let _ = tx.send(Event::Failed(
            "响应流提前结束，未收到正常结束标志".to_owned(),
        ));
        return;
    }
    let calls = assemble(&tool_frags);
    let indices: std::collections::HashSet<_> = tool_frags.iter().map(|f| f.index).collect();
    let mut ids = std::collections::HashSet::new();
    if calls.iter().any(|call| !ids.insert(call.id.as_str())) {
        let _ = tx.send(Event::Failed("工具调用 ID 重复，已阻止执行".to_owned()));
        return;
    }
    if calls.len() != indices.len()
        || calls.iter().any(|call| {
            !matches!(
                serde_json::from_str::<serde_json::Value>(&call.arguments),
                Ok(serde_json::Value::Object(_))
            )
        })
    {
        let _ = tx.send(Event::Failed(
            "工具响应不完整或参数不是 JSON 对象，已阻止执行".to_owned(),
        ));
        return;
    }
    let _ = tx.send(Event::Done {
        tool_calls: !calls.is_empty(),
    });
}

/// 读线程发往主循环的一行（或终止信号）。
enum LineEvent {
    Line(String),
    Eof,
    Failed(String),
}

/// 单个 SSE 行的字节上限。正常的增量块最多几百 KB（大段工具参数）。
const MAX_LINE_BYTES: u64 = 1 << 20;
// 多 data 行拼成的事件也限 1 MiB，在追加到缓冲前拒绝超限。
const MAX_EVENT_BYTES: usize = 1 << 20;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
const MAX_STREAM_EVENTS: usize = 16_384;
const HTTP_TOTAL_TIMEOUT: Duration = Duration::from_secs(300);

fn http_client(total: Duration) -> Result<reqwest::blocking::Client, reqwest::Error> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(total)
        .build()
}

/// 流式读取的静默看门狗：超过它一个字节都没收到就判定连接已死。
const STALL_TIMEOUT: Duration = Duration::from_secs(300);

/// 一次响应里允许的最大工具调用数。服务端故障/恶意响应给出天文序号时，
/// `assemble` 会按序号无上限扩容槽位 —— 在解析入口就拒掉（远程 DoS）。
const MAX_TOOL_CALLS: usize = 128;

/// 从一个 SSE data 块里解析出文本增量、工具调用分片与结束原因。
#[derive(Debug, Default, PartialEq)]
struct Chunk {
    content: String,
    reasoning: String,
    tool_calls: Vec<ToolCallFrag>,
    finish_reason: Option<String>,
}

fn parse_chunk(payload: &str) -> Result<Chunk, String> {
    let v: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("不是合法 JSON：{e}"))?;

    if let Some(msg) = v["error"]["message"].as_str() {
        return Err(msg.to_owned());
    }

    let choice = &v["choices"][0];
    let delta = &choice["delta"];
    let mut chunk = Chunk {
        content: delta["content"].as_str().unwrap_or_default().to_owned(),
        reasoning: delta["reasoning_content"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        finish_reason: choice["finish_reason"]
            .as_str()
            .filter(|reason| !reason.trim().is_empty())
            .map(str::to_owned),
        ..Default::default()
    };

    if let Some(calls) = delta["tool_calls"].as_array() {
        if calls.len() > MAX_TOOL_CALLS {
            return Err("工具调用数量越界".into());
        }
        for (pos, call) in calls.iter().enumerate() {
            // 有的服务端不给 index，用数组下标兜底。
            let index = match call["index"].as_u64() {
                Some(i) if i < MAX_TOOL_CALLS as u64 => i as usize,
                // 天文序号会让 assemble 无上限扩容槽位（远程 DoS）——判负。
                Some(i) => {
                    return Err(format!("工具调用序号 {i} 越界（上限 {MAX_TOOL_CALLS}）"));
                }
                None => pos,
            };
            let id = call["id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let name = call["function"]["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            let args = call["function"]["arguments"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if id.is_none() && name.is_none() && args.is_empty() {
                continue;
            }
            chunk.tool_calls.push(ToolCallFrag {
                index,
                id,
                name,
                args,
            });
        }
    }
    Ok(chunk)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[test]
fn parses_openai_style_model_list() {
    let body = r#"{"object":"list","data":[{"id":"deepseek-chat"},{"id":"deepseek-reasoner"}]}"#;
    assert_eq!(
        parse_models(body).unwrap(),
        vec!["deepseek-chat", "deepseek-reasoner"]
    );
}

#[test]
fn model_list_is_tolerant_and_stable() {
    // 缺 id 的条目跳过、重复项去掉、保留首次出现顺序
    let body = r#"{"data":[{"id":"b"},{"object":"model"},{"id":"a"},{"id":"b"}]}"#;
    assert_eq!(parse_models(body).unwrap(), vec!["b", "a"]);
    // 另一种常见形状也认
    assert_eq!(parse_models(r#"{"models":["x"]}"#).unwrap(), vec!["x"]);
}

#[test]
fn bad_model_response_reports_a_reason() {
    assert!(parse_models("not json").is_err());
    assert!(parse_models(r#"{"data":[]}"#).is_err());
    assert!(parse_models(r#"{"object":"list"}"#).is_err());
}

#[test]
fn model_response_reader_caps_success_and_error_without_echoing_body() {
    use std::io::Cursor;
    let body = vec![b'x'; MAX_MODELS_RESPONSE_BYTES + 100];
    let mut reader = Cursor::new(&body);
    let error = read_models_response(reqwest::StatusCode::OK, &mut reader).unwrap_err();
    assert!(error.contains("1 MiB"));
    assert_eq!(reader.position(), (MAX_MODELS_RESPONSE_BYTES + 1) as u64);

    let mut reader = Cursor::new(&body);
    let error = read_models_response(reqwest::StatusCode::BAD_GATEWAY, &mut reader).unwrap_err();
    assert!(error.contains("502"));
    assert_eq!(reader.position(), MAX_MODELS_ERROR_BYTES as u64);
    let secret = br#"{"error":{"message":"sk-private-key"},"data":["private-model"]}"#;
    let error = read_models_response(reqwest::StatusCode::UNAUTHORIZED, &secret[..]).unwrap_err();
    assert!(error.contains("401"));
    assert!(!error.contains("sk-private-key") && !error.contains("private-model"));

    let mut body = br#"{"data":["ok"]}"#.to_vec();
    body.resize(MAX_MODELS_RESPONSE_BYTES, b' ');
    assert_eq!(read_models_response(reqwest::StatusCode::OK, body.as_slice()).unwrap(), ["ok"]);
    body.push(b' ');
    assert!(parse_models(std::str::from_utf8(&body).unwrap()).is_err());
}

#[test]
fn model_response_errors_never_echo_untrusted_data() {
    struct BrokenReader;
    impl Read for BrokenReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("sk-private-key private-model"))
        }
    }
    for error in [
        read_models_response(reqwest::StatusCode::OK, BrokenReader).unwrap_err(),
        read_models_response(reqwest::StatusCode::UNAUTHORIZED, BrokenReader).unwrap_err(),
        read_models_response(reqwest::StatusCode::OK, &b"\xffsk-private-key"[..]).unwrap_err(),
        parse_models(r#"{"data":["private-model"],"sk-private-key":"#).unwrap_err(),
    ] {
        assert!(!error.contains("sk-private-key") && !error.contains("private-model"));
    }
}

#[test]
fn model_list_id_limits_skip_unsafe_without_changing_identity() {
    let boundary = "a".repeat(MAX_MODEL_ID_BYTES);
    let multibyte = "型".repeat(MAX_MODEL_ID_BYTES / 3);
    let body = serde_json::json!({"models":[
        " z ", "z", "a", boundary, multibyte,
        "x".repeat(MAX_MODEL_ID_BYTES + 1), "型".repeat(MAX_MODEL_ID_BYTES / 3 + 1),
        "\nedge", "bad\rline", "bad\tline", "bad\u{0000}", "bad\u{0085}",
        "bad\u{202e}id", "bad\u{2066}id", "bad\u{2028}id", "", "   "
    ]}).to_string();
    assert_eq!(parse_models(&body).unwrap(), vec!["z".to_owned(), "a".to_owned(), boundary, multibyte]);
    assert!(parse_models(r#"{"data":[{"id":"bad\nmodel"}]}"#).is_err());
}

#[test]
fn model_list_count_limit_applies_after_deduplication() {
    let mut ids: Vec<String> = (0..MAX_MODELS).rev().map(|i| format!("model-{i}")).collect();
    let expected = ids.clone();
    ids.extend(expected.clone());
    let body = serde_json::json!({"data":ids}).to_string();
    assert_eq!(parse_models(&body).unwrap(), expected);
    ids.push("one-too-many".into());
    let error = parse_models(&serde_json::json!({"models":ids}).to_string()).unwrap_err();
    assert!(error.contains(&MAX_MODELS.to_string()));
    assert!(!error.contains("one-too-many"));
}

#[test]
fn list_models_needs_configuration() {
    let cfg = Config::deepseek("");
    assert!(list_models(&cfg).is_err());
}
