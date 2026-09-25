# neo-ui · 设计接口手册

> 目标：**每个按钮、对话框、界面只写一遍。** 页面不再手搓控件，
> 一律调用 `crates/neo-ui`；造型、交互、触控命中区、明暗两套主题
> 都由组件库统一负责。本文是接口速查，配图见 `docs/screens/13-design-kit-1080p.png`
>（跑 `cargo test design_kit` 刷新）。

## 分层

```text
neo-theme   语义 token（色板 / 度量 / 字号 / 超椭圆）     ← 数值的唯一来源
   ↓
neo-ui      组件（Design + 全部控件）                    ← 造型与交互的唯一来源
   ↓
neo-app     页面（hero / conversation / settings …）     ← 只做编排与业务
```

- 页面持有 `Skin`（= `Design` + 鲸鱼品牌资源），控件只认 `&Design`。
- **组件颜色一律从 `d.c()`（组件 token）取**，表面色从 `d.p()`（色板）取；
  不要在页面里写裸 `Color32`。
- 度量一律 `d.m().s(x)`（随观看距离缩放），不要写死像素。

## 三条贯穿全库的约定

1. **动作只回报，不执行。** 组件返回 `egui::Response` 或 `Outcome`，由页面改状态。
2. **视觉保真，命中区扩展。** 绘制尺寸沿用上游 Harness 原值；交互矩形自动扩到
   触控下限（`Metrics::hit_target`）。
3. **交互 Id 必须在同一容器里唯一** —— 而且**不能只靠"文字"区分**。
   egui 的 `ui.interact(rect, id, sense)` 若拿到重复 Id，一次点击会**同时**命中
   两个控件，且两者都不会报错。

   | 控件 | 默认 Id 里的区分项 | 同一容器里相同的… |
   |---|---|---|
   | `Segmented` | **整组选项** + 下标 | 两组一字不差的选项 → 必须给 `id_salt` |
   | `TextField` / `Button` / `NavItem` | 宽度/标签 **+ 绘制位置** | 同位置不可能有两个控件，安全 |
   | `IconButton` / `Chip` / `Card` / `Switch` | 绘制位置 | 安全 |

   **踩过的坑**：设置面板的页签行第 1 项叫「显示」，「显示思考过程」那一行的第 1 项
   也叫「显示」，两处都落在同一个 `Ui` 里 —— 于是点「显示」把页签一起翻走了。
   守卫：`neo-ui` 的 `id_tests::segment_groups_in_settings_do_not_collide`
   （纯 Id 断言）与 `neo-app` 的 `settings_panel_segment_ids_are_unique`
   （渲染真实面板后断言 Id 唯一，探针由 `neo-ui/probe` feature 提供 ——
   egui 自带的 `warn_on_id_clash` 只覆盖真正的 widget，组件库直接用 `ui.interact`
   画的它看不见）。

## 控件速查


### 按钮（`button.rs`）

```rust
use neo_ui::{Button, Variant, Size};

if Button::new("开始讲解").icon(Icon::Board).primary().show(ui, &d).clicked() { … }
Button::new("取消").ghost().show(ui, &d);
Button::new("删除会话").danger().show(ui, &d);
Button::new("保存").elevated().enabled(ok).loading(saving).show(ui, &d);
Button::new("发送").primary().large().full_width().show(ui, &d);   // 整行
```

| 变体 | 用途 | 上游 token |
|---|---|---|
| `Primary` | 主操作（业务蓝） | `button-info-fill` |
| `Elevated` | 次级（抬升面） | `button-elevated-fill` |
| `Ghost` | 最弱（悬停出底） | — |
| `Danger` | 危险 | `state-error-*` |
| `Contrast` | 反色 | `button-contrast-fill` |

尺寸档 `Size::Sm/Md/Lg` = 28/36/44pt；禁用自动置灰，加载态自动转圈。

### 图标按钮（`IconButton`）

```rust
IconButton::new(Icon::Cog).elevated().show_at(ui, &d, center).clicked();  // 绝对定位
IconButton::new(Icon::Plus).subtle().show(ui, &d);                        // 布局流
IconButton::new(Icon::ArrowUp).accent().enabled(can_send)                 // 发送钮
    .show_at(ui, &d, send_center);
```

样式：`Ghost`（列表行内）/ `Elevated`（顶栏圆钮）/ `Floating`（hero 档位）/
`Danger`（悬停变红）/ `Accent`（发送等主圆形动作，禁用时淡出 40%）/
`Subtle`（输入卡 `+`，selector 底）。

### 文本胶囊（`Chip`）

```rust
let w = Chip::width(&painter, &d, "Plan", false);            // 先量宽
Chip::new("Plan").active(on).show_at(ui, &d, rect).clicked();
Chip::new(model).chevron(true).show_at(ui, &d, rect);        // 带下拉箭头
```

### 分段控件（`Segmented`）

```rust
if let Some(i) = Segmented::new(&["暗色", "亮色"], idx).show(ui, &d, width) {
    theme = if i == 0 { Dark } else { Light };
}
```

### 卡片（`container.rs`）

```rust
let (rect, resp) = Card::input().paint(ui, &d, rect);        // 只画表面
let (_, resp) = Card::bubble().interactive().id_salt("s1")    // 可点 + 悬停抬升投影
    .selected(sel).paint(ui, &d, rect);
```

表面：`Input`（输入卡）/ `Bubble`（气泡、场景卡）/ `Raised`（次级区块）/
`Tip`（弱底）。选中态自动 accent 描边 + 浅底。

### 面板 / 对话框（`modal.rs`）

```rust
// 浮层面板底（设置面板用它）：
let body = Panel::new().paint(ui, &d, rect, pad);

// 全屏模态：遮罩 + 阻塞 + 标题栏 + Esc/关闭钮，一次封好：
let modal = Modal::new("设置", ModalSize::Md);
if let Some(body) = modal.begin(ui, &d, h) {
    if modal.end(ui, &d, body) { *open = false; }
}

// 确认框（删除/放弃修改）：
match Confirm::new("删除会话", "「…」及其消息将被移除。").danger()
    .show(ui, &d, width) {
    Some(true) => delete(), Some(false) | None => {}
}
```

### 表单（`field.rs`）

```rust
TextField::new(&mut s).hint("输入课堂问题…").show(ui, &d, w);   // 容器+聚焦环+占位
TextField::new(&mut key).secret(true).id_salt("api-key").show(ui, &d, w);
Switch::new(on).show_at(ui, &d, rect);                          // 开关
FieldRow::new("最终倍率", "1.28 ×").show(ui, &d, w);            // 键值行
field::hint_row(ui, &d, w, "仅保存在本机");                      // 灰字提示
```

> `id_salt` 必须是**常量**（不能随内容变），否则每次按键 id 变化、焦点丢失。

### 列表（`list.rs`）

```rust
NavItem::new("新对话", Icon::Plus).show(ui, &d, w);             // 导航行
match ListRow::new(id, title, meta).active(active)              // 会话行（动作钮恒显）
    .show_normal(ui, &d, w) {
    Some(RowAction::Open) => …, Some(RowAction::Rename) => …, Some(RowAction::Delete) => …, _ => {}
}
confirm_row(ui, &d, rect, id, &ConfirmBar::new("删除这条会话？")); // 危险确认条
rename_frame(ui, &d, rect);                                     // 行内重命名容器
```

> **动作钮恒显而非悬停露出** —— 触控一体机没有 hover。

### 反馈（`feedback.rs` / `badge.rs`）

```rust
Spinner::new().show_painter(&p, &d, rect, color);               // 加载转圈
EmptyState::new(Icon::Mic, "还没有语音记录").hint("点击麦克风开始")
    .show(ui, &d, rect);                                        // 空态
Toast::new(ToastKind::Success, "已保存").show_at(ui, &d, anchor); // 浮动提示
Tooltip::new("Enter 发送").show_at(ui, &d, pos);                 // 悬浮提示
Badge::new("R1").tone(BadgeTone::Accent).show(ui, &d);          // 徽标
```

## 加新控件的流程

1. 问上游：Harness 的 CSS 里有没有对应的 token / 组件？有 → 抄值进
   `neo-theme`（缺 token 时先补 token，再写组件）。
2. 写进 `neo-ui` 对应文件（按钮族 → `button.rs`，容器 → `container.rs`…），
   带上文档注释（用途 + 上游出处）。
3. 组件**不碰业务状态**，只返回 `Response` / `Outcome`。
4. 把它加进 `design_kit_gallery` 陈列室快照，跑串行测试截图核对。

## 页面迁移约定

- 页面里**不允许**再出现自绘按钮/对话框/输入框 —— 见到就迁进组件库。
- 兼容壳（`ui::section_label` 等 Skin 包装）只是历史过渡，新代码直接 `neo_ui::`。
- 每帧内同一控件要用**稳定的 id**（`id_salt` 用常量，别用位置索引以外的易变值）。

---

## 工具调用行（2026-09-22，对照上游 `dsh-client-ui-tool` 重做）

原本是一张「标题 + 摘要 + 原因」的三行卡片。上游不是这么做的：它把每次工具调用
**归成一个变体**，然后渲染成**一行**。结构照搬过来：

```text
[图标/状态点] 标题 · 摘要…………………………  [退出码]
```

| 部分 | 规则 | 出处 |
|---|---|---|
| **标题** | 变体名，不是工具自己的名字 | `VARIANT_TITLES`（源码注明「design literals」） |
| **摘要** | 与标题同行，中间 2pt 圆点、左右各 8pt；三级色 + 省略号 | `.sep` / `.summary` |
| **摘要取值** | 按变体取参数键：执行类 `description→command`、读取类 `path→file_path→url`……取不到退到「第一个非空字符串」 | `SUMMARY_KEYS` |
| **行首** | 平时是变体图标；`error` → 红点、`stopped` → 警告点 | `leadingFor` |
| **执行中** | 一行 300pt 宽的柔和高光从左扫到右，2.6s 循环 | `.running:after` 与它的 keyframes |
| **非零退出** | 摘要后一枚红色退出码胶囊 | `terminalFailed` |

**变体分类**（`neo-tools::present::Variant`）：`search / read / bash / write / edit /
code / others`。上游原表 + Neo 的六个工具都在里面（`read_file` / `view_image` → read，
`write_file` → write，`edit_file` → edit，执行工具 → bash），
认不出来的**不猜**，落到 `others` 并在标题位置显示工具名。

**词换中文、结构不动**：上游用 `Read` / `Bash` 这批字面量，Neo 用「读取 / 执行 /
修改 / 写入 / 搜索 / 运行代码 / 工具」—— 学生扫一眼要能认出「这是在读文件还是在跑命令」。

**唯一有意偏离**：扫光的高光色。上游用 `color-mix(bg-base 60%, transparent)`，
在暗色主题下几乎看不见；Neo 改成正文色 10% 的柔和高光，观感一致但看得出来。

行首图标是上游**插件自己给的**（`keyed per-tool presentation`），它那套里没有终端、
也没有「写入」，所以 Neo 用现有图标挑最贴近的语义（读取→文件夹、写入→加号、
修改→笔、执行→箭头、搜索→放大镜……见 `conversation.rs::icon_for`）。

---

## 列表渲染（2026-09-22）

Markdown 的列表由 `pulldown-cmark` 解析，但**序号规则、标记栏、缩进都是我们自己实现的**，
上游 `MarkdownText` 的观感不能交给第三方主题。

### 序号

`Tag::Item` **不带序号**，只能自己数：每层列表各维护一个「下一个序号」，
`Start(List(Some(n)))` 时置为 `n`（CommonMark 的起点就取第一项写的数字）。

| 写法 | 渲染 |
|---|---|
| `1. 2. 3.` | 1. 2. 3. |
| `1. 1. 1.`（全写 1，CommonMark 允许） | 1. 2. 3.（**不能被字面量骗到**） |
| `5. 6. 7.` | 5. 6. 7. |

> 曾经的 bug：直接拿列表的起点当每一项的序号 → 整张列表全是「1.」。
> 回归快照 `docs/screens/19-list-numbering-1080p.png`（递增 / 非 1 起点 / 两位数 / 嵌套）。

### 标记栏（这是"正文列对齐"的关键）

标记不参与正文的 LayoutJob，而是**单独占一个固定宽度的栏**，数字在栏内**右对齐**：

- 栏宽按**该层最宽的序号**预留，量宽度用的是 `0` 组成的探针（数字等宽），
  所以栏宽只取决于**位数**，与当前项写的是 `1` 还是 `10` 无关；
- 间距 = 0.45 × 字号；
- 无序列表同样占一栏（项目符号 + 同宽间隙），层级之间才对得齐。

> 为什么不能靠空格补位：正文是**比例字体**，`" "` 比数字窄 ——
> 实测 `" 1."` 与 `"10."` 不等宽，两位数时正文列会往右挪 4pt。

实测（`19-list-numbering-1080p`，Classroom 档）：11 项的列表里，
1~9 的标记落在 x=566..577、10~11 落在 558..577（右缘一致），**正文列全部在 x=579**。

### 嵌套缩进

`indent = 正文行高 × 1.1 × depth`。渲染用 `horizontal_top` 而不是 `horizontal` ——
后者会按交叉轴**居中**，多行列表项的标记会飘到整块中间，而不是停在第一行。

### 每个列表项的第一段必须先落块

紧列表（tight list）里 pulldown **不发 `Paragraph` 事件**，唯一的 flush 时机是
`End(Item)`。如果列表项里还挂着嵌套列表，内层 `Start(Item)` 会把外层的
`pending_item` 覆盖掉 —— 外层那行文字会整行消失，还会和内层第一项并进同一块。
所以 `Start(Item)` 里要先 flush 一次。
