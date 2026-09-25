//! # neo-llm
//!
//! 模型接入层：OpenAI 兼容的 `/chat/completions` 流式客户端，默认指向 DeepSeek。
//!
//! ## 架构
//!
//! egui 跑在主线程，网络不能阻塞它。所以 [`start`] 在**独立线程**里做 HTTP，
//! 增量通过 [`mpsc::Receiver`] 回报；取消用一个 [`AtomicBool`]，
//! 流式线程在每次读到新块时检查它 —— 粒度是"一个 SSE 块"，足够灵敏。
//!
//! ```text
//! 主线程                          neo-llm 线程
//! ──────                          ────────────
//! start(cfg, msgs) ──────────────► POST /chat/completions (stream=true)
//! rx.try_iter() ◄───────────────── Delta{content, reasoning} × N
//!                                  ToolCall{index, id, name, args} × N
//!                                  Done{tool_calls}
//! cancel.store(true) ────────────► （读到下一个块时退出）
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
//! 本层**不执行任何工具、不校验参数** —— 它只保证"模型说的话被准确转述"。
//!
//! ## 为什么不用异步
//!
//! eframe 不是异步运行时，把 tokio 拖进来只为了一个 HTTP 请求得不偿失。
//! `reqwest::blocking` + 后台线程在这里是更小、更可控的方案。

use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

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

#[derive(Clone, Debug)]
pub struct Config {
    /// 形如 `https://api.deepseek.com`，**不带**路径与末尾斜杠。
    pub base_url: String,
    /// Bearer 令牌。
    pub api_key: String,
    /// 模型 id（如 `deepseek-chat`），不是展示名。
    pub model: String,
    /// 思考模式档位。默认「不表态」。
    pub thinking: Thinking,
}

impl Config {
    /// DeepSeek 官方默认配置。
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.deepseek.com".to_owned(),
            api_key: api_key.into(),
            model: "deepseek-chat".to_owned(),
            thinking: Thinking::default(),
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
#[derive(Clone, Debug, PartialEq)]
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
    for (i, c) in slots.iter_mut().enumerate() {
        if c.id.is_empty() {
            c.id = format!("call_{i}");
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
    /// 限制（官方）：单张 ≤ 32 MiB、整包 ≤ 48 MiB、一次请求 ≤ 600 张、
    /// 每张按最多 384 token 计费。所以给的是**压缩后的图**（截图直接给 PNG 就行，
    /// 1920×1080 约 2–4 MB），别塞原始位图。
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
        }
    }
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
    /// 置为 `true` 即请求取消。线程在下一个块边界退出。
    pub cancel: Arc<AtomicBool>,
}

impl Stream {
    /// 非阻塞地取走目前所有事件。
    pub fn poll(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            out.push(ev);
        }
        out
    }
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
        .map_err(|e| format!("无法创建网络客户端：{e}"))?;
    let resp = client
        .get(&url)
        .bearer_auth(config.api_key.trim())
        .send()
        .map_err(|e| format!("请求失败：{e}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        let hint = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| v["message"].as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| truncate(&body, 200));
        return Err(format!("接口返回 {status}：{hint}"));
    }
    parse_models(&body)
}

/// 解析 `GET /models` 的响应体。抽成纯函数，便于离线测试。
///
/// 容错两件事：`data` 数组里缺 `id` 的条目跳过；完全不是这个形状时报错而不是返回空。
pub fn parse_models(body: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    let arr = v["data"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .ok_or_else(|| "响应里没有 models/data 数组".to_owned())?;
    let mut out: Vec<String> = Vec::new();
    for item in arr {
        let id = item["id"].as_str().or_else(|| item.as_str());
        if let Some(id) = id {
            let id = id.trim();
            if !id.is_empty() && !out.iter().any(|m| m == id) {
                out.push(id.to_owned());
            }
        }
    }
    if out.is_empty() {
        return Err("模型列表为空".to_owned());
    }
    out.sort();
    Ok(out)
}

/// 发起一次纯文本流式对话（不带工具）。
pub fn start(config: Config, messages: Vec<Msg>) -> Stream {
    start_with_tools(config, messages, Vec::new())
}

/// 发起一次流式对话，可携带工具声明。
///
/// 立即返回；HTTP 在后台线程进行。线程结束后发送端被 drop，
/// 此时 [`Stream::poll`] 会得到空 `Vec` —— 用这一点判断流已结束。
pub fn start_with_tools(
    config: Config,
    messages: Vec<Msg>,
    tools: Vec<serde_json::Value>,
) -> Stream {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));

    {
        let cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("neo-llm-stream".to_owned())
            .spawn(move || {
                run(config, messages, tools, &tx, &cancel);
                // tx 在这里 drop，接收端据此得知流已结束。
            })
            .expect("无法启动 LLM 线程");
    }

    Stream { rx, cancel }
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
/// 与真正发出去的那份**同一段代码**，所以断言它等于断言线上行为。
/// 给测试与诊断用 —— 想弄清"这一轮到底发了什么"时不必抓包。
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
    WireRequest {
        model: &config.model,
        messages: messages
            .iter()
            .map(|m| WireMessage {
                role: m.role.as_str(),
                content: wire_content(m),
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
            })
            .collect(),
        stream: true,
        tools,
        thinking: config.thinking.toggle().map(|kind| WireThinking { kind }),
        reasoning_effort: config.thinking.effort(),
    }
}

fn run(
    config: Config,
    messages: Vec<Msg>,
    tools: Vec<serde_json::Value>,
    tx: &Sender<Event>,
    cancel: &AtomicBool,
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

    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let wire = build_wire(&config, &messages, tools);

    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        // 不设总超时：长回答不该被掐断。
        .build()
    {
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

    if !resp.status().is_success() {
        let status = resp.status();
        // 服务端的错误说明通常就在响应体里，取出来比只报状态码有用得多。
        let body = resp.text().unwrap_or_default();
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
    let mut reader = BufReader::new(resp);
    let mut line = String::new();
    // 收尾块才有 finish_reason，用它判断"是否还要跑工具"。
    let mut saw_tool_call = false;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // 连接结束
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(Event::Failed(format!("读取流失败：{e}")));
                return;
            }
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let Some(payload) = trimmed.strip_prefix("data:") else {
            continue; // 注释行 / 未知前缀
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            break;
        }

        match parse_chunk(payload) {
            Ok(chunk) => {
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
                    saw_tool_call = true;
                    if tx.send(Event::ToolCall(frag)).is_err() {
                        return;
                    }
                }
                if let Some(reason) = chunk.finish_reason {
                    if reason == "tool_calls" {
                        saw_tool_call = true;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(Event::Failed(format!("响应解析失败：{e}")));
                return;
            }
        }
    }

    let _ = tx.send(Event::Done {
        tool_calls: saw_tool_call,
    });
}

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
        finish_reason: choice["finish_reason"].as_str().map(str::to_owned),
        ..Default::default()
    };

    if let Some(calls) = delta["tool_calls"].as_array() {
        for (pos, call) in calls.iter().enumerate() {
            // 有的服务端不给 index，用数组下标兜底。
            let index = call["index"].as_u64().map(|i| i as usize).unwrap_or(pos);
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
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_delta() {
        let payload = r#"{"id":"1","choices":[{"delta":{"content":"你好"}}]}"#;
        let c = parse_chunk(payload).unwrap();
        assert_eq!(c.content, "你好");
        assert_eq!(c.reasoning, "");
        assert!(c.tool_calls.is_empty());
    }

    #[test]
    fn parses_reasoning_delta() {
        let payload = r#"{"choices":[{"delta":{"reasoning_content":"先想想"}}]}"#;
        let c = parse_chunk(payload).unwrap();
        assert_eq!(c.content, "");
        assert_eq!(c.reasoning, "先想想");
    }

    #[test]
    fn ignores_usage_only_chunk() {
        let payload = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let c = parse_chunk(payload).unwrap();
        assert!(c.content.is_empty() && c.reasoning.is_empty() && c.tool_calls.is_empty());
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn surfaces_server_error() {
        let payload = r#"{"error":{"message":"Authentication Fails"}}"#;
        assert_eq!(
            parse_chunk(payload).unwrap_err(),
            "Authentication Fails".to_owned()
        );
    }

    #[test]
    fn parses_tool_call_fragments() {
        // 真实协议里首片带 id/name，后续片只有 arguments 增量。
        let first = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"pa"}}]}}]}"#;
        let second = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#;

        let a = parse_chunk(first).unwrap();
        assert_eq!(a.tool_calls.len(), 1);
        assert_eq!(a.tool_calls[0].index, 0);
        assert_eq!(a.tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(a.tool_calls[0].name.as_deref(), Some("read_file"));

        let b = parse_chunk(second).unwrap();
        assert_eq!(b.tool_calls[0].index, 0);
        assert!(b.tool_calls[0].id.is_none());
        assert_eq!(b.finish_reason.as_deref(), Some("tool_calls"));

        let calls = assemble(&[a.tool_calls[0].clone(), b.tool_calls[0].clone()]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(
            calls[0].parse_arguments().unwrap()["path"],
            serde_json::json!("a.txt")
        );
    }

    #[test]
    fn assemble_handles_parallel_calls_and_missing_ids() {
        let calls = assemble(&[
            ToolCallFrag {
                index: 0,
                id: Some("a".into()),
                name: Some("read_file".into()),
                args: "{}".into(),
            },
            ToolCallFrag {
                index: 1,
                id: None,
                name: Some("bash".into()),
                args: "{}".into(),
            },
        ]);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "call_1", "缺 id 时补合成 id，保证可配对");
    }

    #[test]
    fn bad_arguments_are_reported_not_panicked() {
        let call = ToolCall {
            id: "1".into(),
            name: "read_file".into(),
            arguments: "{not json".into(),
        };
        assert!(call.parse_arguments().is_err());
    }

    #[test]
    fn wire_message_carries_tool_plumbing() {
        let msg = Msg::tool_result("call_1", r#"{"ok":true}"#);
        assert_eq!(msg.role.as_str(), "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));

        let msg = Msg::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: "{}".into(),
            }],
        );
        let wire = serde_json::to_value(WireMessage {
            role: msg.role.as_str(),
            content: wire_content(&msg),
            tool_calls: msg
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
            tool_call_id: None,
            reasoning_content: None,
        })
        .unwrap();
        assert_eq!(wire["tool_calls"][0]["id"], "call_1");
        assert_eq!(wire["tool_calls"][0]["type"], "function");
        assert_eq!(wire["tool_calls"][0]["function"]["name"], "bash");
        assert!(wire.get("tool_call_id").is_none(), "空字段不序列化");
    }

    /// **回归测试**：`tools` 必须是扁平的函数数组。
    ///
    /// 曾经写成 `vec![openai_tools()]`（而 `openai_tools()` 本身已是数组），
    /// 于是线上请求体成了 `tools: [[…]]`，服务端直接
    /// `422 tools[0][0].function: invalid type: map, expected unit`。
    #[test]
    fn wire_tools_are_a_flat_array_of_functions() {
        let cfg = Config::deepseek("sk-x");
        let msgs = vec![Msg::new(Role::User, "hi")];
        let tools = neo_tools::tool_declarations();
        assert_eq!(tools.len(), neo_tools::registry().len());

        let body = serde_json::to_value(build_wire(&cfg, &msgs, tools)).unwrap();
        let arr = body["tools"].as_array().expect("tools 应是数组");
        assert_eq!(arr.len(), neo_tools::registry().len());
        for (i, t) in arr.iter().enumerate() {
            assert!(t.is_object(), "tools[{i}] 必须是单个工具对象，不能是数组");
            assert_eq!(t["type"], "function", "tools[{i}] 缺 type=function");
            assert!(t["function"]["name"].is_string(), "tools[{i}] 缺函数名");
            assert!(
                t["function"]["parameters"]["properties"].is_object(),
                "tools[{i}] 缺 JSON Schema"
            );
        }
        // 注册表里的工具都要出现 —— 数量跟着注册表走，别写死：
        // 写死只会变成"加一个工具就要改一次测试"，那是文档同步测试该管的事。
        assert_eq!(
            arr.len(),
            neo_tools::registry().len(),
            "工具数量与注册表不一致：{:?}",
            neo_tools::tool_names()
        );
        assert!(arr.len() >= 6);

        // ---- 服务端视角的复核 ----
        // 下面这两个结构体只描述服务端**期待**的形状。把我们的请求体反序列化
        // 进去，等价于在本地跑一遍服务端的解析 —— 嵌套数组会在这里失败，
        // 报错形状与线上那条 422 一致（`invalid type: map, expected unit`）。
        #[derive(serde::Deserialize)]
        struct ServerTool {
            #[serde(rename = "type")]
            kind: String,
            function: ServerFunction,
        }
        #[derive(serde::Deserialize)]
        struct ServerFunction {
            name: String,
            #[allow(dead_code)]
            description: String,
            parameters: serde_json::Value,
        }

        let parsed: Vec<ServerTool> = serde_json::from_value(body["tools"].clone())
            .expect("服务端形状解析失败：多半是 tools 被多包了一层数组");
        assert_eq!(parsed.len(), neo_tools::registry().len());
        for t in &parsed {
            assert_eq!(t.kind, "function");
            assert!(!t.function.name.is_empty());
            // properties 必须非空（不是每个工具都有 path —— bash 就没有）
            let props = t.function.parameters["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{} 缺 properties", t.function.name));
            assert!(!props.is_empty(), "{} 的参数表是空的", t.function.name);
        }
    }

    // ---- 多模态（图片输入） ----

    /// 没有图的消息**仍然是纯字符串** —— 别让多模态改动波及最常见的路径。
    #[test]
    fn messages_without_images_keep_plain_string_content() {
        let body = wire_json(Thinking::Model, Vec::new(), &[Msg::new(Role::User, "你好")]);
        assert_eq!(body["messages"][0]["content"], "你好");
        assert!(
            body["messages"][0]["content"].is_string(),
            "无图时 content 必须是字符串：{body}"
        );
    }

    /// **服务端视角**：带图时 content 是 blocks 数组，且 tag 与字段名都对得上。
    #[test]
    fn message_with_image_becomes_content_blocks() {
        #[derive(serde::Deserialize)]
        struct ServerMsg {
            /// 只用来核对角色，不读取内容。
            #[allow(dead_code)]
            role: String,
            content: Vec<ServerPart>,
        }
        #[derive(serde::Deserialize, Debug)]
        #[serde(tag = "type")]
        enum ServerPart {
            #[serde(rename = "text")]
            Text { text: String },
            #[serde(rename = "image_url")]
            Image { image_url: ServerImageUrl },
        }
        #[derive(serde::Deserialize, Debug)]
        struct ServerImageUrl {
            url: String,
        }

        let url = "data:image/png;base64,AAA=";
        let msgs = vec![Msg::new(Role::User, "看看这张图").with_image(url)];
        let body = wire_json(Thinking::Model, Vec::new(), &msgs);
        let parsed: Vec<ServerMsg> =
            serde_json::from_value(body["messages"].clone()).expect("content blocks 形状");
        let parts = &parsed[0].content;
        assert_eq!(parts.len(), 2);
        match &parts[0] {
            ServerPart::Text { text } => assert_eq!(text, "看看这张图"),
            other => panic!("第一块应当是 text：{other:?}"),
        }
        match &parts[1] {
            ServerPart::Image { image_url } => assert_eq!(image_url.url, url),
            _ => panic!("第二块应当是 image_url"),
        }
    }

    /// 只带图、没有文字时不能凭空多出一个空的 text 块。
    #[test]
    fn image_only_message_has_no_empty_text_block() {
        let msgs = vec![Msg::new(Role::Tool, "").with_image("data:image/png;base64,AAA=")];
        let body = wire_json(Thinking::Model, Vec::new(), &msgs);
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1, "只该有图块：{body}");
        assert_eq!(parts[0]["type"], "image_url");
    }

    /// 工具结果也能带图（截屏 / 看图之后把图交给模型的那条路）。
    #[test]
    fn tool_result_can_carry_an_image() {
        let msgs =
            vec![Msg::tool_result("call_1", r#"{"ok":true}"#)
                .with_image("data:image/png;base64,AAA=")];
        let body = wire_json(Thinking::Model, Vec::new(), &msgs);
        let msg = &body["messages"][0];
        assert_eq!(msg["role"], "tool");
        assert_eq!(msg["tool_call_id"], "call_1");
        assert_eq!(msg["content"][1]["type"], "image_url");
    }

    /// 空 data URL 不该被塞进请求（多发一个空字段只会让服务端困惑）。
    #[test]
    fn empty_image_url_is_ignored() {
        let msgs = vec![Msg::new(Role::User, "hi").with_image("")];
        let body = wire_json(Thinking::Model, Vec::new(), &msgs);
        assert!(body["messages"][0]["content"].is_string(), "{body}");
    }

    // ---- 思考模式 ----

    /// 组装一次请求体，返回 JSON（测试用）。
    fn wire_json(
        thinking: Thinking,
        tools: Vec<serde_json::Value>,
        msgs: &[Msg],
    ) -> serde_json::Value {
        let cfg = Config {
            thinking,
            ..Config::deepseek("k")
        };
        serde_json::to_value(build_wire(&cfg, msgs, tools)).unwrap()
    }

    /// 默认档「跟随模型」**不发送任何思考字段** —— 与加这个功能之前的行为一致。
    #[test]
    fn thinking_model_sends_no_control_fields() {
        let body = wire_json(Thinking::Model, Vec::new(), &[Msg::new(Role::User, "hi")]);
        assert!(body.get("thinking").is_none(), "不该发 thinking：{body}");
        assert!(
            body.get("reasoning_effort").is_none(),
            "不该发 effort：{body}"
        );
    }

    /// 关闭思考只发开关、不发强度（发了也是同一个效果）。
    #[test]
    fn thinking_off_sends_only_the_toggle() {
        let body = wire_json(Thinking::Off, Vec::new(), &[Msg::new(Role::User, "hi")]);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());
    }

    /// 三档强度：开关开 + 强度值分别是 low/high/max。
    #[test]
    fn thinking_levels_map_to_low_high_max() {
        for (level, want) in [
            (Thinking::Low, "low"),
            (Thinking::High, "high"),
            (Thinking::Max, "max"),
        ] {
            let body = wire_json(level, Vec::new(), &[Msg::new(Role::User, "hi")]);
            assert_eq!(body["thinking"]["type"], "enabled", "{level:?}");
            assert_eq!(body["reasoning_effort"], want, "{level:?}");
        }
    }

    /// **服务端视角**：带 `tools` 时，历史推理必须原样回传，否则 400。
    #[test]
    fn reasoning_is_passed_back_when_tools_are_present() {
        #[derive(serde::Deserialize)]
        struct ServerMsg {
            role: String,
            #[serde(default)]
            reasoning_content: Option<String>,
        }

        let msgs = vec![
            Msg::new(Role::User, "看一下 a.txt"),
            Msg::assistant_with_tools("我查一下", Vec::new()).with_reasoning("用户要看 a.txt"),
        ];

        // 带 tools → 回传
        let with_tools = wire_json(
            Thinking::High,
            vec![serde_json::json!({"type": "function"})],
            &msgs,
        );
        let parsed: Vec<ServerMsg> =
            serde_json::from_value(with_tools["messages"].clone()).expect("messages 形状");
        let assistant = parsed.iter().find(|m| m.role == "assistant").unwrap();
        assert_eq!(
            assistant.reasoning_content.as_deref(),
            Some("用户要看 a.txt")
        );

        // 不带 tools → 服务端会忽略，我们干脆不发
        let no_tools = wire_json(Thinking::High, Vec::new(), &msgs);
        let parsed: Vec<ServerMsg> =
            serde_json::from_value(no_tools["messages"].clone()).expect("messages 形状");
        let assistant = parsed.iter().find(|m| m.role == "assistant").unwrap();
        assert!(assistant.reasoning_content.is_none());
    }

    /// 明确关闭思考时，历史推理也不再回传（上下文不该自相矛盾）。
    #[test]
    fn reasoning_is_dropped_when_thinking_is_off() {
        let msgs = vec![
            Msg::new(Role::User, "a"),
            Msg::new(Role::Assistant, "b").with_reasoning("想过"),
        ];
        let body = wire_json(
            Thinking::Off,
            vec![serde_json::json!({"type": "function"})],
            &msgs,
        );
        let raw = body.to_string();
        assert!(
            !raw.contains("reasoning_content"),
            "关闭思考后不该回传：{raw}"
        );
    }

    /// 空推理不该发出去（`reasoning_content: ""` 是没意义的字段）。
    #[test]
    fn empty_reasoning_is_never_sent() {
        let msgs = vec![Msg::new(Role::Assistant, "b").with_reasoning("")];
        let body = wire_json(
            Thinking::High,
            vec![serde_json::json!({"type": "function"})],
            &msgs,
        );
        assert!(!body.to_string().contains("reasoning_content"));
    }

    /// 加了思考字段之后，`tools` 仍然是**扁平数组** —— 别把上次那个 422 弄回来。
    #[test]
    fn thinking_does_not_change_tools_shape() {
        #[derive(serde::Deserialize)]
        struct ServerTool {
            #[serde(rename = "type")]
            #[allow(dead_code)]
            kind: String,
            #[allow(dead_code)]
            function: serde_json::Value,
        }
        let body = wire_json(
            Thinking::Max,
            neo_tools::tool_declarations(),
            &[Msg::new(Role::User, "hi")],
        );
        let tools: Vec<ServerTool> =
            serde_json::from_value(body["tools"].clone()).expect("tools 必须是扁平函数数组");
        assert!(!tools.is_empty());
        assert_eq!(body["reasoning_effort"], "max");
    }

    /// 档位的界面名 ↔ 落库键 必须一一对应，不能有重复键。
    #[test]
    fn thinking_keys_round_trip_and_are_unique() {
        let mut keys: Vec<&str> = Thinking::ALL.iter().map(|t| t.key()).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "档位键重复了");
        for t in Thinking::ALL {
            assert_eq!(Thinking::from_key(t.key()), t, "{t:?} 往返失败");
        }
        assert_eq!(Thinking::from_key("没见过的值"), Thinking::Model);
    }

    /// 把当时那个错误写法**钉在测试里**：嵌套数组服务端解析不出来。
    ///
    /// 这条不是为了测 `build_wire`，而是为了让"为什么不能多包一层"这件事
    /// 有一个可执行的解释 —— 后人想改回去时会先看到它失败。
    #[test]
    fn nested_tools_shape_is_rejected() {
        #[derive(serde::Deserialize)]
        struct ServerTool {
            #[serde(rename = "type")]
            #[allow(dead_code)]
            kind: String,
            #[allow(dead_code)]
            function: serde_json::Value,
        }

        // 出事的形状：tools = [[{…}]]（`openai_tools()` 本身已是数组，又套了一层）
        let nested = vec![neo_tools::openai_tools()];
        let body = serde_json::to_value(build_wire(
            &Config::deepseek("k"),
            &[Msg::new(Role::User, "hi")],
            nested,
        ))
        .unwrap();
        assert!(
            serde_json::from_value::<Vec<ServerTool>>(body["tools"].clone()).is_err(),
            "嵌套数组必须解析失败 —— 这正是线上那条 422"
        );
    }

    /// 工具对话的消息形状（服务端视角）。
    ///
    /// 两个最容易写错、且一写错就 422 的点，这里都用**严格结构体**钉住：
    /// 1. `function.arguments` 必须是**字符串**（不是 JSON 对象）；
    /// 2. `role=tool` 必须带 `tool_call_id`，且能对上前面 assistant 的调用 id。
    #[test]
    fn wire_tool_conversation_matches_server_shape() {
        #[derive(serde::Deserialize)]
        struct ServerCallFn {
            name: String,
            /// 注意类型是 String —— 服务端期待字符串化的 JSON
            arguments: String,
        }
        #[derive(serde::Deserialize)]
        struct ServerCall {
            id: String,
            #[serde(rename = "type")]
            #[allow(dead_code)]
            kind: String,
            function: ServerCallFn,
        }
        #[derive(serde::Deserialize)]
        struct ServerMsg {
            role: String,
            #[allow(dead_code)]
            content: String,
            #[serde(default)]
            tool_calls: Vec<ServerCall>,
            #[serde(default)]
            tool_call_id: Option<String>,
        }

        let msgs = vec![
            Msg::new(Role::User, "读一下 a.txt"),
            Msg::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                }],
            ),
            Msg::tool_result("call_1", r#"{"ok":true}"#),
        ];
        let body = serde_json::to_value(build_wire(
            &Config::deepseek("k"),
            &msgs,
            neo_tools::tool_declarations(),
        ))
        .unwrap();

        let parsed: Vec<ServerMsg> =
            serde_json::from_value(body["messages"].clone()).expect("消息形状不符合服务端期待");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[1].role, "assistant");
        assert_eq!(parsed[1].tool_calls.len(), 1);
        assert_eq!(parsed[1].tool_calls[0].id, "call_1");
        assert_eq!(parsed[1].tool_calls[0].kind, "function");
        assert_eq!(parsed[1].tool_calls[0].function.name, "read_file");
        // 参数是字符串，且能再解析成对象
        let args: serde_json::Value =
            serde_json::from_str(&parsed[1].tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["path"], "a.txt");

        assert_eq!(parsed[2].role, "tool");
        assert_eq!(parsed[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            parsed[1].tool_calls[0].id,
            parsed[2].tool_call_id.clone().unwrap()
        );
        // 普通消息不该凭空长出 tool 字段
        assert!(parsed[0].tool_calls.is_empty());
        assert!(parsed[0].tool_call_id.is_none());
    }

    /// 没有工具时，`tools` 字段必须整个不出现（不支持 function calling 的服务端
    /// 不会因为一个空数组报错）。
    #[test]
    fn wire_omits_tools_when_empty() {
        let cfg = Config::deepseek("sk-x");
        let body =
            serde_json::to_value(build_wire(&cfg, &[Msg::new(Role::User, "hi")], vec![])).unwrap();
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn config_gate() {
        let mut cfg = Config::deepseek("");
        assert!(!cfg.is_configured());
        cfg.api_key = "sk-x".to_owned();
        assert!(cfg.is_configured());
    }
}

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
    // 缺 id 的条目跳过、重复项去掉、结果排序
    let body = r#"{"data":[{"id":"b"},{"object":"model"},{"id":"a"},{"id":"b"}]}"#;
    assert_eq!(parse_models(body).unwrap(), vec!["a", "b"]);
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
fn list_models_needs_configuration() {
    let cfg = Config::deepseek("");
    assert!(list_models(&cfg).is_err());
}
