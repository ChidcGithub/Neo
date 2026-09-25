# Neo

面向**教室大屏**（触控一体机，1080p / 4K，远距离观看）的 Windows 桌面 AI 助手。

说「Hi Neo」唤醒：全屏流光跑马灯贴着屏幕边缘亮起（对桌面做实时折射），
说话即转写，LLM 流式回复，可调用截屏、文件、shell 等本地工具。
平时退在系统托盘里常驻后台，等下一次唤醒。

视觉语言取自 DeepSeek Harness（`@deepseek-ai/dsh`）；大屏与触控适配、语音链路、
跑马灯特效与工具链均为 Neo 自研（独立实现，非上游移植）。

## 功能

- **语音唤醒**：复刻 livekit-wakeword 推理链路（mel → embedding → 唤醒词模型），全程本地 ONNX
- **语音转写**：Silero VAD 断句 + SenseVoice-Small(int8)，sherpa-onnx 进程内推理，无网络依赖
- **流光跑马灯**：全屏透明覆盖层（VCC edgeglow v2 移植），DX12 + DirectComposition
  透明交换链，30fps 抓屏喂给 shader 做实时边缘折射，随音频电平呼吸
- **对话**：OpenAI 兼容流式接口（默认 DeepSeek），Markdown / KaTeX 渲染，
  任意时刻可停止（含工具执行中）
- **工具调用**：文件读写编辑、PowerShell / Bash（内置便携 Git Bash）、截屏、
  UI 自动化（点击 / 拖拽 / UIA）、附件解析；危险动作先弹确认
- **大屏适配**：密度 × 距离的整屏缩放链路（视口高 / 1080 × 距离系数），
  48pt 触控命中下限，缩放参数在「显示设置」面板全透明可查

## 运行要求

- Windows 10 2004 (20H1) 或更高版本，支持 DX12 的 GPU
  （跑马灯的防截屏 API 自 2004 起提供；更低的 Win10 版本也能运行，
  会自动退回无折射的纯光环模式）
- Rust 1.95+

## 构建与运行

```bash
cargo run --release   # 大屏建议 release：4K 下 60fps
cargo test            # 单元测试 + 离屏渲染快照（输出到 docs/screens/）
```

### 模型与运行时资产

| 资产 | 位置 | 来源 |
|---|---|---|
| 唤醒模型（~3 MB） | `crates/neo-wake/assets/` | 已随仓库，无需处理 |
| STT 模型（~240 MB） | `crates/neo-stt/assets/`（已 gitignore） | sherpa-onnx 官方模型（hf-mirror 可下载）：`sense-voice/model.int8.onnx` + `tokens.txt`、`vad/silero_vad.onnx`；亦可用 `NEO_STT_MODEL_DIR` 环境变量指到别处 |
| Git Bash 便携运行时（~91 MB） | `runtime/`（已 gitignore） | `python tools/fetch_runtime.py`；缺失时 bash 工具不可用，PowerShell 工具不受影响 |

## 界面预览

| 空态 · 暗色 · 1080p | 生成态（可停止） |
|---|---|
| ![hero dark](docs/screens/01-hero-dark-1080p.png) | ![generating](docs/screens/08-generating-1080p.png) |

| 工具卡片 | 公式渲染（KaTeX 移植） |
|---|---|
| ![tool cards](docs/screens/14-tool-cards-1080p.png) | ![math](docs/screens/16-math-1080p.png) |

更多快照（亮色 / 4K 远距 / 工具确认弹窗 / 设置面板）见 `docs/screens/`。

## 工程结构

| crate | 职责 |
|---|---|
| `neo-app` | 主程序：eframe 界面、语音状态机、系统托盘、一轮对话的编排 |
| `neo-ui` | 自绘组件库（按钮 / 弹窗 / 列表 / 徽标 / 输入框…） |
| `neo-theme` | 设计系统：语义色板、度量与缩放链路、字体装配、超椭圆圆角 |
| `neo-llm` | OpenAI 兼容 `/chat/completions` 流式客户端，function calling 协议搬运 |
| `neo-tools` | 本地工具集：spec / 确认策略 / 执行 / 结果呈现 |
| `neo-wake` | 语音唤醒引擎（麦克风 → mel → embedding → 唤醒词打分） |
| `neo-stt` | 本地语音转写（VAD 断句 + SenseVoice 离线识别） |
| `neo-overlay` | 跑马灯覆盖层：独立窗口线程 + wgpu/DX12 渲染，与 egui 主界面解耦 |
| `neo-store` | 会话与消息持久化（SQLite） |

`vendor/egui_commonmark` 是打了本地补丁的 Markdown 渲染依赖
（`[patch.crates-io]` 接入）。设计规范与组件手册见 `docs/design-spec.md`、
`docs/design-kit.md`。

## 版本号

唯一来源是根 `Cargo.toml` 的 `[workspace.package].version`。
所有 crate 以 `version.workspace = true` 继承；界面内显示的版本号
（设置面板）由构建系统经 `env!("CARGO_PKG_VERSION")` 注入。改版本只动那一行。

## 三条设计原则

1. **视觉保真优先，适配通过「非视觉通道」完成**：上游的造型语言（圆角、
   按钮尺寸、字号节奏）不改；大屏适配只做整体等比放大和触控命中区扩展。
2. **缩放要可见**：最终倍率公式完整摊在设置面板里，现场排障第一眼定位。
3. **egui 只保留它做得最好的事**：文本编辑与滚动裁剪用原生，
   其余全部自绘 —— 硬拼默认控件的结果是既不像上游、又丢掉可用性。

## License

MIT
