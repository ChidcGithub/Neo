# tools —— 上游素材抽取 + 随包运行时

Neo 的视觉是"复刻 DeepSeek Harness"，所以**图形几何不靠手绘，靠抽取**。
这里放着抽取脚本；产物直接进源码。

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

## 两步流水线

```bash
# ① 从 bundle 抽出全部图标的几何 → icons.json（67 个）
python tools/extract_icons.py

# ② 按映射表生成 Rust 常量 → crates/neo-ui/src/icons/paths.rs
python tools/gen_icons.py
```

两个脚本里的路径是**绝对路径**（指向本机参考副本），换机器要改。
`gen_icons.py` 负责把 SVG `transform="translate(...)"` **烘焙进坐标**——
上游有 5 个图标靠 transform 把图案摆进 viewBox，漏掉它字形就会偏移
（`FolderClose` 会偏出左上角，是个很容易踩的坑）。

## 为什么不能只抄"看起来像"的形状

上游图标是 **16/14 栅格上的填充路径**：线稿的观感由**两条反向绕行的轮廓**
（外轮廓 + 内轮廓）填出来，不是一笔描边。手绘同类线条时，粗细、端点、圆角
必然差一截 —— 而且改不动：上游换个尺寸，你就得重画一遍。

因此渲染侧也配套做了改动：`neo_theme::svg` 用扫描线 + 填充规则做带洞填充
（`epaint` 的 `fill_closed_path` 是三角扇，只能填凸多边形），图标先光栅化成
白色 alpha 纹理、绘制时 `tint` 上色。

## 校验手法

改完一定要**看**，而且不能只看缩略图：

- `cargo test -p neo-ui dump_glyphs -- --ignored --nocapture`
  把字形打成 ASCII 字符画 —— 挖空、朝向、偏移一眼可见；
- `cargo test -p neo-ui` 里的 `rasterized_ink_matches_path_bounds`
  用"墨迹包围盒 == 路径几何包围盒"守住翻转/缩放/漏 transform 这类静默错误；
- 快照（`docs/screens/`）+ `tools/` 之外的临时脚本可以按像素采样核对位置。

---

## `fetch_runtime.py` —— 随包的 Git Bash 运行时

`bash` 工具要一个类 Unix 环境。一体机上不一定装了 Git，所以**运行时随包提供**：

```bash
python tools/fetch_runtime.py                 # 默认走最快镜像
python tools/fetch_runtime.py --mirror github # 只用官方源
python tools/fetch_runtime.py --force         # 重下
```

产物落 `runtime/gitbash/`（**不进版本库**），工具按
`NEO_GIT_BASH` → `runtime/gitbash/` → PATH → 系统安装 的顺序挑宿主。

两个实测踩到的坑，都写进脚本了：

1. **GitHub 的 release 资产在国内下不动**（直接 `TimeoutError`）。
   所以资产名从 GitHub API 问（那个接口很快），**字节从 npmmirror 取**；
   两个源都试，全失败才报错。
2. **镜像上的资产名和 tag 不一样**：tag 是 `v2.55.0.windows.5`，
   资产叫 `MinGit-2.55.0.5-64-bit.zip`（少一段 `.windows`）。
   所以必须先问 API 拿文件名再拼地址 —— 从 tag 拼出来的地址是 404。
