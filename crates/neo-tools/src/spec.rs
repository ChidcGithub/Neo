//! 工具描述与 JSON Schema 生成。
//!
//! 参数只在 [`Param`] 里声明一次：schema 由 [`schema_of`] 生成，
//! 取值与默认值由 [`crate::Args`] 读取。**声明即文档、即校验、即 schema**，
//! 三处不可能漂移。

use serde_json::{json, Value};

/// 参数类型。只保留模型函数调用真正用得到的三种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    String,
    Integer,
    Boolean,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::String => "string",
            Ty::Integer => "integer",
            Ty::Boolean => "boolean",
        }
    }
}

/// 可选参数的默认值。写成枚举（而不是 `serde_json::Value`）是为了让
/// [`Param`] 能保持 `const`，工具表因此可以是一个静态数组。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Default {
    Str(&'static str),
    Int(i64),
    Bool(bool),
}

impl Default {
    pub(crate) fn to_json(self) -> Value {
        match self {
            Default::Str(s) => json!(s),
            Default::Int(i) => json!(i),
            Default::Bool(b) => json!(b),
        }
    }
}

/// 一个参数的完整声明。
#[derive(Clone, Copy, Debug)]
pub struct Param {
    /// 参数名（`snake_case`；路径一律叫 `path`，内容一律叫 `content`）。
    pub name: &'static str,
    pub ty: Ty,
    /// 是否必填。可选参数必须有 [`Param::default`]。
    pub required: bool,
    /// 给模型看的说明：写"什么时候该给什么"，不写"它是什么"。
    pub desc: &'static str,
    pub default: Option<Default>,
    /// 整数参数的闭区间上下限。
    pub range: Option<(i64, i64)>,
}

impl Param {
    /// 必填字符串。
    pub const fn text(name: &'static str, desc: &'static str) -> Self {
        Self {
            name,
            ty: Ty::String,
            required: true,
            desc,
            default: None,
            range: None,
        }
    }

    /// 可选字符串，省略时为空串。
    pub const fn opt_text(name: &'static str, desc: &'static str) -> Self {
        Self {
            name,
            ty: Ty::String,
            required: false,
            desc,
            default: Some(Default::Str("")),
            range: None,
        }
    }

    /// 可选整数，`[lo, hi]` 闭区间，省略时用 `by_default`。
    /// 必填整数（带闭区间）。
    ///
    /// 坐标这类"没有合理默认值"的参数用它 —— 给个默认值反而危险：
    /// 模型漏填时会被悄悄点到 (0,0)。
    pub const fn int(name: &'static str, desc: &'static str, min: i64, max: i64) -> Self {
        Self {
            name,
            desc,
            required: true,
            ty: Ty::Integer,
            default: None,
            range: Some((min, max)),
        }
    }

    pub const fn opt_int(
        name: &'static str,
        desc: &'static str,
        by_default: i64,
        lo: i64,
        hi: i64,
    ) -> Self {
        Self {
            name,
            ty: Ty::Integer,
            required: false,
            desc,
            default: Some(Default::Int(by_default)),
            range: Some((lo, hi)),
        }
    }

    /// 可选布尔，默认 `false`；语义统一为"更危险/更主动的那一侧"。
    pub const fn flag(name: &'static str, desc: &'static str) -> Self {
        Self {
            name,
            ty: Ty::Boolean,
            required: false,
            desc,
            default: Some(Default::Bool(false)),
            range: None,
        }
    }
}

/// 由参数声明生成 JSON Schema。
pub fn schema_of(params: &[Param]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        let mut spec = json!({ "type": p.ty.name(), "description": p.desc });
        if let Some((lo, hi)) = p.range {
            spec["minimum"] = json!(lo);
            spec["maximum"] = json!(hi);
        }
        if let Some(d) = p.default {
            spec["default"] = d.to_json();
        }
        props.insert(p.name.to_owned(), spec);
        if p.required {
            required.push(json!(p.name));
        }
    }
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
        // 明确禁止额外字段：模型编参数时立刻拿到错误，而不是被静默忽略。
        "additionalProperties": false,
    })
}

/// 一个可调用工具。
pub struct Tool {
    /// `snake_case` 工具名（与模型可见的名字一致）。
    pub name: &'static str,
    /// 中文短名，给 UI 卡片做标题。
    pub title: &'static str,
    /// 一句话用途（同时进 schema description 与系统提示词）。
    pub purpose: &'static str,
    pub risk: crate::policy::Risk,
    /// 参数声明 —— schema 与取值校验的共同来源。
    pub params: &'static [Param],
    /// 执行前的一句话预览（权限确认框与 UI 卡片共用）。
    ///
    /// 与执行后的 [`crate::Outcome::summary`] 是**两个时刻**的同一件事：
    /// 预览写"将要做什么"，摘要写"已经做了什么"。用户点头前看到的就是它，
    /// 所以必须把关键参数（路径、命令、命中次数）写进来，不能只写工具名。
    pub preview: fn(&crate::Args) -> String,
    /// 执行体。`Args` 已保证类型正确；仍可能越界、不存在、不唯一。
    pub run: fn(&crate::Scope, &crate::Args) -> crate::Outcome,
}

impl Tool {
    pub fn schema(&self) -> Value {
        schema_of(self.params)
    }

    /// OpenAI 函数调用格式的一条工具声明。
    pub fn openai(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.purpose,
                "parameters": self.schema(),
            }
        })
    }
}

/// 工具注册表 —— 顺序即推荐顺序（先读、再写、最后执行）。
pub fn registry() -> &'static [Tool] {
    crate::tools::REGISTRY
}

/// 按名字找工具。
pub fn find(name: &str) -> Option<&'static Tool> {
    registry().iter().find(|t| t.name == name)
}

/// 每一条工具声明（对应协议里 `tools` 数组的**一个元素**）。
///
/// ⚠️ 用这个，不要用 [`openai_tools`] 再套一层 —— `tools` 是**扁平的函数数组**，
/// 多包一层数组会让服务端在反序列化时直接 422
/// （`tools[0][0].function: invalid type: map, expected unit`）。
pub fn tool_declarations() -> Vec<Value> {
    registry().iter().map(Tool::openai).collect()
}

/// 组装 `/chat/completions` 的 `tools` 参数（已经是完整的数组）。
///
/// 只有"要的就是这个数组本身"时才用它；要塞进 [`crate::Config`] 之类的字段，
/// 请用 [`tool_declarations`]。
pub fn openai_tools() -> Value {
    Value::Array(tool_declarations())
}

/// 全部工具的一句话清单（拼进系统提示词）。
pub fn prompt_catalog() -> String {
    registry()
        .iter()
        .map(|t| format!("- `{}`（{}）：{}", t.name, t.title, t.purpose))
        .collect::<Vec<_>>()
        .join("\n")
}
