# Neo 设计规范

> 视觉语言的**唯一来源**是 DeepSeek Harness（`@deepseek-ai/dsh`，v0.1.5-rc.2）。
> 本文记录「取了哪些值、来自哪个文件、为什么这么取」，便于日后对照上游升级。
>
> 上游仓库：`https://github.com/deepseek-ai/deepseek-harness`（`master`）

---

## 一、取值来源清单

| Neo 里的位置 | 上游文件 | 说明 |
|---|---|---|
| `neo-theme/src/palette.rs` | `packages/client/ui-theme/src/styles/design-platform.css` | 全部 `--dsw-static-*` 与 `--dsw-alias-*` 语义 token |
| `neo-theme/src/metrics.rs` | `packages/client/ui-conversation/src/client/skeleton/InputBar.module.css`<br>`.../HeroShell.module.css`<br>`.../ConversationRoot.module.css` | 输入卡、hero、整体骨架的几何与字号 |
| `neo-theme/src/squircle.rs` | `packages/client/ui-theme/src/styles/corner-shape.css` | `corner-shape: superellipse(1.5)` |
| `neo-theme/src/fonts.rs` | `packages/client/ui-theme/src/styles/base.css` | 字体栈与「等宽栈不接裸 monospace」的取舍 |
| `neo-app/src/brand/whale_path.rs` | `packages/client/ui-primitives/src/FishLogo.tsx` | 鲸鱼标志的 viewBox 与路径数据 |

---

## 二、颜色

### 2.1 静态色阶（节选，完整见 `palette.rs`）

| 名称 | 值 | 用途 |
|---|---|---|
| `deepseek-100` | `#E4EDFD` | 亮色主题的强调弱底 |
| `deepseek-400` | `#679EFE` | **暗色主题的强调色**、链接 |
| `deepseek-500` | `#4176E6` | **亮色主题的强调色** |
| `deepseek-800` | `#34415B` | 暗色主题的强调弱底 |
| `neutral-bluish-950` | `#151517` | 暗色主题背景 |
| `neutral-bluish-875` | `#232324` | 暗色 layer-1 |
| `neutral-bluish-850` | `#2C2C2E` | 暗色 layer-2 / 输入卡表面 / 气泡 |
| `neutral-bluish-800` | `#353638` | 暗色 layer-3 / 圆形控件底 |

### 2.2 语义别名（Neo 直接引用，不做二次映射）

| Neo 字段 | 上游 token | 暗色 | 亮色 |
|---|---|---|---|
| `bg_base` | `--dsw-alias-bg-base` | `#151517` | `#FFFFFF` |
| `sidebar_fill` | `--dsw-specific-sidebar-fill` | `#1B1B1C` | `#FAFAFB` |
| `input_surface` | `--dsw-specific-input-major` | `#2C2C2E` | `#FFFFFF` |
| `selector` | `--dsw-specific-selector` | `#353638` | `#F5F6F7` |
| `label_primary` | `--dsw-alias-label-primary` | `#F9FAFB` | `#0F1115` |
| `label_secondary` | `--dsw-alias-label-secondary` | `#CFD3D6` | `#61666B` |
| `label_tertiary` | `--dsw-alias-label-tertiary` | `#ADB2B8` | `#81858C` |
| `label_caption` | `--dsw-alias-label-caption` | `#81858C` | `#ADB2B8` |
| `accent` | `--dsw-alias-button-info-fill` | `#679EFE` | `#4176E6` |
| `border_l1…l4` | `--dsw-alias-border-l1…l4` | 白 6% → 20% | 黑 4% → 16% |
| `hover` | `--dsw-alias-interactive-bg-hover` | 白 8% | `rgba(38,49,72,.06)` |
| `nav_active` | `--dsw-specific-sidebar-nav-item-active` | `#43454A` | `#EBEEF2` |

> **易踩的坑**：`--dsw-alias-brand-primary` 在本套规范里解析为**墨色**（亮色主题是
> `#0F1115`，暗色主题是 `#F9FAFB`），不是蓝色。真正的强调蓝是
> `--dsw-alias-button-info-fill`。Neo 里分别叫 `brand_ink` 与 `accent`。

### 2.3 明暗策略

教室一体机通常在明亮环境下使用，因此**亮色主题是一等公民**（不是暗色的附属），
`Palette::LIGHT` 的每个值都独立取自上游，不是由暗色调推算。

---

## 三、度量

### 3.1 基准值（`scale = 1.0`，即上游的 1:1）

| 项 | 值 | 上游出处 |
|---|---|---|
| 输入卡圆角 | `22` | `InputBar .card { border-radius: 22px }` |
| 输入卡内间距 | `gap: 12` / `padding-top: 8` | 同上 |
| 文本区内边距 | `4px 8px 0 14px` | `.input` |
| 文本行高 | `24` | `.card { line-height: 24px }` |
| 文本区地板 | hero `52` / 停靠 `36` | `.hero .input { min-height: 52px }` |
| 文本区上限 | `336` | `--dsh-composer-text-max-height` |
| 工具栏行 | `padding: 2px 8px 6px; gap: 12` | `.row` |
| 附件圆钮 | `28` 直径 | `.add` |
| 发送圆钮 | `34` 直径 | `.primary` |
| chip 高度 | `28`，圆角 `8`，13/20 wt500 | `.select` |
| hero 标题 | `26 / 32`, wt500 | `.headline` |
| 鲸鱼标志 | 宽 `34`（viewBox 23.16 : 17.04） | figma `34:10412` |
| 内容列宽 | `clamp(680, 64%列宽, 920)` | `--dsh-chat-content-width` |
| 输入卡宽 | 内容列 `+ 32` | `--dsh-composer-card-max-width` |
| 侧向留白 | `16` | `--dsh-composer-side-clearance` |
| 顶栏内边距 | `10px 28px 0 20px` | `ConversationRoot` 头部 |

### 3.2 Neo 的放大链路

```
像素密度因子 = 视口逻辑高 / 1080
距离系数     = 近距 1.0 · 教室 1.25 · 远距 1.6
最终倍率     = clamp(像素密度 × 距离系数, 0.85, 2.8)
```

所有基准值乘最终倍率。这样做的目的：**让界面占据的视角（而非像素数）保持稳定**。

| 场景 | 视口逻辑高 | 距离 | 倍率 | 正文 |
|---|---|---|---|---|
| 1080p 近距 | 1080 | 近距 | 1.00 | 14pt |
| 1080p 一体机 | 1080 | 教室 | 1.25 | 17.5pt |
| 4K 一体机 | 2160 | 教室 | 2.50 | 35pt |
| 4K 报告厅 | 2160 | 远距 | 2.80（截顶） | 39.2pt |

> 收敛上界 `2.8` 是必要的：4K + 远距的理论值是 `2.0 × 1.6 = 3.2`，
> 会把输入卡撑到 3000px 以上，一屏放不下一条完整消息。

列宽不按固定百分比收缩，而是：

```
内容列宽 = min(920 × scale, 可用宽度 − 4 × 16 × scale)
输入卡宽 = 内容列宽 + 32 × scale
```

即「保留上游 920px 的扫读上界」，同时在窄屏上也不会把列宽压到比可用空间还小。

内容列在**主区中轴居中**（`x_off = (可用宽度 − 内容列宽) / 2`），
与输入卡共用同一条左基线 —— 上游同样是居中而非贴侧栏。

> 实现坑：egui 的 `ui.add_space` 只作用于**当前布局方向**，在纵向布局里放它不会
> 产生横向偏移。内容列右移必须包一层 `Layout::left_to_right`，并在内层
> `set_max_width(内容列宽)`；只做偏移不收窄的话，Markdown 的 `Frame`（代码块）
> 会按 `available_width` 撑到屏幕边缘（见 `ui/conversation.rs::draw_messages`）。

### 3.3 触控适配

上游的 28 / 34px 圆钮在手指下太小。Neo **不放大视觉尺寸**（那会破坏造型比例），
而是把命中区单独扩到 `max(视觉尺寸, 48pt × scale)`：

```rust
fn hit_target(&self, visual: f32) -> f32 { visual.max(self.s(TOUCH_TARGET_MIN)) }
```

在 4K 一体机上视觉尺寸本身已达 70~85px，扩边自动失效；在 1080p 近距下
（视觉 28 / 命中 48）才真正起作用。

---

## 四、圆角：超椭圆而非圆弧

上游用 `corner-shape: superellipse(1.5)` —— 介于正圆（`K=1`）与 squircle（`K=2`）
之间的曲率，比普通圆角"更撑"，观感更接近原生应用。

CSS 的 `superellipse(K)` 落在 `|x|^p + |y|^p = 1` 这一族曲线上，其中 `p = 2K`：

| K | p | 形状 |
|---|---|---|
| 0.5 | 1 | 直切角 |
| 1 | 2 | 正圆弧 |
| **1.5** | **3** | **本项目** |
| 2 | 4 | squircle |

参数化后 `x = r·|cosθ|^(1/K)`、`y = r·|sinθ|^(1/K)`，每角采样 12 段。

egui 原生只有正圆圆角，因此 `neo-theme::squircle` 自行生成多边形路径交给
`PathShape` 填充，并封装成 `SquirclePaint` 扩展 trait。

---

## 五、字体

上游字体栈：

```css
--dsw-font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC',
                   'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif;
--ds-font-family-code: 'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas, 'Liberation Mono',
                       Menlo, Courier, 'PingFang SC', 'Microsoft YaHei';
```

上游注释里有一条值得照抄的取舍：**等宽栈故意不接裸 `monospace`**，
否则 Windows 下的中文会掉到 SimSun（宋体）。Neo 的等宽链同样是
`Consolas → Microsoft YaHei`。

Neo 在 Windows 上的实际落点：

| 族 | 文件 |
|---|---|
| Proportional | `segoeui.ttf` → `msyh.ttc` |
| `neo-bold`（wt500 以上） | `seguibl.ttf` → `msyhbd.ttc` |
| `neo-mono` | `consola.ttf` → `msyh.ttc` |

egui 内置的 `NotoEmoji-Regular` / `emoji-icon-font` 保留在链尾，保证 emoji 不变成豆腐块。

---

## 六、动效

上游曲线与时长：

```css
--ds-ease-in-out: cubic-bezier(0.4, 0, 0.2, 1);
--ds-transition-duration: 0.2s;      /* fast 0.1s · slow 0.3s */
```

egui 没有逐帧插值的阴影，也没有 CSS 的关键帧。Neo 用两样东西拼出同等观感：

| 上游 | Neo 的实现 |
|---|---|
| `transition: 100ms`（悬停态） | `ctx.animate_value_with_time(id, target, 0.1)`，见 `ui::ease` |
| 半透明叠加（hover 底色） | `Color32::gamma_multiply(k)` —— 预乘色等比缩放 RGB 与 alpha 就是正确的降透明度，叠画在静息态之上 |
| `hero-fish-swim`（1.6s 循环，±0.9px + ±4°） | `ctx.time()` 相位 + 水平/垂直位移（egui 的 painter 不支持逐形状旋转），幅度同为 1pt 量级 |
| `.pending` 呼吸点 | 三个相位错开 0.18s 的正弦脉动点，1s 一轮 |
| 卡片 hover 抬升 + 投影浮现 | 上浮 2pt + 投影颜色随 `lift` 淡入 |

两个工程细节：

- **持续动画要显式保帧率**：`animate_value_with_time` 只在数值未收敛时请求重绘，
  循环动画（游动、脉动）需要自己 `request_repaint_after`。
- **渐隐遮罩用顶点色网格**：`Shape::mesh` + 逐行顶点色（透明 → 背景色），
  对应上游"the bottom gradient mask is owned by the chat scroller"。

---

## 七、品牌标志

鲸鱼路径**原样取自上游** `FishLogo.tsx`（3448 字符，`M/C/L/Z` 共 4 条子路径：
身体 + 鳍挖空 + 双眼）。`neo-app/src/brand` 做了：

1. 解析 SVG 路径，三次贝塞尔按 24 段展平；
2. 按**奇偶填充规则**做扫描线光栅化（512px 宽，垂直 4× 超采样）→ 白色带 alpha 纹理；
3. 绘制时用 `tint` 上色，一份纹理适配两个主题。

> 上游 SVG 用默认的非零填充。两者只在子路径互相重叠时分歧 ——
> 当前几何并不重叠，`even_odd_matches_nonzero_on_this_geometry` 这条测试
> 把该等价性钉住了：**若上游几何变更导致两者分歧，测试会失败**，
> 提示需要改用非零规则。

---

## 八、与上游的差异总表

| 项 | 上游 | Neo | 理由 |
|---|---|---|---|
| 缩放 | 固定 1×（浏览器 DPR 处理） | 密度 × 距离，可调 | 教室大屏观看距离差异极大 |
| 触控命中区 | 等于视觉尺寸 | 扩到 48pt 下限 | 手指 vs 鼠标 |
| 默认主题 | 跟随系统 | 暗色（可一键切） | 一体机安装环境固定 |
| 场景卡 | 无 | 4 张课堂场景入口 | 老师需要"点一下就开工" |
| 显示设置 | 分散在设置页 | 侧栏底部 + 诊断面板 | 现场排障要快 |
| 输入卡上限 | 336px | 336 × scale | 大屏下多写几行不必滚动 |

---

## 九、验证方式

设计规范如果只停留在"抄了一遍数值"，就容易在实现细节上走样。
Neo 用**离屏渲染快照**把规范钉在像素上：

```bash
cd Neo && cargo test -p neo-app -- --nocapture
# → Neo/docs/screens/*.png
```

`egui_kittest` 起一个无窗口的 wgpu 渲染器，走**与真机完全相同**的字体装配与主题构建
路径（`NeoApp::install` 只依赖 `egui::Context`），输出 7 张覆盖矩阵：

| 文件 | 场景 |
|---|---|
| `01-hero-dark-1080p` | 1080p 教室档 · 空态 |
| `02-hero-light-1080p` | 同上 · 亮色主题 |
| `03-hero-dark-4k-far` | 4K 远距档（倍率截顶 2.8） |
| `04-conversation-dark-1080p` | 对话态（含顶栏与消息流） |
| `05-conversation-light-1080p` | 对话态 · 亮色 |
| `06-display-panel-1080p` | 显示设置面板 |
| `07-hero-dark-1080p-near` | 近距档（倍率 1.0，验证触控扩边） |
| `08-generating-1080p` | 生成态：脉动点 + 停止按钮 |
| `09-readonly-notice-1080p` | 只读模式的提示条 |
| `10-markdown-1080p` | Markdown 渲染：标题 / 列表 / 代码块 / 引用 / 行内样式 |
| `11-settings-appearance-1080p` | 设置 · 外观页（亮色） |
| `12-session-actions-1080p` | 侧栏会话：恒显动作按钮 + 删除确认态 |
| `13-design-kit-1080p` | 组件陈列室（`docs/design-kit.md` 的配图） |
| `14-tool-cards-1080p` | 工具调用行：成功 / 失败 / 非零退出码 |
| `15-tool-confirm-1080p` | 工具权限确认弹窗 |
| `16-math-1080p` | LaTeX 公式：行内 `$…$` 与行间 `$$…$$` |
| `17-reasoning-1080p` | 思考过程（长文本换行） |
| `18-settings-model-1080p` | 设置 · 模型页（含思考强度） |
| `19-list-numbering-1080p` | 有序列表序号：递增 / 非 1 起点 / 两位数 / 嵌套 |
| `20-no-model-notice-1080p` | 还没有模型时的提示条（空列表最常见的现场） |
| `21-settings-model-empty-1080p` | 设置 · 模型页的空现场（还没从模型商拉到列表） |

### 这套办法实际抓到的问题

首轮渲染暴露了 5 个纯靠读代码看不出来的缺陷，全部已修（另有 1 个后续轮次发现的重复消息 bug）：

| 现象 | 根因 |
|---|---|
| 「只读」chip 退化成孤零零一个省略号 | `inner_w = 31.0624` 与 `text_w = 31.0625` 差 1e-4 —— 宽度由「总宽减内边距」算出，末位舍入使"恰好等宽"被判为放不下。修法：测量加 0.5px 容差，并把 chip 几何收进 `Metrics::chip_width`，让布局与绘制共用同一个式子 |
| 发送按钮是圆角方块而非正圆 | `superellipse(1.5)` 会把半径接近半边的形状压成方形。上游对此有明确规定（完全圆形一律配 `corner-shape: round`），Neo 的圆形控件因此改走 `circle_filled` |
| 显示面板里的分段控件跑到窗口左下角 | 面板背景画在给定矩形上，内部控件却用**父级 `Ui` 的游标**定位。修法：内容整体放进 `at(ui, inner, …)` 子 `Ui`，并改用分配式布局 |
| 窗口四周有一圈未绘制的黑边 | eframe 交给 `App::ui` 的根 `Ui` 比屏幕内缩 8pt，而背景只铺了 `max_rect`。修法：背景与遮罩铺满 `ctx.content_rect()`，布局仍走 `max_rect` |

此外还有一处排版缺陷：顶栏的「新对话」按钮与档位控件用了各自的锚点定位，
在 chip 文字变长时会重叠。改为顺序摆放（先档位、再按钮向左排）。

> 反面教材值得记下来：以上 5 个问题里，有 4 个是"读代码时看起来完全合理"的。
> 图形界面的一致性只能靠**看**来保证 —— 规范和实现之间必须有一个把像素渲出来的环节。


---

## 2026-09-21 补充：组件层落地

本文档描述的 token 体系已沉淀为**组件库 `crates/neo-ui`**（按钮族 / 图标按钮 /
Chip / 分段 / 卡片 / 面板 / 模态 / 表单 / 列表行 / 徽标 / 反馈态），
页面层不再手搓控件。接口速查见 **`docs/design-kit.md`**，
全部控件的一页速览见 `docs/screens/13-design-kit-1080p.png`
（`cargo test design_kit` 刷新）。

---

## 2026-09-21 补充：新月图标与设置入口归位

### 新月（`Icon::Moon`）为什么是坏的

旧实现是两段**各自摆角**的圆弧：外圆 `(12.4,12) r7.6` 走 −58°→238°，
内圆 `(6.6,12) r7.0` 走 −96°→96°。问题不在参数而在方法：

- 两段弧的端点不在同一个点上 —— 内弧比外弧的"尖角"多走 24°，
  于是两条线在犄角处**交叉出线头**，在顶部又留出一个 64° 的**缺口**；
- 摆角是手写常量，改一个半径就得重新凑角度，改不动也不敢改。

改成**双圆相减**：外圆与内圆的两个交点就是月牙的两个尖角，
两段弧都从交点起止（外弧走右侧长弧 246°、内弧走右侧短弧 146°），
尖端自然收成尖、没有线头也没有缺口。几何参数只有一个含义明确的四元组：

```rust
Icon::Moon => crescent(painter, rect, color, w,
    ((10.19, 12.0), 8.0),   // 外圆：圆心 / 半径
    ((3.79,  12.0), 7.0));   // 内圆
```

厚薄比（厚 7.4 / 尖角间距 13.4 = 0.55）：小尺寸下仍看得清内弧，
又不像"实心豆子"。外接框 `x∈[5.82,18.19]`、`y∈[4,20]`，在 24 栅格内居中。

### 顶栏不再重复设置入口

对话态顶栏右侧原有「设置 + 新对话」两个圆钮，而侧栏底部本来就有一行设置 ——
同一个动作两个入口，且顶栏那个抢了最靠右的视觉重心。现在顶栏只留「新对话」，
并顶到最右侧；设置统一从侧栏进。

反映到代码：`conversation::Outcome` 去掉 `open_settings` 字段，
`app.rs` 的设置开关只剩 `sb.open_settings` 一个来源 —— 入口唯一，状态也就没有二义性。

---

## 2026-09-21 补充：工具集落地

Neo 现在可以调用一组工具（读/写/改文件、查看图片、打开文件、执行命令）。
工具层单独成 crate：`crates/neo-tools`（纯逻辑、无 UI），
协议搬运在 `neo-llm`（`tool_calls` 分片聚合），权限交互与卡片展示在 `neo-app`。

**逐工具的契约（名称 / 用途 / 参数 / 返回 / 场景与边界 / 权限 / 错误）见 `docs/tools.md`。**
工具执行在**独立线程**（渲染循环只逐帧收结果），执行工具用 PowerShell。
界面形态见 `docs/screens/14-tool-cards-1080p.png`（工具卡片三态）与
`docs/screens/15-tool-confirm-1080p.png`（权限确认弹窗）。

---

## 2026-09-22 补充：图标与标志的几何全部取自上游

### 之前错在哪

Neo 的图标此前是**手绘描边**（24 栅格、1.6 单位宽的线）。看起来"风格接近"，
但和上游放一起就是不一样。原因查清楚了：

**上游图标不是一笔描边，而是 16/14 栅格上的填充路径。**
`viewBox="0 0 16 16"` + `fill="currentColor"`，线稿的观感由**两条反向绕行的
轮廓**（外轮廓 + 内轮廓）填出来 —— 例如月亮是一个圆环，齿轮的中心孔也是这么来的。
所以只要还在"画线"，粗细、端点、圆角就永远差一截；而且上游一改几何就得重画。

### 现在的做法：抽，不画

```
npm 上的 @deepseek-ai/dsh-client-ui-primitives
   └─ tools/extract_icons.py  → icons.json（67 个图标的路径几何）
        └─ tools/gen_icons.py  → crates/neo-ui/src/icons/paths.rs（22 个）
```

- 22 个图标**逐字节使用上游路径**，与 `brand/whale_path.rs` 同一套做法；
- 生成时把 `transform="translate(...)"` 烘焙进坐标（上游 5 个图标用它摆位，
  漏掉 `FolderClose` 就会偏出左上角）；
- 只有 `Icon::Mic` 仍手绘 —— 上游图标集里没有麦克风。

渲染侧配套新增 `neo_theme::svg`：SVG 路径解析（`M/L/H/V/C/Q/S/T/Z`、绝对与相对、
**科学计数法**、隐式重复）+ 扫描线光栅化（**nonzero / even-odd 两套填充规则，
支持挖空**）。必须自己写的原因是 `epaint::fill_closed_path` 是三角扇，
只对外凸多边形正确 —— 双轮廓图标用它会填成一坨实心。

图标先光栅化成**白色 alpha 纹理**（懒生成、每图标一份、128px），绘制时 `tint`
上色：抗锯齿在纹理里一次算好，每帧只是一个贴图四边形。

### 顺带修正的尺寸问题

`IconButton` 原来把图标铺满整个按钮（`visual_d × visual_d`）。手绘时代图标
自带留白，看着还行；换成上游几何后图标墨迹基本填满 viewBox，铺满就会顶到圆边。
现在按上游比例：**28/34px 圆钮放 16px 图标、24px 圆钮放 14px**（上游图标集里
14/16 两档正是为这两种场景准备的）。

### 校验

- `rasterized_ink_matches_path_bounds`：对全部 22 个图标断言
  **光栅化墨迹的包围盒 == 路径几何的包围盒** —— 一次守住坐标翻转、viewBox 比例、
  漏 transform、上限裁切这几类静默错误；
- `moon_has_a_cut_out_and_no_solid_blob`：把"填充规则退化成实心"钉住；
- `dump_glyphs`（`--ignored --nocapture`）：把字形打成 ASCII 字符画，用于人工核对。

上游包位于 `D:\My things\Learn\高二\Harness\`（不进 git），
重跑流程见 `tools/README.md`。

---

## 2026-09-22 补充：模型列表只由模型商提供

早先 `AppState::default` 里预置了一张写死的候选表（`deepseek-chat` /
`deepseek-reasoner`），拉取失败时拿它兜底。现在**默认为空**：

| 环节 | 行为 |
|---|---|
| 默认 | `models` 为空 —— 代码里不再有"候选模型"这种东西 |
| 首次启动 | 已配密钥就**自动**拉一次 `GET /models`（见下） |
| 拉到之后 | 列表**落库**（setting `models`，换行分隔的 id） |
| 之后每次启动 | 先从库里读回来（立刻可用，不依赖网络），**随后再刷新一次** |
| 拉取失败 | 保留上次存下的列表；一次都没成功过就保持空并给出指引 |

写死的表没有消失，但换了角色：`KNOWN_MODELS` 只是**命名表** ——
把拉到的裸 id 换成好看的展示名（`deepseek-chat` → `DeepSeek-V3.2`），
表里没有的按 id 原样显示。它不再是任何东西的兜底。

### 两条由此产生的不变量

- **没有模型就不能发请求**：`can_call_real()` 要求"密钥 + 地址 + 有模型"三者齐全。
  否则请求体里的 `model` 是空串，服务端只会回一个难懂的 400。
- **选中项存 id、不存索引**：列表每次刷新都可能重排，索引会错位。
  读回时按 id 认；认不出来再当旧的数字索引认一次（从老版本升上来选择不丢）。

### 空列表在界面上是什么样

输入卡上方那条提示（与 `Plan` / `只读` 共用同一条 `.notice`）会变成
「还没有可用模型 · 到「设置 → 模型」点「从模型商刷新」」；模型 chip 显示
「未选择模型」，并且**点它一下会就地触发一次拉取** —— 比"点了什么都不发生"好。
按发送键时的行为同理：配了密钥却没有列表，就**先把草稿留着**、就地拉一次，
不让用户白打一遍字。

见 `20-no-model-notice-1080p` / `21-settings-model-empty-1080p`。

### 启动顺序是个坑（已在测试里钉住）

`install` 里"拉取"一度写在"装载设置"**之前**，而 `start_model_fetch` 会先看
密钥决定拉不拉 —— 于是每次启动都因为"密钥还是空的"直接返回，
**自动刷新的开关形同虚设**。现在顺序是「装载 → 拉取」，
`startup_loads_saved_models_then_refreshes` 同时断言"列表读回来了"和
"紧接着真的发起了刷新"（拉取指向本机一个必定拒绝连接的端口，不碰外网）。

### 测试之间共享库的问题（也一并修掉）

快照测试共享同一个临时库，而 `install` 会从库里装载设置 —— 于是一张快照
往库里写过的 `api_key`，会让**后续每张快照启动时都真的联网**（画面上出现
"正在拉取…"，还取决于外面有没有网）。这是同一个坑咬的第三次（前两次：
"上次会话"被恢复导致 hero 画成对话态、14/15 拍成了 hero）。

现在每次装配前调用 `fresh_db()` 删掉库文件：每个快照都是**全新安装**，
需要预置数据的用例自己在 `setup` 里造。
