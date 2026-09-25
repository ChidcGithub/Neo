# 研究：让 agent 感知屏幕上所有可点的东西

> 2026-09-24 · 为 `screen_elements` 工具做的选型研究与本机验证。
> 结论先行：**UIA 树为主 + SoM 编号 + `element_id` 引用点击**，视觉兜底走现有截图。

## 1. 问题

Neo 已有 `screenshot` / `click` / `drag`（虚拟桌面**物理像素**坐标系）。
但模型现在只能对着截图**猜坐标** —— 调研里有个反直觉的实测（GPT-4o-mini 对照实验）：
agent 能正确说出目标元素的名字，却始终点击 `(0,0)`。**感知的瓶颈不在"看不见"，在"把语义映射到精确动作"**。
所以需要一个算法把"屏幕上有什么可点"变成结构化数据，并把动作从"像素"升级成"引用"。

## 2. 业界方案对比

| 方案 | 原理 | 精度 | 覆盖面 | 成本 | 结论 |
|---|---|---|---|---|---|
| **UIA 辅助功能树** | OS 维护的控件树（Name/Type/BBox） | 高（官方数据） | 原生应用 + **Chromium/Electron 良好**；游戏/自绘 canvas 无 | **零**（系统 API，~500ms） | **主力** ✓ |
| **OmniParser v2**（微软） | YOLOv8 检测可交互区域 + Florence-2 图标释义 | ScreenSpot-Pro 39.5% | 任意像素（含游戏/远程桌面） | 需 GPU 推理或云服务 | 暂不用：一体机无 GPU；作为未来可选云服务 |
| **SoM 视觉标注** | 截图上画编号框，模型答「点击 7」 | 与检测器同 | 同上 | 仅画图成本 | **采纳其思想**（编号 + 引用），不采其分割模型 |
| **DOM 树**（browser-use 系） | 读浏览器 DOM | 高 | 仅浏览器 | 低 | 不做：UIA 已覆盖 Chromium 内容 |

微软自家 agent 框架 **UFO** 的推荐配置就是混合：`CONTROL_BACKEND: ["uia", "omniparser"]` —— UIA 优先、视觉兜底。行业已经把"树优先"验证过了。

## 3. 本机验证（2026-09-24，Win11 · 200% 缩放 · 3200×2000 虚拟桌面）

Python `uiautomation 2.0.29` 实测：

| 指标 | 数值 |
|---|---|
| 全屏可交互元素 | **144 个**（任务栏 16 + 资源管理器 69 + 桌面图标 17 + WorkBuddy 56 …） |
| 全屏枚举耗时 | **~540 ms**（含每窗口 DFS） |
| 元素质量 | **96% 带可读名字**（「开始」「共享」「此电脑」「发现应用」…），噪音仅 4% 无名 + 3 个滚动条子按钮 |
| 坐标契约 | `BoundingRectangle` = **物理像素**（任务栏 y=1904、桌面 3200×2000 —— 与 `click`/`drag` 同一坐标系 ✓） |
| 打包成本 | 紧凑 JSON **6.3K 字符 ≈ 2.5K tokens**（全屏 144 元素） |
| Electron/Chromium | WorkBuddy 自身 56 个元素、52 个带名 ✓（UIA 对 Chromium 内容支持良好） |

**本机踩出的设计教训（重要）**：

> 第一次枚举时 WorkBuddy 整个窗口丢失。根因：在**祖先节点**上用 `IsOffscreen`
> 做了整树剪枝，而 Chromium 对该属性的报告不可靠（窗口非激活时整棵树被判 offscreen）。
> **正确做法：不在祖先剪枝，过滤放到叶子级 —— 用 bbox 与虚拟桌面求交来判断可见性。**

## 4. 推荐算法

```
screen_elements(window?, annotate?)
   │
   ├─ 1. DPI 感知（复用 screen.rs 的 PER_MONITOR_AWARE_V2 基建）
   ├─ 2. 枚举顶层窗口（DWM DWMWA_CLOAKED 剔除隐藏窗口，比 UIA IsOffscreen 可靠）
   ├─ 3. 每窗口 UIA DFS（预算制：每窗 ≤600 元素、深度 ≤14、总预算 ≤800）
   ├─ 4. 叶子级过滤：
   │      · 可交互 ControlType（Button/MenuItem/Hyperlink/Edit/… 16 类）
   │      · bbox 与虚拟桌面相交、面积 ≥ 阈值
   │      · 丢无名元素 + 滚动条子按钮（名字特征黑名单）
   │      · 去重（同 bbox 同名）
   ├─ 5. SoM 编号：按 从上到下、从左到右 排序 → id = 1..N
   └─ 6. 打包（同一 Outcome 两路输出）：
          · 文本：[{id, role, name, x, y, w, h}] 紧凑 JSON（≤24K 字符，超了按序截断并提示缩小范围）
          · 视觉：标注截图（画编号框）→ 复用 Outcome.images 的多模态通道
```

**配套升级（这才是真正的收益）**：`click` / `drag` 新增可选参数 `element_id` ——
按编号查缓存的元素 bbox 中心去点。模型的工作从「看图算像素」变成「引用编号」，
把连续视觉定位**离散化**；`x/y` 直连模式保留，作为 UIA 覆盖不到时的兜底。

## 5. Rust 实现要点（实现阶段的备忘）

- UIA COM：用 `windows` crate（非 `windows-sys`，COM 友好封装）：
  `CoCreateInstance(CUIAutomation)` → `GetRootElement` → `ControlViewWalker`；
  用 **Cached** 属性（`CurrentName` 逐元素跨 COM 调用会慢一个量级）
- 每元素字段：`id / role / name(≤48 字符) / rect[x,y,w,h]`，缓存到 `AppState`（带时间戳，供 `element_id` 点击查表）
- 枚举已在独立线程执行（现有工具骨架 `shell.rs` 同款），不阻塞渲染
- 截图标注：在 `screenshot` 的 PNG 上画 1.5px 框 + 半透明底 + 编号（复用 `neo_theme` 的 squircle/文字基建）

## 6. 风险与兜底

| 风险 | 缓解 |
|---|---|
| 游戏 / 自绘 canvas 没有 UIA 树 | `x/y` 直连点击保留；未来可插 OmniParser 云服务（对 Neo 是可选增强，非依赖） |
| 管理员权限窗口：UIPI 静默丢弃输入 | 已有处理（`click` 返回 `not_allowed`） |
| 元素爆炸（超大列表） | 预算制 + 按序截断 + `window` 参数缩小范围 |
| UIA 树挂起（个别窗口响应慢） | 每窗口预算制 + 线程超时兜底（与 shell 工具同策略） |

## 7. 一句话结论

**不需要训练任何模型**：Windows 的 UIA 树已经免费提供了 96% 带名、
物理像素坐标、2.5K token 打包成本的"可点元素清单"；算法的全部工作
是**过滤、编号、打包**，以及把点击从"像素"升级成"引用编号"。
视觉模型（OmniParser）留给 UIA 照不到的角落，作为未来的可选增强。
