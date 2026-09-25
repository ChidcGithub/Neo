# Neo 工具集规格

> 实现：`crates/neo-tools`（纯逻辑）+ `crates/neo-llm`（协议搬运）+ `neo-app`（权限交互与展示）
> 本文是**逐工具的契约**：名称、用途、参数、返回、适用场景与边界、权限、错误。
> 校验方式：`cargo test -p neo-tools -- --test-threads=1`（含共享解析、分页、路径围栏与异常断言）。
> **工具调用在界面上怎么显示**（变体分类 / 摘要派生 / 状态点 / 扫光）见
> `docs/design-kit.md` 的「工具调用行」一节，规则取自上游 `dsh-client-ui-tool`。

---

## 一、总览

| 工具 | 中文名 | 风险 | 一句话 | 什么时候**不该**用它 |
|---|---|---|---|---|
| `read_file` | 查看文件 | `read` | 读文本文件内容给模型看 | 图片走 `view_image`；Word/PPT 走 `read_document` |
| `read_document` | 读取文档 | `read` | 提取 DOC/DOCX、PPT/PPTX 及文本正文并按字符分页 | 不做 OCR，不执行宏，不读取工作区外路径 |
| `view_image` | 查看图片 | `read` | 报图片格式/尺寸，可选取回原始数据 | 它不解码像素、不做识别 |
| `open_file` | 打开文件 | `open` | 在用户机器上打开文件/定位它 | 它不读内容，给模型读要用 `read_file` |
| `write_file` | 写入文件 | `write` | 写入或整体覆盖 | 只改一小段用 `edit_file` |
| `edit_file` | 修改文件 | `write` | 把一段确定的原文替换成新文本 | 新建文件用 `write_file` |
| `powershell` | 执行命令 | `exec` | 工作区内执行 PowerShell 命令 | 专用文件/文档/图像工具能表达的不要用它 |
| `bash` | 执行命令 | `exec` | 工作区内的类 Unix 环境（随包 Git Bash）里执行命令 | 同上；写 PowerShell 语法用 `powershell` |
| `screenshot` | 截取屏幕 | `read` | 截屏并把图交给模型看 | 它只是「看一眼」，不会操作界面 |
| `screen_elements` | 屏幕元素 | `read` | 枚举可交互元素并按阅读顺序编号 | 游戏/画布等 UIA 不可见区域仍要靠截图坐标 |
| `screen_element_search` | 搜索屏幕元素 | `read` | 按按钮名称、类型或屏幕位置搜索最近清单，返回 `element_id` 和精确位置 | 找不到按钮时先用它，不要凭截图猜坐标 |
| `click` | 点击鼠标 | `exec` | 按元素编号或坐标点击（左/右键、双击） | 用编号前先 `screen_elements`；坐标兜底前先 `screenshot` |
| `drag` | 拖动鼠标 | `exec` | 按住鼠标从一处拖到另一处 | 用之前先 `screenshot` |

记忆口诀：**读文本 `read_file`、看图 `view_image`、给人看 `open_file`、
整写 `write_file`、点改 `edit_file`、其余交给 shell（Windows 事务用 `powershell`，
文本与 git 类事务用 `bash`）；屏幕上要「看」用 `screenshot`、"动手"用 `click` / `drag`。**

### 命名与参数风格（强制）

- 工具名：`snake_case` 动词短语，动词在前；
- 参数名：全小写 `snake_case`；**路径一律叫 `path`，内容一律叫 `content`**；
- 布尔参数默认 `false`，且语义指向"更危险/更主动的那一侧"（`overwrite` / `replace_all` / `include_data` / `reveal`）；
- 整数参数都带闭区间，越界报 `bad_arguments`，**绝不静默夹取**；
- 全部参数声明在 `Param` 里写一次，JSON Schema 与取值校验都由它生成（见 §五）。

---

## 二、逐工具契约

### 1. `read_file` —— 查看文件

**用途**：读取工作区内文本文件的内容。模型改文件之前必须先用它确认原文。

**风险**：`read`（只读，直接放行，不打扰用户）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `path` | string | ✅ | — | 工作区内 | 相对工作区，如 `src/main.rs` |
| `offset` | integer | | `0` | `0…10000000` | 起始行号（从 0 开始） |
| `limit` | integer | | `400` | `1…2000` | 最多返回多少行 |

**成功返回**

```json
{ "ok": true, "tool": "read_file", "summary": "读取 a.txt 第 0–399 行（共 1200 行）",
  "data": { "path": "a.txt", "bytes": 20480, "lines_total": 1200,
            "lines_returned": 400, "offset": 0, "truncated": true,
            "next_offset": 400, "content": "…" } }
```

`truncated = true` 时一定带 `next_offset`，模型据此接着读。

**失败**

| 情形 | kind | hint 指向 |
|---|---|---|
| 缺 `path` / `path` 为空 | `bad_arguments` | 该参数是必填 |
| 文件不存在 | `not_found` | 先用 `powershell` 的 `Get-ChildItem` 确认路径 |
| 目标是目录 | `unsupported` | `ls` 列目录，本工具只读单个文件 |
| 超过 1 MiB | `too_large` | 用 offset/limit 分段，或 grep 定位 |
| 不是 UTF-8 | `unsupported` | 图片用 `view_image`，二进制用 `powershell` |
| 路径越界 | `not_allowed` | 工具只能碰工作区内 |

**边界**：只读文本。不知道文件在哪、有哪些文件时，用 `powershell` 的 `Get-ChildItem` —— 本工具不做目录列举。

---

### `read_document` —— 读取文档

附件导入和工具调用共用 `neo-tools/src/documents.rs`；模型可以直接读取工作区内文件，无需用户先加附件。

- `path`：必填，工作区内 DOC/DOCX、PPT/PPTX、TXT/MD/CSV/JSON/LOG/TSV。
- `offset`：Unicode 字符位置，从 0 开始，范围 0–80000；不是字节或行号。
- `limit`：默认 4000，范围 1–8000。输出还受 24000 个 JSON 字符及 32 KiB 字节双预算限制，可能缩页。
- 返回 `content`、`name`、`type`、`total_chars`、`offset`、`next_offset`、`has_more`、`warning`。`has_more=true` 时使用返回的 `next_offset` 继续；末页为 `null`。起点超过已提取长度时返回末尾空页。
- 原文件最多 32 MiB，提取文字最多 80000 字符；超限有警告。`has_more=false` 只代表已提取内容读完，不代表被截断的原文件全文已读完。
- DOCX 提取正文和表格；PPTX 按演示关系中的幻灯片顺序。旧 DOC 只覆盖支持的 Word97–2007 主文档；旧 PPT 可能包含历史保存记录，记录顺序不等于放映顺序，精确读取建议转 PPTX。
- 不执行宏、不启动 Office、不获取外部关系；不做嵌入图片 OCR。加密、损坏、无可提取文本明确失败。图片引导 `view_image`。
- 路径必须通过工作区规范化及已存在目标围栏验证；权限归类为 `read`，工具行归类为读取变体。

```json
{"path":"课程/课件.pptx","offset":0,"limit":4000}
```

---

### 2. `view_image` —— 查看图片

**用途**：查看工作区内图片的格式、像素尺寸与体积；需要时把原始数据（base64 data URL）
一并取回，交给多模态模型"看图"。

**风险**：`read`

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `path` | string | ✅ | — | 相对工作区 |
| `include_data` | boolean | | `false` | 是否连同原始数据返回（很占上下文） |

**成功返回**

```json
{ "ok": true, "tool": "view_image", "summary": "docs/a.png：1920×1080 PNG（78.4 KB）",
  "data": { "path": "docs/a.png", "format": "png", "width": 1920, "height": 1080,
            "bytes": 80281, "data_url": "data:image/png;base64,…" } }
```

`data_url` 仅在 `include_data = true` 时出现。

**支持格式**：PNG / JPEG / GIF / BMP / WEBP（按文件头识别，读的是头，不解码像素）。

**失败**

| 情形 | kind | 说明 |
|---|---|---|
| 非图片 / 未识别的容器 | `unsupported` | hint 指向 `read_file`（文本）或 `powershell` 的格式判断 |
| 超过 8 MiB | `too_large` | 先缩小图片 |
| 不存在 / 越界 | `not_found` / `not_allowed` | 同上 |

**边界**：**不做图像处理**。缩放、裁切、OCR、识别画面内容都不在这里 ——
它只回答"这是什么图、多大、原始数据能不能给你"。要看画面内容，靠多模态模型。

---

### 3. `open_file` —— 打开文件

**用途**：在**用户机器上**打开文件（系统默认程序），或在文件管理器里定位它。
用于"让老师亲眼看一眼"，是可见但无破坏的动作。

**风险**：`open`（默认放行；`Policy::allow_open = false` 时拒绝）

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `path` | string | ✅ | — | 相对工作区，**必须已存在** |
| `reveal` | boolean | | `false` | `true` = 在文件管理器里选中它（不打开） |

**成功返回**

```json
{ "ok": true, "tool": "open_file", "summary": "已用默认程序打开 docs/a.pdf",
  "data": { "path": "docs/a.pdf", "action": "open", "launcher": "cmd" } }
```

**平台落点**：Windows `cmd /C start "" <path>` / `explorer /select,<path>`；
macOS `open [-R]`；Linux `xdg-open`。

**失败**：目标不存在 → `not_found`；拉起系统命令失败 → `io`（hint 指出依赖哪个系统命令）。

**边界**：**只管交给系统** —— 不读内容、不解析、不等程序退出。
模型自己要看内容用 `read_file`；这里只解决"给人看"。

---

### 4. `write_file` —— 写入文件

**用途**：写入或**整体覆盖**一个文件。新建文件、整篇重写时用它。

**风险**：`write`（**默认要用户确认**）

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `path` | string | ✅ | — | 相对工作区；父目录会自动创建 |
| `content` | string | ✅ | — | 完整文件内容（**不是追加**）；允许空串 |
| `overwrite` | boolean | | `false` | 目标已存在时是否允许覆盖 |

**成功返回**

```json
{ "ok": true, "tool": "write_file", "summary": "新建 src/a.rs（12 行 / 240 字节）",
  "data": { "path": "src/a.rs", "bytes": 240, "lines": 12,
            "created": true, "overwritten": false } }
```

**失败**

| 情形 | kind | hint 指向 |
|---|---|---|
| 已存在且 `overwrite = false` | `conflict` | 先 `read_file` 看原内容；确认整体替换再设 `overwrite`，或改用 `edit_file` |
| 目标是目录 | `unsupported` | 给一个文件名 |
| 内容超过 1 MiB | `too_large` | 拆文件或用 `powershell` 生成 |
| 越界 | `not_allowed` | — |

**边界**：**整文件语义**。覆盖式写法会抹掉文件里不在上下文中的内容，是最容易出事故的一类操作 ——
所以 `overwrite` 默认 false，且已存在的目标会先做符号链接复查。

---

### 5. `edit_file` —— 修改文件

**用途**：把文件里**一段确定的原文**替换成新文本。**这是默认的改文件方式**。

**风险**：`write`（默认要用户确认；确认框里会显示被替换的原文）

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `path` | string | ✅ | — | 文件必须已存在 |
| `old_string` | string | ✅ | — | 原文，必须**逐字符**一致（含缩进与换行）；不允许空串 |
| `new_string` | string | ✅ | — | 新文本；允许空串（= 删除这段） |
| `replace_all` | boolean | | `false` | 是否替换全部命中 |

**成功返回**

```json
{ "ok": true, "tool": "edit_file", "summary": "src/a.rs：替换 1 处（+3 字节）",
  "data": { "path": "src/a.rs", "replacements": 1, "bytes_before": 240,
            "bytes_after": 243, "old_preview": "let x = 1;", "new_preview": "let x = 2;" } }
```

**失败**

| 情形 | kind | hint 指向 |
|---|---|---|
| 找不到 `old_string` | `not_found` | 先 `read_file` 读原文，别凭记忆写 old_string |
| 命中多处且未开 `replace_all` | `not_unique` | 多带几行上下文让它唯一；确实全替换再设 `replace_all` |
| `old_string == new_string` | `bad_arguments` | 这次调用不会改变任何东西 |
| `old_string` 为空 | `bad_arguments` | 删一段请填原文 |
| 文件不存在 | `not_found` | 新建请用 `write_file` |
| 超过 1 MiB | `too_large` | 用 `powershell` 处理大文件 |

`not_unique` 的 message 里会带上**命中行号**（最多 20 个），模型据此加长上下文即可唯一定位。

**边界**：不做模糊匹配、不做正则、不做 diff——
**宁可失败也不"猜一个改掉"**。要重排全文就用 `write_file`。

---

### 6. `powershell` —— 执行命令

**用途**：在工作区内执行 **PowerShell** 命令（编译、测试、搜索、批量处理）。
**能力上界最高、也最难审计的一个。**

**风险**：`exec`（默认要用户确认；确认框用等宽字体显示完整命令）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `command` | string | ✅ | — | 非空 | **PowerShell 5.1 语法**；同一逻辑操作的多步用 `;` 串起来 |
| `cwd` | string | | 工作区根 | 工作区内、且是已存在的目录 | 执行目录 |
| `timeout_ms` | integer | | `500000` | `100..1800000` | **前台模式**的超时毫秒数（默认 500 秒，够编译与测试）；超时杀进程。后台模式忽略 |
| `background` | boolean | | `false` | — | `true` = 后台启动：立即返回 PID，不等结束、不捕获输出 |

**为什么不是 bash**：Neo 跑在教室一体机（Windows）上 —— 那里一定有 Windows PowerShell，
却不一定有 Git Bash。挑一个**必然存在**的宿主，"可调用"才成立。
`pwsh`（PowerShell 7）若存在会优先用（输出默认 UTF-8）。

**成功返回**（`ok = true` 表示**命令跑起来了**，不代表命令成功 —— 看 `exit_code`）

```json
{ "ok": true, "tool": "powershell",
  "summary": "`cargo test` 退出码 1（2310 ms）",
  "data": { "command": "cargo test",
            "host": "pwsh",
            "executable": "C:/Program Files/PowerShell/7/pwsh.exe",
            "cwd": ".", "background": false,
            "exit_code": 1, "duration_ms": 2310,
            "stdout": "…", "stderr": "…",
            "stdout_truncated": false, "stderr_truncated": false } }
```

**失败**（工具本身没跑起来）

| 情形 | kind | hint 指向 |
|---|---|---|
| 超时 | `timeout` | 拆小命令或提高 `timeout_ms`（上限 1800000 = 30 min）；长驻进程改 `background: true` |
| `cwd` 越界 | `not_allowed` | 只能在当前工作区内 |
| `cwd` 不是目录 | `bad_arguments` | 用 `Get-ChildItem` 确认 |
| 启动宿主失败 | `io` | 可用 `NEO_SHELL` 指定可执行文件 |
| 非 Windows 且无 `pwsh` | `unsupported` | 装 PowerShell 7 或指定 `NEO_SHELL` |

**宿主选择顺序**：`NEO_SHELL` → PATH 里的 `pwsh` → `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`
（已知路径优先于 PATH，便于被裁剪过 PATH 的环境）。

**每次调用都会固定三件事**（否则结果不可信）：

| 固定项 | 为什么 |
|---|---|
| `[Console]::OutputEncoding = UTF8` | PowerShell 5.1 按控制台 OEM 代码页（中文机器 GBK）写出，不钉住就全是乱码 |
| `$ErrorActionPreference = 'Stop'` | cmdlet 的错误变成终止错误（退出码 1），而不是"打一行红字继续跑完" |
| 尾部 `if ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE }` | `-Command` 自己不传原生程序退出码，不加这条 `cargo test` 失败也报 0 |

另外固定 `-NoProfile -NonInteractive`：不加载用户 profile（输出可预期），
需要交互的 cmdlet 直接报错而不是挂住。

**两种模式**：

| 模式 | `background` | 行为 | 用在哪 |
|---|---|---|---|
| 前台（默认） | `false` | 等命令结束，捕获 stdout/stderr，返回退出码与耗时 | 编译、测试、搜索 —— 要结果的 |
| 后台 | `true` | 启动即返回（带 `pid`），不等待、不捕获输出 | 起服务、监听、长驻进程 |

后台模式要看输出就自己重定向（PowerShell 的 `*>` 收全部流），再用 `read_file` 读：

```powershell
npm run dev *> .neo-dev.log
```

返回里带 `"pid"`、`"timeout_applies": false`，可用 `Get-Process -Id <pid>` / `Stop-Process` 管理。

**执行在独立线程**：整次工具调用（不论前台后台）由 `neo-app` 丢到后台线程执行，
渲染循环只负责逐帧收结果。这是必须的 —— 前台模式默认给 500 秒，
同步执行会把大屏冻住 5 分钟，看起来就是死机。
执行期间的卡片显示「执行中…」，界面保持可交互（但这一轮不接受新的提问）。

**边界**：
1. 能用 `read_file` / `write_file` / `edit_file` / `view_image` 表达的，**不要用命令** ——
   前者回传结构化结果，后者回传一坨回显，模型和用户都更难核对；
2. 一次命令能做完的，不要拆成多次调用；
3. `kill` 只杀直接子进程（没有作业对象），超时后孙进程可能残留 —— 已知局限；
4. 执行策略（ExecutionPolicy）不被绕过：仓库里的 `.ps1` 若被策略禁止，会照实报错。

### 7. `bash` —— 执行命令（类 Unix 环境）

**用途**：在工作区内的**类 Unix 环境**里执行命令。Git Bash / MSYS2 给的是
一整套 Unix 工具（`ls` / `grep` / `sed` / `awk` / `find` / `git`），处理文本、
跑版本控制、批量改文件都比 PowerShell 顺手，也是模型最熟的方言。

**风险**：`exec`（**需用户确认**）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `command` | string | ✅ | — | 非空 | bash 语法（**不是 PowerShell**），多步用 `&&` / `;` |
| `cwd` | string | | 工作区根 | 工作区内、且是存在的目录 | 执行目录 |
| `timeout_ms` | integer | | `500000` | `100…1800000` | 前台超时；超时杀进程并返回 `timeout` |
| `background` | boolean | | `false` | — | `true` = 后台启动，立即返回 PID，不等、不捕获 |

参数与 `powershell` **逐项一致**（同结构、同默认值、同区间），
有一条测试专门盯着，防止两份声明漂移。

**宿主从哪来：随包提供**

一体机上不一定装了 Git，所以**运行时随包提供** ——
`tools/fetch_runtime.py` 把 Git for Windows 的 MinGit 解到 `runtime/gitbash/`，
本工具优先用它。"这个工具真的能用"因此不依赖用户先装 Git。

解析顺序（`tools::shell::resolve_bash`，顺序即优先级）：

| # | 来源 | 说明 |
|---|---|---|
| 1 | `NEO_GIT_BASH` | 显式指定 bash.exe 的完整路径 |
| 2 | **随包运行时** | `runtime/gitbash/bin/bash.exe`（可执行文件目录及其上溯几层；`NEO_RUNTIME` 可直指 runtime 目录） |
| 3 | PATH 里的 `bash.exe` | **排除** `WindowsApps\bash.exe` —— 那是 WSL 的应用执行别名，跑它等于去启动一个 Linux 发行版 |
| 4 | PATH 里 `git.exe` 反推 | Git for Windows 的标准安装只把 `cmd` 放进 PATH，那里**没有** bash.exe，所以拿 git 的位置反推 |
| 5 | 常见安装位置 | `%ProgramFiles%\Git`、`%LOCALAPPDATA%\Programs\Git` 等 |
| 6 | C..G 盘扫描 | 装在非系统盘又没进 PATH 的情况（一体机上常见） |

全都没有时返回 `unsupported`，`hint` 里给出三条出路（下载运行时 / 装 Git / 用 `NEO_GIT_BASH`）。

**环境约定**

- `--noprofile --norc`：不读用户配置，输出可预期（与 `powershell` 的 `-NoProfile` 同理）；
- 启动时把所选 bash 自己的目录前置到 `PATH`：MinGit 的核心工具在 `usr/bin`，不做这步会出现「bash 能启动，但 ls/grep/sed command not found」；
- `LANG` / `LC_ALL` 钉在 `C.UTF-8`：否则某些工具在未设 locale 时按单字节处理 UTF-8；
- **不开 `pipefail`**：那会改变命令语义。用户看到的应当和自己在 Git Bash 里敲的一样；
- MSYS2 会把"看起来像 Unix 路径"的**参数**自动转成 Windows 路径 —— 这是 Git Bash 的默认行为，
  **不干预**（同上：行为与用户手敲一致）。

**结果**：与 `powershell` 同一外壳（`ok` / `data.exit_code` / `stdout` / `stderr` / `duration_ms`），
`host` 字段区分用的是哪一份宿主：`gitbash-bundled`（随包）/ `gitbash`（系统安装）。

**示例**

```jsonc
// 找一处调用点
{ "command": "grep -rn 'spawn_ready_tools' crates/ | head -20" }

// 看 git 状态并打印最近三次提交
{ "command": "git status --short && git log --oneline -3" }

// 起一个常驻进程（后台）
{ "command": "npm run dev > .neo-dev.log 2>&1", "background": true }
```

**边界**：

1. 能用前五个工具表达的，**不要用命令**（同 `powershell`）；
2. **两个 shell 不互相代替**：`bash` 里写 PowerShell 语法会 "command not found"，反之亦然。
   各自的 `command` 说明里写明了方言，并互相指路；
3. `background: true` 同样**不等待、不捕获输出**，要看输出请自己重定向到工作区内的文件；
4. 超时同样只杀直接子进程（没有作业对象），孙进程可能残留 —— 已知局限。

### 8. `screenshot` —— 截取屏幕

**用途**：截取整个虚拟桌面或其中一块，**把图直接交给模型看**（认界面、读屏幕上的
文字、确认某个操作的结果）。图同时存进工作区的 `screenshots/` 下，之后可以再用
`view_image`、裁剪或交给用户。

**风险**：`read`（只看不动，直接放行）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `x` | integer | | 整屏 | `-32768…32768` | 区域左上角 x（可负） |
| `y` | integer | | 整屏 | `-32768…32768` | 区域左上角 y |
| `width` | integer | | 整屏 | `0…32768` | 区域宽度，需为正 |
| `height` | integer | | 整屏 | `0…32768` | 区域高度，需为正 |

**`x`/`y`/`width`/`height` 要么四个都给、要么都不给** —— 只给一半会明确报错并指出
缺了哪个。半开区间比"缺的按 0 算"好排查得多（后者会静默截到一块莫名其妙的区域）。

**返回**：`path`（工作区内的相对路径）、`saved_bytes`、`region`、`virtual_screen`、
`dpi_scale`、`image_attached`，并把 PNG 作为图片附在结果上。

**上限**：单张内联图官方限制 32 MiB（`limits::INLINE_IMAGE_BYTES`）。超了就
**不发图**、只给路径，并在 `data.next` 里说明改怎么办（截小一点，或用 `view_image`
看已保存的文件）—— 静静发一个超限请求只会换来一个难懂的 400。

### 9. `screen_elements` —— 屏幕元素

**用途**：通过 Windows UI Automation 枚举屏幕上的按钮、链接、菜单项、输入框等可交互元素，按**上→下、左→右**编号。模型随后用 `click.element_id` 引用编号，无需从截图估算坐标。

**风险**：`read`（只枚举辅助功能树；直接放行）

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `window` | string | | 空 | 只保留顶层窗口标题包含该串的元素；给出后覆盖默认焦点窗口范围 |
| `annotate` | boolean | | `false` | 附带当前页的红框 + 黑底白字编号 SoM 截图 |
| `all` | boolean | | `false` | `true` 时枚举所有窗口；默认只看当前焦点窗口 |

**返回**：默认只枚举当前焦点窗口，避免把其他应用的控件噪音塞给模型。每次最多返回 **50 个**元素；若 `pages > 1`，按相同参数重复调用会自动轮换到下一页，最后一页后回到第一页。`elements` 每项含 `id`、`role`、`name`、`window`、`intent_hint`、`x/y/w/h`。其中 `intent_hint` 只是根据控件类型和可访问名称生成的保守用途提示，会明确要求结合窗口和截图确认；UIA 原始名称与窗口才是系统事实。编号在整次枚举中保持稳定，因此第二页的 `id` 可以直接交给 `click.element_id`。

**实现约束**：不在祖先节点按 `IsOffscreen` 剪枝（Chromium 会误报整棵树不可见），而是在叶子级用 bbox 与虚拟桌面求交；DWM cloaked 顶层窗口会剔除。最近一次枚举结果缓存 5 分钟，供 `click.element_id` 查表。

**边界**：游戏、视频画布、GPU 自绘控件可能不暴露 UIA 元素，此时回退到 `screenshot` + `click.x/y`。窗口刚切换时 UIA 树可能短暂重建，可等一秒再枚举。

### 10. `screen_element_search` —— 搜索屏幕元素

**用途**：当模型知道“要点保存按钮/右上角菜单/输入框”，但不确定对应编号或坐标时，按名称、控件类型和屏幕区域搜索最近一次 UIA 清单。没有缓存会自动枚举当前焦点窗口。

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `query` | string | 空 | 名称或窗口标题关键词，大小写不敏感 |
| `role` | string | 空 | `Button`、`Edit`、`MenuItem` 等 |
| `position` | string | 空 | `top`、`bottom`、`left`、`right`、`center` 或四角 |
| `limit` | integer | `10` | 最多返回 50 个候选 |
| `refresh` | boolean | `false` | 重新枚举当前焦点窗口 |

每个候选都返回 `element_id`、`window`、`position.x/y/width/height`、中心点、屏幕区域和用途提示。确认后直接把 `element_id` 传给 `click`，不要重新估算坐标。

**推荐工作流**：`screen_element_search(query="保存", role="Button")` → 从候选的名称/窗口/位置确认 → `click(element_id=...)` → `screenshot` 验证结果。搜索结果中的 `intent_hint` 是启发式提示，不是系统保证。

### 11. `click` —— 点击鼠标

**用途**：按 `screen_elements` 的编号点击元素中心，或直接按屏幕坐标点击。左键/右键、单击/双击。

**风险**：`exec`（**需用户确认** —— 点一下可能真的改变什么）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `element_id` | integer | | `0` | `0…9999` | 最近一次 `screen_elements` 返回的编号；给了就优先于 x/y |
| `x` | integer | | `0` | `-32768…32768` | 坐标定位的 x |
| `y` | integer | | `0` | `-32768…32768` | 坐标定位的 y |
| `button` | string | | `left` | `left` / `right` | 也接受 `l` / `r` / `primary` / `secondary` |
| `double` | boolean | | `false` | — | 双击 |

`element_id` 与 `x/y` **至少给一种**；坐标模式必须同时给 x 与 y，两者都给时编号优先。编号不存在或缓存超过 5 分钟时，在发出鼠标事件之前返回 `bad_arguments`，提示重新运行 `screen_elements`。

**双击是一个动作，不是两次 `click`**：两次工具调用之间隔着一次网络往返，
早就超过系统的双击间隔了。`double: true` 会在**一次** `SendInput` 里发出
"按下—抬起—按下—抬起"，刚好落在双击判定窗口内。

### 11. `drag` —— 拖动鼠标

**用途**：选中一段文字、拖滑块、把文件拖进窗口、框选。

**风险**：`exec`（**需用户确认**）

| 参数 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `x` / `y` | integer | ✅ | — | `-32768…32768` | 起点 |
| `to_x` / `to_y` | integer | ✅ | — | `-32768…32768` | 终点 |
| `button` | string | | `left` | `left` / `right` | 也接受别名 |
| `duration_ms` | integer | | `300` | `0…10000` | 从起点移到终点的耗时 |

**为什么不是一个 `click` 加一个 `click`**：拖动＝按下 → **移动** → 抬起，而"移动"
是其中的关键 —— 直接"按下、跳到终点、抬起"在多数应用里会被当成单击
（中间没有鼠标移动消息）。所以按 `duration_ms` 切成若干步（每步 ~15 ms）走完。

**中途失败也要松手**：任何一步出错都会先补一次"移到位 + 抬起"，
否则用户会留下一个"一直按着"的鼠标。

### 屏幕交互的公共约定

#### 坐标系：虚拟桌面的物理像素

三个屏幕工具用的是**同一套坐标**：虚拟桌面（所有显示器拼起来的画布）的**物理像素**，
原点在虚拟桌面左上角。副屏排在主屏左边/上边时，坐标是**负数**。

```text
        ┌─────────────┬─────────────┐
        │ 显示器 1     │ 显示器 2     │   ← 虚拟桌面 = 并集
        │  (0,0)      │  (1920,0)   │
        └─────────────┴─────────────┘
```

**照 `screenshot` 里看到的像素位置写就行**，不需要自己换算缩放。

#### DPI：为什么必须处理

进程如果是 **DPI 不感知**的，Windows 会把桌面按缩放比例"虚拟化"：一块
3840×2160、缩放 200% 的屏在进程眼里是 1920×1080，`BitBlt` 抓回来的是**被拉伸过的
糊图**，`SendInput` 也按虚拟坐标落点 —— 于是"照着截图点"必然点偏。

所以进屏幕层之前先 `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`
（`eframe`/`winit` 启动时通常已设过，重复设置只返回 `FALSE`，无害），
拿到真实物理像素；并用 `GetDpiForMonitor` 把缩放比报在结果里。

**实测对照**（开发机，200% 缩放。两侧读的是同一组 Win32 API，
一份是 Rust 屏幕层、一份是 Python + ctypes 的独立实现）：

| 读法 | `SM_CXVIRTUALSCREEN` × `SM_CYVIRTUALSCREEN` |
|---|---|
| 不设 DPI 感知（被虚拟化） | 1600 × 1000 |
| 设 `PER_MONITOR_AWARE_V2`（本工具的坐标系） | **3200 × 2000** |

工具报出来的是后者，说明坐标确实落在物理像素空间 ——
这正是"照着截图点"能点准的前提。

诊断入口：`cargo test -p neo-tools --lib -- --ignored --nocapture dump_screen_info`
会打印虚拟桌面尺寸与中心点的缩放比，"为什么点偏了"先看它。

#### 越界一个事件都不发

点击/拖动的坐标会先按虚拟桌面**半开区间**校验：越界直接返回 `bad_args`，
错误里带上**合法范围**（模型据此自己改，不用再问一轮）。
这条是有测试盯着的 —— 鼠标不该"先跑过去再报错"。

#### 能力边界（已知局限）

- `SendInput` 打到**以管理员身份运行**的窗口会被 UIPI 静默丢掉。
  工具会核对"送出去几个事件、系统收下几个"，不一致就返回 `not_allowed`
  并提示"要么用管理员身份启动 Neo"；这是**唯一**能区分"点了但没生效"的办法。
- 截屏**不含鼠标光标**（`BitBlt` 本来就不含）—— 对"给模型看内容"来说正好。
- 还没有键盘输入（打字、快捷键）。要打字目前只能用 `bash` 里的 `input` 之类，
  或者后续再补 `type_keys`。
- 超时/取消只作用于**这次工具调用**；已经发出去的那几下点击是收不回来的。

#### 测试怎么对待"会真的动鼠标"

- **截屏**是只读的，集成测试里直接跑（抓一块 64×48、存进工作区、断言 PNG 魔数）；
- **点击 / 拖动会真的动用户的鼠标**，所以默认**跳过**，只有显式设
  `NEO_TEST_INPUT=1` 才跑（`click_and_drag_really_move_the_mouse_when_explicitly_allowed`）。
  跑测试的人可能正在用这台电脑，自动化用例不该去抢他的鼠标；
- **越界拒绝**这条不需要门控 —— 它断言的正是"一个事件都没发"。

### 附：bash → PowerShell 速查（写进 `powershell` 的 `command` 说明里，供模型对照）

| bash | PowerShell 5.1 |
|---|---|
| `a && b` | `a; b`（5.1 没有 `&&`；PS7 才支持） |
| `ls` / `cat` | `Get-ChildItem` / `Get-Content`（`ls`、`cat` 作为别名可用） |
| `rm -rf x` | `Remove-Item -Recurse -Force x` |
| `grep pat` | `Select-String pat` |
| `> /dev/null` | `> $null` |
| `export A=1` | `$env:A = '1'` |
| `which x` | `Get-Command x` |
| `x \| head -20` | `x \| Select-Object -First 20` |

## 三、权限与安全约束

### 围栏（`Scope`，工具层强制）

1. `path` 相对路径按工作区根展开，绝对路径必须落在根内；
2. `..` 先**词法归一**再判边界 —— `a/../../etc/passwd` 直接被拒；
3. 目标已存在时再 `canonicalize` 复查一次 —— 挡住"工作区内一个指向外面的符号链接"。

工作区根来自 `AppState::workspace_root()`：`NEO_WORKSPACE` 环境变量 →
工作区标签若恰好是存在的目录 → 进程当前目录。

### 风险等级与裁定（`Policy::decide`）

| 风险 | 工具 | 默认裁定 |
|---|---|---|
| `read` | `read_file` / `view_image` | 直接放行 |
| `open` | `open_file` | 直接放行（策略可关） |
| `write` | `write_file` / `edit_file` | **需用户确认** |
| `exec` | `powershell` / `bash` / `click` / `drag` | **需用户确认** |
| `read` | `screenshot` | 直接放行（它只看不动） |

三条附加规则：

- **只读模式**（输入卡的「只读」开关）：`write` / `exec` 一律拒绝，结果照实回灌给模型；
- **本会话自动批准**：确认框上的「本会话都允许」把 `auto_approve` 置真，**只在本次会话有效**；
- **参数不合法先于打扰用户**：未知参数、类型错误在裁定时就拒掉，不弹框、不执行。

### 体积与时间上限

| 项 | 上限 | 超限行为 |
|---|---|---|
| 读文件 | 1 MiB | `too_large` + 分段建议 |
| 写文件 | 1 MiB | `too_large` |
| 图片 | 8 MiB | `too_large` |
| 命令 stdout / stderr | 各 64 KiB | 截断 + `*_truncated = true` |
| 命令超时（前台） | 默认 **500 s** / 上限 1800 s | 杀进程 + `timeout` |
| 后台命令 | 不等、不捕获 | 立即返回 PID（`timeout_applies: false`） |
| 单条结果回灌给模型 | 24 000 字符 | 截断成合法 JSON，带 `truncated: true` 与 `head` |

---

### 屏幕坐标

屏幕工具的 `x`/`y` 一律是**虚拟桌面物理像素**，参数区间 `-32768…32768`
（容得下多显示器的负坐标）。越界分两层：先撞**参数区间**（`bad_args`，
说清合法区间），再撞**屏幕范围**（`bad_args`，说清屏幕范围）——
两者的 hint 都会指路到 `screenshot`。

## 四、返回结构

成功与失败**同一个外壳**，靠 `ok` 区分 —— 模型只需要学一种形状：

```json
{ "ok": true,  "tool": "read_file", "summary": "给人看的一行", "data": { } }
{ "ok": false, "tool": "edit_file", "error": { "kind": "not_unique", "message": "…", "hint": "…" } }
```

- `summary`：给人看（UI 卡片一行）；
- `data` / `error`：给模型看（结构化）；`hint` 一律是**可执行的下一步**，不写"请重试"；
- `ok` 对 `powershell` 的含义是"命令跑起来了"而非"命令成功" —— 命令成败看 `data.exit_code`，
  这样"工具没跑起来"（有 `error`）与"命令返回非零"（无 `error`）不会混为一谈。

### 错误分类（`ErrorKind`）

| kind | 含义 | 上层应有的反应 |
|---|---|---|
| `bad_arguments` | 参数缺失/类型/越界 | 不打扰用户，把可选值回给模型 |
| `not_found` | 目标不存在 | 让模型重新探路（先 ls / read） |
| `not_allowed` | 越界 / 只读 / 策略拒绝 | 提示权限，不重试同一动作 |
| `too_large` | 超过体积上限 | 按 hint 分段或换工具 |
| `not_unique` | `old_string` 命中多处 | 加长上下文或开 `replace_all` |
| `conflict` | 已存在且未允许覆盖 | 先读后写，或改 `edit_file` |
| `unsupported` | 不是 UTF-8 / 不是图片 / 是目录 | 换工具（`powershell` / `view_image`） |
| `timeout` | 命令超时 | 拆小命令或延长超时 |
| `io` | 读写/进程失败 | 报给用户 |
| `internal` | 工具自身问题（不该出现） | 报 bug |

---

## 五、接入与扩展

### 一轮 agent loop

```text
模型 → tool_calls（参数是分片下发的）
   ↓  neo_llm::assemble           按 index 聚合，缺 id 自动补
   ↓  Policy::decide              Allow / Confirm / Deny
   ↓  Confirm → 弹窗（preview + 原始参数）
   ↓  Allow
neo_tools::dispatch(scope, name, args) → Outcome
   ↓  neo_tools::to_model_message  截断到 24 000 字符
role=tool 消息回灌 → 模型续写（仍是同一个 start_real_stream 入口）
```

### 加一个新工具

1. 在 `crates/neo-tools/src/tools/` 新建 `xxx.rs`，声明 `PARAMS` / `preview` / `run`；
2. 在 `tools/mod.rs` 的 `REGISTRY` 里加一行（**顺序即推荐顺序**）；
3. 在 `tests/tools.rs` 加一条异常路径断言；
4. 在本文件补一节 —— **有测试盯着**（`every_tool_is_documented`），漏了会先炸。

若新工具与既有工具**共用参数或执行骨架**（像 `bash` 与 `powershell` 那样），
把共用部分抽成模块，并加一条测试断言两边的参数声明**逐项一致** ——
两份声明是最容易漂移的东西。

不需要改任何别的地方：

- JSON Schema 由 `Param` 生成 → `/chat/completions` 的 `tools` 自动带上；
- 系统提示词由 `prompt_catalog()` 生成 → 模型立刻知道有这个工具；
- 权限由 `Risk` 决定 → 确认框自动接管；
- UI 卡片按 `Tool::title` 渲染 → 中文名自动出现。

### 测试覆盖

| 范围 | 位置 | 数量 |
|---|---|---|
| 工具行为 + 异常路径 | `crates/neo-tools/tests/tools.rs` | 16 |
| 参数/schema/截断/错误 | `neo-tools/src/result.rs`、`scope.rs`、`policy.rs`、`tools/*` | 23 |
| 协议分片与聚合 | `neo-llm` 内联测试 | 9 |
| 工具轮、权限交互、异步执行 | `neo-app` 内联测试 | 9（含 2 张快照） |

快照：`docs/screens/14-tool-cards-1080p.png`（三种卡片形态）、
`docs/screens/15-tool-confirm-1080p.png`（确认弹窗）。

---

## 附：踩过的坑（都已在测试里钉住）

### `tools` 不能多包一层数组

请求体里 `tools` 是**扁平的函数数组**，每个元素一条工具：

```json
"tools": [ {"type":"function","function":{"name":"read_file", ...}}, ... ]
```

早期实现写成 `vec![neo_tools::openai_tools()]`，而 `openai_tools()` 本身已经返回数组，
于是线上请求体成了 `tools: [[…]]`，服务端直接返回：

```
422 Unprocessable Entity: Failed to deserialize the JSON body into the target type:
tools[0][0].function: invalid type: map, expected unit at line 1 column 1797
```

正解是用 `neo_tools::tool_declarations()`（每元素一条）。两条测试看着这件事：

- `wire_tools_are_a_flat_array_of_functions`：把请求体**反序列化回服务端期待的形状**
  （本地复刻服务端那一步），嵌套数组会在这里失败；
- `nested_tools_shape_is_rejected`：把错误写法本身钉住，作为可执行的解释。

### 消息形状的两个硬要求

- `function.arguments` 必须是**字符串**（字符串化的 JSON），不是对象；
- `role=tool` 必须带 `tool_call_id`，且能对上前面 `assistant.tool_calls[].id`。

`wire_tool_conversation_matches_server_shape` 用严格结构体同时校验这两点。

### 推理模型不带工具

`deepseek-reasoner`（R1 系）按官方文档**不支持 Function Calling**。
`start_real_stream` 按 `ModelDef::reasoning` 决定是否附带 `tools` ——
切到 R1 时自动不声明工具，而不是等服务端拒。
