//! 参数取值、错误分类与统一返回结构。

use serde_json::{json, Value};

use crate::spec::{Default, Param, Tool, Ty};

/// 错误分类。**类别决定上层怎么反应**，`message` 只负责说人话：
/// `NotAllowed` 该弹权限提示，`NotFound` 该让模型重新探路，
/// `BadArguments` 是模型自己的锅（把可选值一起回给它就能自己改对）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// 参数缺失、类型不对、越界。
    BadArguments,
    /// 目标不存在。
    NotFound,
    /// 工作区外 / 只读模式 / 策略拒绝。
    NotAllowed,
    /// 内容或输出超过体积上限。
    TooLarge,
    /// `old_string` 命中多处，无法确定改哪里。
    NotUnique,
    /// 已存在且未允许覆盖。
    Conflict,
    /// 编码不是 UTF-8 等不支持的情况。
    Unsupported,
    /// 超时（仅 `bash`）。
    Timeout,
    /// 其它 I/O 错误。
    Io,
    /// 工具自身实现出了问题（不该出现）。
    Internal,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::BadArguments => "bad_arguments",
            ErrorKind::NotFound => "not_found",
            ErrorKind::NotAllowed => "not_allowed",
            ErrorKind::TooLarge => "too_large",
            ErrorKind::NotUnique => "not_unique",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Io => "io",
            ErrorKind::Internal => "internal",
        }
    }
}

/// 一次失败的结构化说明。
#[derive(Clone, Debug)]
pub struct ToolError {
    pub kind: ErrorKind,
    /// 发生了什么（面向模型与用户，一句话）。
    pub message: String,
    /// 下一步该怎么办。**必须可执行**，不写"请重试"这类废话。
    pub hint: Option<String>,
}

impl ToolError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn bad_args(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadArguments, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotAllowed, message)
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, message)
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({ "kind": self.kind.as_str(), "message": self.message });
        if let Some(h) = &self.hint {
            v["hint"] = json!(h);
        }
        v
    }
}

/// 工具执行结果。成功与失败**同一个外壳**，靠 `ok` 区分。
#[derive(Clone, Debug)]
pub struct Outcome {
    /// 工具名；工具不存在时为 `"unknown"`。
    pub tool: &'static str,
    /// 一行摘要，给人看（UI 卡片）。
    pub summary: String,
    /// 结构化结果，给模型看。
    pub data: Value,
    /// 失败原因；`None` 表示成功。
    pub error: Option<ToolError>,
    /// 随结果一起回灌给模型的图片（data URL）。
    ///
    /// **多模态模型会真的"看到"它们** —— 截图、看图这类工具靠这条路把图交给模型，
    /// 而不是把 base64 塞进 JSON 字符串里（那样既占爆上下文、模型也根本"看不见"）。
    /// 非多模态模型只会拿到 `data` 里的文字，不会报错。
    pub images: Vec<String>,
}

impl Outcome {
    pub fn ok(tool: &'static str, summary: impl Into<String>, data: Value) -> Self {
        Self {
            tool,
            summary: summary.into(),
            data,
            error: None,
            images: Vec::new(),
        }
    }

    /// 附上一张要给模型看的图（data URL）。
    pub fn with_image(mut self, data_url: impl Into<String>) -> Self {
        let url = data_url.into();
        if !url.is_empty() {
            self.images.push(url);
        }
        self
    }

    pub fn fail(tool: &'static str, err: ToolError) -> Self {
        Self {
            tool,
            summary: format!("{}：{}", err.kind.as_str(), err.message),
            data: Value::Null,
            error: Some(err),
            images: Vec::new(),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// 回给模型的内容：紧凑 JSON。
    ///
    /// `limit` 是**字符**上限 —— 超长输出（命令回显、整文件内容）在这里被截断
    /// 并打上标记，避免一次吃光上下文窗口。截断只影响回灌给模型的副本，
    /// [`Outcome::data`] 保持完整，UI 仍能展示全量。
    pub fn to_model_json(&self, limit: usize) -> String {
        let body = if let Some(e) = &self.error {
            json!({ "ok": false, "tool": self.tool, "error": e.to_json() })
        } else {
            json!({ "ok": true, "tool": self.tool, "summary": self.summary, "data": self.data })
        };
        let text = serde_json::to_string(&body).unwrap_or_else(|_| "{\"ok\":false}".to_owned());
        if text.chars().count() <= limit {
            return text;
        }
        // 截断后仍必须是合法 JSON —— 回灌的是协议消息，不能是坏 JSON。
        let head: String = text.chars().take(limit).collect();
        json!({
            "ok": self.is_ok(),
            "tool": self.tool,
            "truncated": true,
            "summary": self.summary,
            "note": format!("结果过长，已截断到 {limit} 字符；完整内容见界面卡片，或缩小范围重试"),
            "head": head,
        })
        .to_string()
    }
}

/// 工具参数的取值包装：**唯一**的读参入口。
///
/// 所有类型/范围错误都被规范化成 [`ErrorKind::BadArguments`]，并附上"该怎么给"，
/// 让模型能自己改对，而不是靠猜。参数声明来自工具本身，不会张冠李戴。
pub struct Args<'a> {
    params: &'static [Param],
    value: &'a Value,
}

impl<'a> Args<'a> {
    pub fn new(tool: &'static Tool, value: &'a Value) -> Self {
        Self {
            params: tool.params,
            value,
        }
    }

    /// 参数**是否出现过**（显式给 `null` 算没给）。
    ///
    /// 用来区分"没给"与"给了默认值"—— `screenshot` 的局部区域要靠它判断
    /// "四个参数是都给全了还是都没给"，不能看值（0 也是合法坐标）。
    pub fn has(&self, name: &str) -> bool {
        !matches!(self.value.get(name), None | Some(Value::Null))
    }

    /// 必填字符串；缺失或空白都算参数错误。
    pub fn require_str(&self, name: &str) -> Result<String, ToolError> {
        match self.value.get(name).and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => Ok(s.to_owned()),
            Some(_) => Err(self.bad(name, "不能是空字符串")),
            None => Err(self.missing(name)),
        }
    }

    /// 必填但**允许空串**：写文件的内容、替换后的新文本都可能合法地为空。
    /// 与 [`Args::require_str`] 的区别只是"空串算不算没给"。
    pub fn require_present_str(&self, name: &str) -> Result<String, ToolError> {
        match self.value.get(name) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(self.bad(name, "应为字符串")),
            None => Err(self.missing(name)),
        }
    }

    /// 可选字符串；缺失时用声明里的默认值（通常是空串）。
    pub fn opt_str(&self, name: &str) -> Result<String, ToolError> {
        match self.value.get(name) {
            None | Some(Value::Null) => Ok(self.default_str(name)),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => Err(self.bad(name, "应为字符串")),
        }
    }

    /// 可选布尔。
    pub fn flag(&self, name: &str) -> Result<bool, ToolError> {
        match self.value.get(name) {
            None | Some(Value::Null) => Ok(matches!(
                self.declared(name).and_then(|p| p.default),
                Some(Default::Bool(true))
            )),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(self.bad(name, "应为布尔值 true/false")),
        }
    }

    /// 必填整数（与 [`Args::require_str`] 对称）。
    ///
    /// 坐标这类参数必须走它：`opt_int` 对"必填但没给"只会给出 0，
    /// 于是一次漏填的点击就被悄悄落到 (0,0) 上了。
    pub fn require_int(&self, name: &str) -> Result<i64, ToolError> {
        if self.value.get(name).is_none() {
            return Err(self.missing(name));
        }
        self.opt_int(name)
    }

    /// 可选整数，按声明里的闭区间校验（越界即报错，不静默夹取）。
    pub fn opt_int(&self, name: &str) -> Result<i64, ToolError> {
        let raw = match self.value.get(name) {
            None | Some(Value::Null) => match self.declared(name).and_then(|p| p.default) {
                Some(Default::Int(i)) => i,
                // 必填却没有默认值 → 缺失就是错误，不能当成 0。
                _ if self.declared(name).is_some_and(|p| p.required) => {
                    return Err(self.missing(name))
                }
                _ => 0,
            },
            Some(v) => v.as_i64().ok_or_else(|| self.bad(name, "应为整数"))?,
        };
        if let Some((lo, hi)) = self.declared(name).and_then(|p| p.range) {
            if raw < lo || raw > hi {
                return Err(self.bad(name, format!("需在 [{lo}, {hi}] 之间，收到 {raw}")));
            }
        }
        Ok(raw)
    }

    /// 表里没有的键一律拒绝 —— schema 里已声明 `additionalProperties: false`，
    /// 这里做同等检查，免得手写调用绕过 schema。
    pub fn reject_unknown(&self) -> Result<(), ToolError> {
        if let Some(obj) = self.value.as_object() {
            for key in obj.keys() {
                if !self.params.iter().any(|p| p.name == key) {
                    let known: Vec<&str> = self.params.iter().map(|p| p.name).collect();
                    return Err(ToolError::bad_args(format!("未知参数 `{key}`"))
                        .with_hint(format!("本工具只接受：{}", known.join(", "))));
                }
            }
        }
        Ok(())
    }

    fn declared(&self, name: &str) -> Option<&'static Param> {
        self.params.iter().find(|p| p.name == name)
    }

    fn default_str(&self, name: &str) -> String {
        match self.declared(name).and_then(|p| p.default) {
            Some(Default::Str(s)) => s.to_owned(),
            _ => String::new(),
        }
    }

    fn bad(&self, name: &str, why: impl Into<String>) -> ToolError {
        let why = why.into();
        let mut err = ToolError::bad_args(format!("参数 `{name}` {why}"));
        if let Some(p) = self.declared(name) {
            let allowed = match p.ty {
                Ty::String => "字符串".to_owned(),
                Ty::Integer => p
                    .range
                    .map(|(lo, hi)| format!("整数（{lo}…{hi}）"))
                    .unwrap_or_else(|| "整数".to_owned()),
                Ty::Boolean => "布尔值".to_owned(),
            };
            let dflt = p
                .default
                .map(|d| format!("，省略时默认 {}", d.to_json()))
                .unwrap_or_default();
            err = err.with_hint(format!("`{name}` 应为{allowed}{dflt}"));
        }
        err
    }

    fn missing(&self, name: &str) -> ToolError {
        let hint = match self.declared(name) {
            Some(p) if p.required => format!("`{name}` 是必填参数：{}", p.desc),
            Some(p) => format!(
                "`{name}` 可省略，默认 {}",
                p.default
                    .map(|d| d.to_json().to_string())
                    .unwrap_or_else(|| "空".to_owned())
            ),
            None => "检查参数名拼写".to_owned(),
        };
        ToolError::new(ErrorKind::BadArguments, format!("缺少必填参数 `{name}`")).with_hint(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome_ok() -> Outcome {
        Outcome::ok("read_file", "读取 a.rs", json!({ "lines": 3 }))
    }

    #[test]
    fn model_json_shape_on_success() {
        let v: Value = serde_json::from_str(&outcome_ok().to_model_json(4096)).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["tool"], "read_file");
        assert_eq!(v["data"]["lines"], 3);
    }

    #[test]
    fn model_json_shape_on_failure() {
        let out = Outcome::fail("bash", ToolError::not_allowed("越界").with_hint("换个路径"));
        let v: Value = serde_json::from_str(&out.to_model_json(4096)).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["kind"], "not_allowed");
        assert_eq!(v["error"]["hint"], "换个路径");
    }

    #[test]
    fn truncation_stays_valid_json() {
        let out = Outcome::ok("bash", "输出很长", json!({ "stdout": "汉".repeat(500) }));
        let v: Value = serde_json::from_str(&out.to_model_json(200)).expect("截断后仍是合法 JSON");
        assert_eq!(v["truncated"], true);
        assert!(v["head"].as_str().unwrap().chars().count() <= 200);
    }

    #[test]
    fn args_report_range_and_type() {
        let tool = crate::spec::find("read_file").unwrap();

        let v = json!({ "path": "a.txt", "limit": 99999 });
        let err = Args::new(tool, &v).opt_int("limit").unwrap_err();
        assert_eq!(err.kind, ErrorKind::BadArguments);
        assert!(err.hint.unwrap().contains("2000"));

        let v = json!({ "path": "a.txt", "limit": "x" });
        assert_eq!(
            Args::new(tool, &v).opt_int("limit").unwrap_err().kind,
            ErrorKind::BadArguments
        );

        let v = json!({});
        assert_eq!(
            Args::new(tool, &v).require_str("path").unwrap_err().kind,
            ErrorKind::BadArguments
        );

        let v = json!({ "path": "a.txt", "nope": 1 });
        assert_eq!(
            Args::new(tool, &v).reject_unknown().unwrap_err().kind,
            ErrorKind::BadArguments
        );
    }

    #[test]
    fn defaults_come_from_declaration() {
        let tool = crate::spec::find("read_file").unwrap();
        let v = json!({ "path": "a.txt" });
        let args = Args::new(tool, &v);
        assert_eq!(args.opt_int("limit").unwrap(), 400);
        assert_eq!(args.opt_int("offset").unwrap(), 0);
    }

    #[test]
    fn empty_string_is_allowed_only_where_declared() {
        let tool = crate::spec::find("edit_file").unwrap();
        let v = json!({ "path": "a.txt", "old_string": "x", "new_string": "" });
        let args = Args::new(tool, &v);
        // 删除一段文本 → new_string 合法为空
        assert_eq!(args.require_present_str("new_string").unwrap(), "");
        // old_string 为空则无意义 → 必须报错
        let v2 = json!({ "path": "a.txt", "old_string": "", "new_string": "y" });
        assert_eq!(
            Args::new(tool, &v2)
                .require_str("old_string")
                .unwrap_err()
                .kind,
            ErrorKind::BadArguments
        );
    }
}
