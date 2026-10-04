# tools —— 上游素材抽取 + 随包运行时与发布检查

这里保留上游素材抽取、运行时获取和发行包辅助工具。
当前 UI 图标使用 Phosphor 字体；上游 bundle 抽取仅供参考，不是构建必需步骤。

## 上游包从哪来

`@deepseek-ai/dsh` 及其 `dsh-client-ui-*` 子包在 npm 上是公开的：

```bash
npm view @deepseek-ai/dsh versions        # 或查镜像 registry.npmmirror.com
```

Neo 目前对齐的版本与用到的包：

| 包 | 版本 | 用途 |
|---|---|---|
| `@deepseek-ai/dsh-client-ui-theme` | `0.0.1-rc.1` | 设计 token（`lib/styles/design-platform.css`） |
| `@deepseek-ai/dsh-client-ui-primitives` | `0.0.1-rc.1` | **图标几何**（`lib/index.js` 里的 React 组件）+ 鲸鱼标志 |
| `@deepseek-ai/dsh-client-ui-conversation` | `0.0.1-rc.1` | 会话骨架 / 输入卡 / 气泡 |
| `@deepseek-ai/dsh-client-ui-sidebar` | `0.0.1-rc.1` | 侧栏 |
| `@deepseek-ai/dsh-client-ui-layout` | `0.0.1-rc.1` | 三栏 AppFrame |

本地参考副本放在 `D:\My things\Learn\高二\Harness\<包名>@<版本>\`，
**不进 git**（体积大、且是可重新下载的只读素材）。

> 子包发布的是 **bundle**（`lib/index.js`），没有 TSX 源码。图标定义长这样：
> `const IconDarkOutline16 = ({ size = 16, className }) => jsx("svg", {...children: jsx("path", { d, fill }) })`，
> 所以脚本是"从 bundle 里按模式抓结构"，不是解析真正的源码。改上游版本后
> **必须重跑 + 看一次产物 diff**，别假设格式没变。

## 现存的素材抽取工具

```bash
# 从上游 bundle 抽取几何，输出到脚本 OUT 指定的 icons.json
python tools/extract_icons.py
```

`extract_icons.py` 的 `SRC` / `OUT` 是指向本机参考副本的**绝对路径**，
换机器需先调整。脚本保留 SVG 的 `transform` 字段，不负责烘焙变换或生成 Rust 常量。
旧 SVG 图标生成流程已退役；当前映射在 `crates/neo-ui/src/icons.rs`，
字体由 `neo-theme` 内嵌。不要把参考 JSON 当作当前 UI 的自动生成输入。

## 为什么不能只抄"看起来像"的形状

上游图标是 **16/14 栅格上的填充路径**：线稿的观感由**两条反向绕行的轮廓**
（外轮廓 + 内轮廓）填出来，不是一笔描边。手绘同类线条时，粗细、端点、圆角
必然差一截 —— 而且改不动：上游换个尺寸，你就得重画一遍。

对抽取出的 SVG 做比对时，仍需保留填充规则与变换，不能只比较路径字符串。
这不代表当前 Phosphor 字体图标仍走旧的逐图标 SVG 纹理管线。

## 校验手法

当前图标映射可用现有纯逻辑测试检查（不生成截图）：

```bash
cargo test --locked -p neo-ui --lib every_icon_maps_to_a_single_pua_char
```

该测试只验证每个映射是单个私有使用区字符，不证明字形视觉正确或完成第三方许可审查。

---

## `fetch_runtime.py` —— 随包的 Git Bash 运行时

`bash` 工具要一个类 Unix 环境。一体机上不一定装了 Git，所以**运行时随包提供**：

```bash
python tools/fetch_runtime.py                 # 默认走最快镜像
python tools/fetch_runtime.py --mirror github # 只用官方源
python tools/fetch_runtime.py --force         # 重下
```

运行时下载缓存落 `.cache/runtime/gitbash/`（**不进版本库**，不是对应源码交付）；发行时仅复制该目录到
`dist/neo/runtime/gitbash/`，发行路径保持不变。旧根目录 `runtime/` 不再作为下载目的地，
本脚本不会自动迁移或删除旧缓存。

下载有大小/时间预算、解压路径检查和暂存验证；这些**不等于完整性 hash pin**。
当前发布工作流未固定 MinGit 版本，STT 与 MinGit 下载均仍待补充可信预期哈希并审计
第三方 LICENSE/notice；指定 `--version` 或使用 HTTPS 也不能代替完整性校验。

两个实测踩到的坑，都写进脚本了：

1. **GitHub 的 release 资产在国内下不动**（直接 `TimeoutError`）。
   所以资产名从 GitHub API 问（那个接口很快），**字节从 npmmirror 取**；
   两个源都试，全失败才报错。
2. **镜像上的资产名和 tag 不一样**：tag 是 `v2.55.0.windows.5`，
   资产叫 `MinGit-2.55.0.5-64-bit.zip`（少一段 `.windows`）。
   所以必须先问 API 拿文件名再拼地址 —— 从 tag 拼出来的地址是 404。

## 离线许可审计

`python tools/audit_licenses.py`（Python 3.11+）使用 `Cargo.lock`、离线 Cargo 元数据和本地缓存，
不构建、不联网、不选择许可证或作出合规结论。相同输入可重现生成：

- `docs-pri/licenses/cargo-inventory.json`、`cargo-inventory.md`：私有详细审计清单，不上传、不随包。
- `docs/licenses/cargo-notices.txt`：公开的第三方完整许可/notice 汇编，保留原始文本字节，继续随包。
- `target/license-audit/`：原始命令证据，不随包。

素材、运行时详细审计及详细 README 备份也放在 `docs-pri/licenses/`；
公开 `docs/licenses/` 保留简短索引和第三方完整法律文本。生成器不搬迁或删除旧报告；
迁移旧工作区时须先将公开目录中的详细审计移入私有目录，避免递归装配将其带入发行包。

## 发行辅助工具

- `make_installer_art.py`：从 `crates/neo-app/src/brand/whale_path.rs` 生成 NSIS
  图标、位图及预览，依赖 Pillow；工作流使用
  `python tools/make_installer_art.py --out target/package/installer-art`（也是默认输出位置）。
- `installer.nsi`：每用户免提权安装器；最低 Windows 10 2004 / build 19041，
  仅原生 AMD64，不支持 ARM64 仿真。升级保留完整旧目录备份，卸载保留非空资源目录；
  旧版无可信卸载残留记录时应先将原目录改名保留，再安装到原路径。
  新 `resources/`、`docs/`、`runtime/` 和旧 `assets/`、`assets-stt/` 均只清理已知空目录，
  `resources/lang/` 同样只清理空目录；非空资源/文档目录及自定义文件保留；
  根 `LICENSE`、`NOTICE` 和旧根 `README.md` 按已知文件删除；
  `docs/licenses/` 及其 `assets/`、`runtime/` 仅清理空目录，非空法律资料及用户文件不递归删除。
  NSIS 许可证页读取仓库根 `LICENSE`（Neo Apache-2.0），其余安装交互保持不变。
  详见[发行使用说明](../docs/distribution/README.md)。
- `check_release.py`：检查必需载荷非空，包含根 `LICENSE`、`NOTICE`，以及
  `docs/licenses/README.md`（公开索引）和 `cargo-notices.txt`；缺失、空文件或同名目录都会失败。
  私有的 Cargo 清单、素材及运行时审计不属于必需发行载荷。
  **法律文件检查仅证明载荷完整，不证明许可合规、notice 充分或再分发已获授权。**
  两份语言资源必须是 UTF-8 JSON `str:str` 对象，
  key 集合非空、key 非空且两份一致，英文翻译不得留空，不接受其他 `.lang` 语言文件。
  审计 `neo.exe`、`runtime/onnx/*.dll` 及递归导入的
  白名单 CRT 的 x64 PE；**不覆盖完整第三方 DLL 闭包、MinGit PE/依赖或模型有效性**。
  `--redist-dir` 仅接受获授权 Visual Studio 的 x64 CRT Redist 目录，不从 System32
  收集 DLL。app-local CRT 的安全更新须由发行方更新载荷并重发 Neo。

发行装配契约（`.github/workflows/release.yml`）：

- `dist/neo/neo.exe`、`LICENSE`（Neo Apache-2.0）、`NOTICE`、实际导入的白名单 MSVC CRT DLL 保留在发行根。
- 仅公开的 `docs/licenses/` 整个目录递归复制到发行 `docs/licenses/`，包含简短索引、
  第三方 notices 及 `assets/`、`runtime/` 下的完整法律文件；不依赖或复制 `docs-pri/` 详细审计或私有缓存。
  Neo 的 Apache-2.0 不替代第三方组件各自的许可证。
- `crates/neo-wake/assets/` 只取三个正式 ONNX 模型到 `resources/models/wake/`；
  同源 DLL 单独复制到 `runtime/onnx/`，不混入模型目录。
- STT 下载包、解压目录及待打包模型全部放 `.cache/stt/`；只将 `models/` 复制为
  `resources/models/stt/`，不把下载包或解压暂存带入发行包。
- 源码 `resources/lang/` 仅将 `zh-CN.lang`、`en-US.lang` 原样复制到发行 `resources/lang/`，
  不带入开发说明或其他文件；任一缺失都会使装配及发行检查失败。
- 开发用根 `README.md` 不随包；`docs/distribution/README.md` 复制为发行 `docs/README.md`。
- 安装器和便携包使用同一个 `dist/neo/` 载荷；美术中间产物在 `target/package/installer-art/`。

不下载、不安装的定向检查：

```bash
python -B -m unittest tools.test_audit_licenses tools.test_installer tools.test_check_release tools.test_fetch_runtime tools.test_release_workflow -v
```

测试使用临时载荷实际执行工作流的 PowerShell 装配片段，检查法律文件及嵌套目录逐字节随包、
开发根 README、四份私有审计报告、详细 README 备份和私有缓存不随包，
并确认必需公开法律文件缺失会在装配或随后的载荷检查失败。生成器测试同时检查输出分流及逐字节可重现性。
有 NSIS 时仅隔离编译，不执行生成的安装器；这些测试不等同于实际安装验收。

公开法律文件索引见 [`docs/licenses/README.md`](../docs/licenses/README.md)；详细审计留在
`docs-pri/licenses/`，不上传、不随包。**未解决的法务阻断项必须在公开发行前解决或取得明确授权**。
本次不新增依据法务结论自动阻断现有 CI 的策略；CI 通过仅表示这些技术检查通过，
不能代替人工发布批准。必须随发行提供的第三方许可证、notice 及其他法定义务材料不能仅留在私有目录。
公开使用说明见 [`docs/distribution/README.md`](../docs/distribution/README.md)。
