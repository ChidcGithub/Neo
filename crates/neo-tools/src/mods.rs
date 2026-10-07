//! Out-of-process MOD contracts with explicit read-only discovery and an inert
//! session state machine. No process execution, sandbox or approval grants.

pub mod discovery;
pub mod session;
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
};

pub const API_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: usize = 32_768;
pub const MAX_MESSAGE_BYTES: usize = 65_536;
pub const MAX_RESULT_BYTES: usize = 32_768;
pub const MAX_PARAMS_BYTES: usize = 16_384;
pub const MAX_STRING_BYTES: usize = 4_096;
pub const MAX_TOOLS: usize = 16;
pub const MAX_CATALOG: usize = 128;

/// Static diagnostics deliberately exclude untrusted input and parser traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Check<T = ()> = Result<T, Error>;
fn ensure(ok: bool, reason: &'static str) -> Check {
    if ok {
        Ok(())
    } else {
        Err(Error(reason))
    }
}

// serde_json::Value normally accepts duplicate keys. Reject them at every object level.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Json;
        impl<'de> Visitor<'de> for Json {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(Unique(item)) = a.next_element()? {
                    v.push(item);
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = serde_json::Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if v.contains_key(&key) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Unique(item) = a.next_value()?;
                    v.insert(key, item);
                }
                Ok(Unique(Value::Object(v)))
            }
        }
        d.deserialize_any(Json)
    }
}
fn parse<T: de::DeserializeOwned>(bytes: &[u8], cap: usize) -> Check<T> {
    ensure(!bytes.is_empty() && bytes.len() <= cap, "JSON byte limit")?;
    let Unique(value) = serde_json::from_slice(bytes).map_err(|_| Error("invalid JSON"))?;
    serde_json::from_value(value).map_err(|_| Error("invalid contract fields"))
}
/// Bounded serialization also covers values constructed by SDK callers, not just parsed bytes.
fn encode<T: Serialize>(value: &T, cap: usize) -> Check<Vec<u8>> {
    struct Buffer(Vec<u8>, usize);
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.1.saturating_sub(self.0.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "byte limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer(Vec::new(), cap);
    serde_json::to_writer(&mut buffer, value).map_err(|_| Error("JSON byte limit"))?;
    Ok(buffer.0)
}
fn token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}
fn valid_id(id: &str) -> bool {
    id.len() <= 32 && id.contains('.') && id.split('.').all(|part| token(part, 16))
}
/// Injective mapping: dots become underscores; source tokens cannot contain underscores.
pub fn tool_name(mod_id: &str, local_name: &str) -> Check<String> {
    ensure(
        valid_id(mod_id) && token(local_name, 24),
        "invalid MOD/tool identifier",
    )?;
    let name = format!("mod_{}__{}", mod_id.replace('.', "_"), local_name);
    ensure(
        name.len() <= 64 && crate::find(&name).is_none(),
        "reserved tool name",
    )?;
    Ok(name)
}
fn executable(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && path.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit());
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with('.')
                && !device
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
}

/// Untrusted declarations, NOT granted permissions or confinement.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum Capability {
    WorkspaceRead,
    WorkspaceWrite,
    Network,
    DesktopRead,
    DesktopInput,
    ProcessSpawn,
}
impl TryFrom<String> for Capability {
    type Error = Error;
    fn try_from(value: String) -> Check<Self> {
        match value.as_str() {
            "workspace_read" => Ok(Self::WorkspaceRead),
            "workspace_write" => Ok(Self::WorkspaceWrite),
            "network" => Ok(Self::Network),
            "desktop_read" => Ok(Self::DesktopRead),
            "desktop_input" => Ok(Self::DesktopInput),
            "process_spawn" => Ok(Self::ProcessSpawn),
            _ => Err(Error("unknown capability")),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub timeout_ms: u32,
    pub memory_mib: u32,
    pub max_in_flight: u32,
    pub max_message_bytes: usize,
    pub max_result_bytes: usize,
}
impl Limits {
    pub fn validate(&self) -> Check {
        ensure(
            (1..=60_000).contains(&self.timeout_ms)
                && (1..=256).contains(&self.memory_mib)
                && (1..=4).contains(&self.max_in_flight)
                && (1..=MAX_MESSAGE_BYTES).contains(&self.max_message_bytes)
                && (1..=MAX_RESULT_BYTES).contains(&self.max_result_bytes)
                && self.max_result_bytes <= self.max_message_bytes,
            "invalid resource limits",
        )
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Scalar {
    String {},
    Integer {},
    Number {},
    Boolean {},
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum ObjectType {
    Object,
}
impl TryFrom<String> for ObjectType {
    type Error = Error;
    fn try_from(value: String) -> Check<Self> {
        match value.as_str() {
            "object" => Ok(Self::Object),
            _ => Err(Error("unknown schema type")),
        }
    }
}
/// JSON Schema subset: flat object, scalar properties, explicit required, no extra properties.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Schema {
    #[serde(rename = "type")]
    pub kind: ObjectType,
    pub properties: BTreeMap<String, Scalar>,
    pub required: Vec<String>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: bool,
}
impl Schema {
    pub fn validate(&self) -> Check {
        let unique: BTreeSet<_> = self.required.iter().collect();
        ensure(
            !self.additional_properties
                && self.properties.len() <= 32
                && self.properties.keys().all(|k| token(k, 32))
                && unique.len() == self.required.len()
                && self
                    .required
                    .iter()
                    .all(|k| self.properties.contains_key(k)),
            "invalid schema",
        )
    }
    pub fn validate_value(&self, value: &Value) -> Check {
        self.validate()?;
        let object = value.as_object().ok_or(Error("expected object"))?;
        ensure(
            self.required.iter().all(|k| object.contains_key(k)),
            "missing required parameter",
        )?;
        for (key, value) in object {
            let ty = self.properties.get(key).ok_or(Error("unknown parameter"))?;
            let valid = match ty {
                Scalar::String {} => value.as_str().is_some_and(|s| s.len() <= MAX_STRING_BYTES),
                Scalar::Integer {} => value.is_i64() || value.is_u64(),
                Scalar::Number {} => value.is_number(),
                Scalar::Boolean {} => value.is_boolean(),
            };
            ensure(valid, "invalid scalar value")?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub input_schema: Schema,
    pub output_schema: Schema,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    /// Positive monotonic release integer, not SemVer and not the API version.
    pub revision: u32,
    pub executable: String,
    pub capabilities: Vec<Capability>,
    pub limits: Limits,
    pub tools: Vec<Tool>,
}
#[derive(Clone, Debug)]
pub struct ValidatedManifest {
    manifest: Manifest,
}
impl Manifest {
    pub fn validate(self) -> Check<ValidatedManifest> {
        encode(&self, MAX_MANIFEST_BYTES)?;
        ensure(
            self.api_version == API_VERSION && self.revision > 0,
            "unsupported version",
        )?;
        ensure(
            valid_id(&self.id) && executable(&self.executable),
            "invalid identity or executable",
        )?;
        self.limits.validate()?;
        let caps: BTreeSet<_> = self.capabilities.iter().collect();
        ensure(
            caps.len() == self.capabilities.len(),
            "duplicate capability",
        )?;
        ensure(
            !self.tools.is_empty() && self.tools.len() <= MAX_TOOLS,
            "tool count limit",
        )?;
        let mut names = BTreeSet::new();
        for tool in &self.tools {
            ensure(
                names.insert(tool_name(&self.id, &tool.name)?),
                "duplicate tool name",
            )?;
            ensure(
                !tool.description.trim().is_empty()
                    && tool.description.len() <= 512
                    && !tool.description.chars().any(char::is_control),
                "invalid description",
            )?;
            tool.input_schema.validate()?;
            tool.output_schema.validate()?;
        }
        Ok(ValidatedManifest { manifest: self })
    }
}
impl ValidatedManifest {
    pub fn parse(bytes: &[u8]) -> Check<Self> {
        parse::<Manifest>(bytes, MAX_MANIFEST_BYTES)?.validate()
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn tool(&self, name: &str) -> Check<&Tool> {
        self.manifest
            .tools
            .iter()
            .find(|t| tool_name(&self.manifest.id, &t.name).as_deref() == Ok(name))
            .ok_or(Error("unknown MOD tool"))
    }
}

/// An inert catalog. There is deliberately no enable/execute/auto-approve API.
#[derive(Default)]
pub struct Catalog {
    entries: BTreeMap<String, CatalogEntry>,
    names: BTreeSet<String>,
}
pub struct CatalogEntry {
    manifest: ValidatedManifest,
}
impl CatalogEntry {
    pub fn manifest(&self) -> &ValidatedManifest {
        &self.manifest
    }
    pub fn enabled(&self) -> bool {
        false
    }
}
impl Catalog {
    pub fn register(&mut self, manifest: ValidatedManifest) -> Check {
        let m = manifest.manifest();
        ensure(self.entries.len() < MAX_CATALOG, "catalog limit")?;
        ensure(!self.entries.contains_key(&m.id), "duplicate MOD id")?;
        let names: BTreeSet<_> = m
            .tools
            .iter()
            .map(|t| tool_name(&m.id, &t.name))
            .collect::<Check<_>>()?;
        ensure(names.is_disjoint(&self.names), "duplicate catalog tool")?;
        // All fallible validation precedes either mutation.
        self.names.extend(names);
        self.entries.insert(m.id.clone(), CatalogEntry { manifest });
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.get(id)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionReview {
    Denied,
    RequiresExplicitUserApproval,
}
/// Analysis only. Even a claimed read-only MOD is an unconfined arbitrary process.
pub fn analyze_execution(policy: &crate::Policy) -> ExecutionReview {
    if policy.classroom_safe || policy.read_only {
        ExecutionReview::Denied
    } else {
        ExecutionReview::RequiresExplicitUserApproval
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum FailureCode {
    InvalidParams,
    Failed,
    Cancelled,
    LimitExceeded,
}
impl TryFrom<String> for FailureCode {
    type Error = Error;
    fn try_from(value: String) -> Check<Self> {
        match value.as_str() {
            "invalid_params" => Ok(Self::InvalidParams),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "limit_exceeded" => Ok(Self::LimitExceeded),
            _ => Err(Error("unknown failure code")),
        }
    }
}
/// No plugin diagnostics/messages/stacks are accepted in error results.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Success { data: Value },
    Error { code: FailureCode },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Message {
    Handshake {
        api_version: u32,
        mod_id: String,
    },
    Request {
        api_version: u32,
        id: String,
        tool: String,
        params: Value,
    },
    Result {
        api_version: u32,
        id: String,
        tool: String,
        outcome: Reply,
    },
    Cancel {
        api_version: u32,
        id: String,
    },
}
impl Message {
    pub fn parse(bytes: &[u8], manifest: &ValidatedManifest) -> Check<Self> {
        let message: Self = parse(bytes, manifest.manifest.limits.max_message_bytes)?;
        message.validate(manifest)?;
        if matches!(message, Self::Result { .. }) {
            ensure(
                bytes.len() <= manifest.manifest.limits.max_result_bytes,
                "result byte limit",
            )?;
        }
        Ok(message)
    }
    pub fn to_bytes(&self, manifest: &ValidatedManifest) -> Check<Vec<u8>> {
        self.validate(manifest)?;
        encode(self, manifest.manifest.limits.max_message_bytes)
    }
    pub fn validate(&self, manifest: &ValidatedManifest) -> Check {
        let m = manifest.manifest();
        let (version, id) = match self {
            Self::Handshake {
                api_version,
                mod_id,
            } => {
                ensure(mod_id == &m.id, "handshake identity mismatch")?;
                (*api_version, None)
            }
            Self::Request {
                api_version,
                id,
                tool,
                params,
            } => {
                manifest.tool(tool)?.input_schema.validate_value(params)?;
                encode(params, MAX_PARAMS_BYTES)?;
                (*api_version, Some(id))
            }
            Self::Result {
                api_version,
                id,
                tool,
                outcome,
            } => {
                let spec = manifest.tool(tool)?;
                if let Reply::Success { data } = outcome {
                    spec.output_schema.validate_value(data)?;
                }
                encode(self, m.limits.max_result_bytes)?;
                (*api_version, Some(id))
            }
            Self::Cancel { api_version, id } => (*api_version, Some(id)),
        };
        ensure(version == API_VERSION, "unsupported API version")?;
        if let Some(id) = id {
            ensure(token(id, 64), "invalid correlation id")?;
        }
        encode(self, m.limits.max_message_bytes)?;
        Ok(())
    }
    /// Stateless kind/correlation check, not transport direction enforcement or cancellation.
    /// Caller must track outstanding IDs, transport direction, replay and lifecycle.
    pub fn validate_reply_to(&self, request: &Message, manifest: &ValidatedManifest) -> Check {
        request.validate(manifest)?;
        self.validate(manifest)?;
        let Self::Request {
            id: expected,
            tool: expected_tool,
            ..
        } = request
        else {
            return Err(Error("expected request"));
        };
        match self {
            Self::Result { id, tool, .. } => ensure(
                id == expected && tool == expected_tool,
                "result correlation mismatch",
            ),
            Self::Cancel { id, .. } => ensure(id == expected, "cancel correlation mismatch"),
            _ => Err(Error("expected result or cancel")),
        }
    }
}

#[cfg(test)]
#[path = "mods_tests.rs"]
mod tests;
