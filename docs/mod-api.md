# Neo MOD API v1 草案：纯数据 SDK，不是加载器

## 当前边界

已实现 `neo_tools::mods`：清单校验、受限 JSON Schema 子集、消息序列化/校验、禁用目录、执行前策略分析。所有合约字符串均自有所有权，不借用输入，也不泄漏为静态工具。没有新增依赖。

**没有**目录扫描、安装、签名验证、进程启动、动态库加载、脚本执行、IPC、沙箱、超时终止、内存配额执行、自动批准或模型消息接入。不会改动内置 `registry()`、`dispatch()`、`openai_tools()`。示例只有 JSON，`bin/example.exe` 是描述性路径，没有附带可执行文件，不会查找或运行它。

本文是 `docs/` 中唯一公开的文档；其他文档与本地示例文件已移至被 Git 忽略的 `docs-pri/`，不随仓库发布。公开的 [mods_tests.rs](../crates/neo-tools/src/mods_tests.rs) 自带完整清单与消息示例，默认测试不依赖私有文件。另提供显式 `#[ignore]` 本地文档审计，用于比对本文和私有示例；只有手动指定 `--ignored` 才执行，缺失文件会报错。普通仓库使用者只需运行默认 MOD 测试。

## 实际公共接口

所有校验返回 `Check<T> = Result<T, Error>`，`Error` 只含静态诊断短语，不复制恶意 JSON、解析器堆栈或插件诊断。

| 接口 | 实际作用 |
| --- | --- |
| `ValidatedManifest::parse(&[u8])` | 限制原始字节数，严格解析并验证，得到不可变有效清单 |
| `Manifest::validate(self)` | 校验手动构造的自有字段，消费原始清单，返回 `ValidatedManifest` |
| `ValidatedManifest::manifest()` | 只读访问 `&Manifest`；不能修改已验证内容 |
| `tool_name(mod_id, local_name)` | 返回稳定的命名空间工具名，并检查当前内置名称冲突 |
| `Limits::validate()` | 拒绝越界声明，不默默截断 |
| `Schema::validate()` / `validate_value(&Value)` | 校验 Schema 子集/具体对象的必需字段、未知字段、类型、字符串字节上限 |
| `Catalog::default()` / `register(ValidatedManifest)` | 创建内存目录/原子注册；失败前不修改目录 |
| `Catalog::get(id)` / `len()` / `is_empty()` | 只读查询；`CatalogEntry::manifest()` 返回有效清单 |
| `CatalogEntry::enabled()` | 恒为 `false`；没有启用入口 |
| `analyze_execution(&Policy)` | 只返回 `Denied` 或 `RequiresExplicitUserApproval`；没有 Allow |
| `Message::parse(bytes, &manifest)` | 校验整个消息、版本、工具、参数或结果；不是流读取器 |
| `Message::validate(&manifest)` / `to_bytes(&manifest)` | 校验自有消息/输出紧凑 JSON 字节，不含帧头或换行 |
| `Message::validate_reply_to(&request, &manifest)` | 验证双方合约以及 Result 的 id+tool、Cancel 的 id 一致；不发送取消、不追踪生命周期 |

公开数据类型还有 `Manifest`、`Tool`、`Capability`、`Limits`、`Schema`、`ObjectType`、`Scalar`、`Message`、`Reply`、`FailureCode`、`ExecutionReview`。普通 serde 反序列化只构造原始数据，**不是受信入口**；外部字节必须走上述 `parse`，手动构造必须走 `validate`。`Schema::validate_value` 本身不执行信封或总参数字节上限；使用 `Message` 才会检查这些上限。

## 清单、命名与版本

完整示例清单见 [mods_tests.rs](../crates/neo-tools/src/mods_tests.rs) 的 `MANIFEST` 常量，所有列出的结构字段都必填，所有结构使用 `deny_unknown_fields`；枚举只接受已列值。能力、Schema 的 object 类型及错误码必须使用 JSON 字符串，不接受 serde 外部标签对象（例如 `{"cancelled": null}`）。各层 JSON 对象拒绝重复键，包括参数、Schema 属性和消息标签。拒绝非 UTF-8、多个连续 JSON 值、非有限数、空输入；保留 serde_json 默认递归深度限制；当前依赖下允许最多 127 层嵌套容器，第 128 层拒绝，包含根容器。Schema 子集本身仍不允许嵌套对象或数组值。重复键按解码后的名称比较，`"text"` 与 `"te\u0078t"` 也算重复。

- `api_version`：整数，严格等于 `API_VERSION = 1`，不做模糊兼容协商。
- `revision`：正 `u32` 发布序号，不是 SemVer。发行方应递增；当前没有更新机制，因此不比较序号或防降级。
- `id`：至少两个点分段，整体最多 32 ASCII 字节；每段 1–16 字符，语法 `[a-z][a-z0-9]*`。建议用组织命名空间；没有域名所有权认证。
- 工具 `name`：`[a-z][a-z0-9]*`，最多 24 字符。完整名为 `mod_` + 将 id 的点换为单下划线 + `__` + 本地名。例如 `org.example` / `echo` → `mod_org_example__echo`。源名称不允许下划线，映射无歧义；最多 62 字符，且代码显式要求 ≤64。不同 MOD 可以用相同本地名。
- 注册检查清单内重复工具名、已有 MOD id、已有完整工具名、Neo 内置工具名；已有 id 即使 revision 更高也拒绝，不是覆盖更新。目录最多 128 个 MOD，每个清单 1–16 个工具。
- `description`：非空白、最多 512 UTF-8 字节、无控制字符；仍是不可信文字，不自动进入模型提示词。
- `executable`：仅做平台无关的保守词法校验。长度 1–240 ASCII 字节，用 `/` 分段；每段仅允许字母、数字、点、下划线、连字符。不允许空段、`.`、`..`、尾点、空格、反斜杠、冒号、盘符、UNC、绝对路径、Windows ADS、控制字符及非 ASCII 路径。每段不分大小写拒绝设备名（含带扩展名的 CON/PRN/AUX/NUL、COM0–9、LPT0–9 等）；也不会展开环境变量或 `~`。

路径合法**不证明文件存在、可执行或位于包内**。未来加载器仍须处理规范化路径、符号链接、Windows junction/reparse point、替换竞态、安装包身份以及真实目录边界。

v1 草案只认当前字段和类型；新增不兼容字段/消息/能力/Schema 功能应发布新的 API 整数版本及对应迁移校验器。当前不能直接接受 v2，不自动降级，不把 `revision` 当作协议兼容标记。

## 能力声明与策略：不构成权限执行

`capabilities` 接受 `workspace_read`、`workspace_write`、`network`、`desktop_read`、`desktop_input`、`process_spawn`，可以为空但不能重复。它描述 MOD 自称需要什么，**不是授权、沙箱或可信行为证明**。没有声明 `process_spawn` 也不意味着一个任意可执行程序不能启动子进程。

不能把 MOD 包装成内置 `Risk::Read` 后调用现有 `Policy::decide`：内置 Read 可能直接放行，而任意未隔离进程可能访问文件、桌面、网络、凭据或启动子进程。`analyze_execution` 无论声明什么能力：

1. `classroom_safe` 或 `read_only` 为真，一律 `Denied`。
2. 其余情况一律 `RequiresExplicitUserApproval`，即使 `auto_approve = true`；`allow_open` 不提供 MOD 授权捷径。
3. 这个结果只是分析，不保存批准、不授予权限、不启用目录、更不会运行进程。未来实际执行边界必须重新检查策略，并获得针对当前 MOD 身份/版本和操作的明确用户批准。

## Schema 子集和边界

输入与成功输出均为顶层 object，显式写 `type: object`、`properties`、`required`、`additionalProperties: false`。最多 32 个属性，属性名为 `[a-z][a-z0-9]*` 且最多 32 字符。required 无重复且必须引用已有属性；空对象 Schema 合法。

属性仅支持 `{ "type": "string" }`、`integer`、`number`、`boolean`。不支持嵌套对象、数组、null、联合类型、引用、enum、默认值、正则、范围关键字或任意完整 JSON Schema。未知关键字直接拒绝，不忽略。integer 必须是 serde_json 保留的有符号/无符号整数表示，`1.0` 不算 integer；number 接受整数和有限浮点数，不保证任意精度。每个字符串最多 4096 UTF-8 字节，不按字符数计算。

| 边界 | 主机硬上限/检查方式 |
| --- | --- |
| 清单 | 原始输入和紧凑序列化均 ≤32768 字节 |
| 整个消息 | `max_message_bytes`，1–65536 字节；原始输入包含空白 |
| 整个 Result 信封 | `max_result_bytes`，1–32768 字节且不大于消息上限；原始输入和紧凑序列化都检查 |
| Request.params | 紧凑 JSON ≤16384 字节；原始空白仍计入消息上限 |
| `timeout_ms` | 声明必须为 1–60000 |
| `memory_mib` | 声明必须为 1–256 |
| `max_in_flight` | 声明必须为 1–4 |

字节序列化使用有上限的输出缓冲，超限拒绝而不截断 JSON。解析先检查输入总长，再做重复键与类型校验。时间、内存、并发目前**只验证声明范围，不落实进程资源限制**。声明过小可以导致任何实际消息都无法通过，不承诺该 MOD 可运行。Schema 不赋予参数值任何文件权限，也不能识别字符串里的秘密或提示注入。

## 消息示例与关联

以下唯一 JSON 代码块与公开 `mods_tests.rs` 内的信封示例一致。默认测试校验内置副本；显式文档审计先严格解析本地展示数组（拒绝重复键），再逐条用实际 `Message` API 校验，并对 Result/Cancel 验证 Request 关联。数组只是展示容器，**不是 wire 批处理**；成功结果与 cancelled 结果是互斥的示例分支，不应作为同一请求的两个终结结果发送。

```json
[
  {"type":"handshake","api_version":1,"mod_id":"org.example"},
  {"type":"request","api_version":1,"id":"r1","tool":"mod_org_example__echo","params":{"text":"你好"}},
  {"type":"result","api_version":1,"id":"r1","tool":"mod_org_example__echo","outcome":{"status":"success","data":{"text":"你好"}}},
  {"type":"result","api_version":1,"id":"r1","tool":"mod_org_example__echo","outcome":{"status":"error","code":"cancelled"}},
  {"type":"cancel","api_version":1,"id":"r1"}
]
```

所有消息带精确 `api_version`。Handshake 的 `mod_id` 必须与清单一致，但它不是身份认证。Request/Result/Cancel 的 `id` 为 1–64 个 ASCII 字符，语法 `[a-z][a-z0-9]*`；工具名必须匹配该清单。Result 成功 data 必须通过输出 Schema；错误只接受固定 `FailureCode`：`invalid_params`、`failed`、`cancelled`、`limit_exceeded`，没有自由格式 message、stderr、trace 或 stack 字段。

`Message::parse` 不知道目前有哪些调用；接收 Result 或检查 Cancel 时还须对相应 Request 调用 `validate_reply_to`。它检查关联，不检查时序、唯一 id、重放、会话身份、传输方向或并发数，也不终止进程。未来主机应维护待处理请求表，拒绝未知/重复/已完成 id；每个调用最多接受一个终结结果；取消只是请求，已发生副作用不能回滚。

## 未来传输提案（尚无实现）

建议专用管道使用 4 字节无符号大端长度 + 一个 UTF-8 JSON 对象；在按长度分配/读取前检查主机上限。这里的任何 API 都不读写管道、不添加/移除长度前缀、不处理分片或 EOF。

建议 MOD → 主机先发送 Handshake，再由主机在**明确批准与隔离准备完成后**发送 Request；主机 → MOD 可发送 Cancel，MOD → 主机发送 Result。诊断走独立本地通道且设限，绝不与协议 stdout 混用。仍需未来实现握手超时、进程树终止、操作系统隔离、工作目录/句柄/环境最小化、参数来源控制等。

## 秘密、诊断与模型边界

合约没有主机 API key、会话 token、环境、系统提示、对话历史、诊断上传或授权对象字段；本模块也不提供任何这样的数据。**未来主机不得向 MOD 注入秘密**，不得继承包含凭据的完整环境或敏感句柄；参数只允许用户明确提供且通过适当数据策略的内容。没有秘密字段不代表任意字符串天然不含秘密，未隔离程序也可能自行读取本机敏感数据。

**主机永远不得把诊断 trace、stderr、解析错误原文、环境或堆栈回灌给模型。** 本模块错误采用静态诊断，插件错误只接收固定 code，且没有模型转换接口。未来主机只能映射固定错误或经过专门审查的结构化结果。成功 data 即使通过 Schema 仍是不可信业务数据，插件可把恶意文本伪装成普通字符串；本 SDK 不能语义识别此类内容，不能自动发送给模型或把它当系统指令。

## 测试范围

`mods_tests.rs` 覆盖有效清单与信封往返、重复/未知字段、版本、非法路径、稳定命名、重复注册原子性（含第二个工具冲突、版本替换拒绝及满目录）、目录容量、只读/课堂/auto_approve 组合、标量参数与结果验证、结果关联、输入和输出字节边界。回归测试还覆盖解码后重复键、默认 JSON 深度边界、枚举字符串表示，以及全部消息种类的精确上限与超限、自有消息验证。

只运行 MOD 测试，串行执行（仓库根目录）：

```sh
cargo test --locked --offline -p neo-tools --lib mods::tests:: -- --test-threads=1
```

维护者可显式审计公开文档与私有 fixtures（要求 `docs/mod-api.md`、`docs-pri/mod-example/manifest.json` 和 `docs-pri/mod-example/envelopes.json` 全部存在；后两者不随仓库发布）：

```sh
cargo test --locked --offline -p neo-tools --lib mods::tests::local_docs_match_validated_fixtures -- --exact --ignored --test-threads=1
```

默认测试不依赖私有文档或 fixtures；LF/CRLF、重复键和无效文档示例的审计辅助函数回归测试使用内存字符串。测试不启动任何 MOD 进程，不验证 runtime、传输或操作系统隔离；通过协议测试不能证明这些尚未实现的边界安全。
