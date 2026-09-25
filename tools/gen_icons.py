"""把上游图标几何生成成 Rust 常量表（neo-ui/src/icons/paths.rs）。"""

import io
import json

ICONS = r"D:\My things\Learn\高二\Harness\icons.json"
OUT = r"D:\My things\Learn\高二\Neo\crates\neo-ui\src\icons\paths.rs"
VERSION = "dsh-client-ui-primitives@0.0.1-rc.1"

# Neo 的图标 → 上游图标（None = 上游没有，保留手绘）
MAP = [
    ("PLUS", "IconPlusOutline16", "Plus"),
    ("ARROW_UP", "IconSendOutline16", "ArrowUp"),
    ("STOP", "IconStopFill16", "Stop"),
    ("FOLDER", "IconFolderClose16", "Folder"),
    ("CHEVRON_DOWN", "IconChevronDownOutline14", "ChevronDown"),
    ("SUN", "IconLightOutline16", "Sun"),
    ("MOON", "IconDarkOutline16", "Moon"),
    ("CLOSE", "IconCloseOutline16", "Close"),
    ("COG", "IconSettingsOutline16", "Cog"),
    ("TRASH", "IconTrashOutline16", "Trash"),
    ("BOARD", "IconPanelLeftOutline16", "Board"),
    ("CHECKLIST", "IconChecklistOutline14", "Checklist"),
    ("PEN", "IconEditOutline16", "Pen"),
    ("CHECK", "IconCheckOutline16", "Check"),
    ("INFO", "IconQuestionOutline14", "Info"),
    ("WARN", "IconWarningOutline16", "Warn"),
    ("SPARKLE", "IconSparkle16", "Sparkle"),
    ("COPY", "IconCopyOutline16", "Copy"),
    ("SEARCH", "IconSearchOutline16", "Search"),
    ("ARROW_LEFT", "IconChevronLeftOutline14", "ArrowLeft"),
    ("ARROW_RIGHT", "IconChevronRightOutline14", "ArrowRight"),
    ("DOTS", "IconEllipsisOutline16", "Dots"),
]



ARITY = {"M": 2, "L": 2, "H": 1, "V": 1, "C": 6, "Q": 4, "S": 4, "T": 2, "A": 7, "Z": 0}
NUM = r"-?\d*\.?\d+(?:[eE][-+]?\d+)?"


def fmt(v):
    return format(v, ".10g")


def bake(d, tx, ty):
    """把 translate(tx, ty) 烘焙进坐标 —— Rust 侧就不必再实现 transform。

    上游只有 5 个图标用 transform 且全是 translate；这里只支持绝对命令，
    遇到相对命令直接断言失败，而不是悄悄画歪。
    """
    import re

    out = []
    for cmd, nums in re.findall(r"([MmLlHhVvCcSsQqTtAaZz])([^MmLlHhVvCcSsQqTtAaZz]*)", d):
        up = cmd.upper()
        if up == "Z":
            out.append(cmd)
            continue
        assert cmd == up, f"只支持绝对命令，遇到 {cmd}"
        vals = [float(x) for x in re.findall(NUM, nums)]
        n = ARITY[up]
        parts = [cmd]
        for i in range(0, len(vals), n):
            ch = vals[i : i + n]
            if up in ("M", "L", "T", "C", "S", "Q"):
                for k in range(0, len(ch), 2):
                    parts.append(fmt(ch[k] + tx))
                    parts.append(fmt(ch[k + 1] + ty))
            elif up == "H":
                parts.append(fmt(ch[0] + tx))
            elif up == "V":
                parts.append(fmt(ch[0] + ty))
            elif up == "A":
                parts += [
                    fmt(ch[0]), fmt(ch[1]), fmt(ch[2]), fmt(ch[3]), fmt(ch[4]),
                    fmt(ch[5] + tx), fmt(ch[6] + ty),
                ]
        out.append(" ".join(parts))
    return "".join(out)


icons = json.load(io.open(ICONS, encoding="utf-8"))

head = f'''//! 上游图标几何（**自动生成，不要手改**）。
//!
//! 来源：`{VERSION}` 的 `lib/index.js`，
//! 由 `tools/gen_icons.py` 从 bundle 里抽取 —— 与 `brand/whale_path.rs` 同一套做法：
//! 把上游的路径数据原样搬过来，保证曲线与比例完全一致。
//!
//! 上游图标是 **16/14 栅格上的填充路径**（`viewBox="0 0 16 16"` + `fill: currentColor`），
//! **不是**一笔描边：描边效果由两条反向绕行的轮廓构成，所以渲染必须支持带洞填充
//! （见 `neo_theme::svg`）。这也解释了为什么"看起来是线稿，实际是填充"。
//!
//! `EVEN_ODD` 为真的图标在上游声明了 `fillRule="evenodd"`。

use std::sync::OnceLock;

use egui::Color32;

/// 一个图标的几何 + 懒光栅化缓存。
pub struct Glyph {{
    /// viewBox 宽（等于高时即图标边长）。
    pub vw: f32,
    pub vh: f32,
    /// 上游是否声明 `fillRule="evenodd"`。
    pub even_odd: bool,
    /// 上游的 path `d` 字符串，一条一个填充单元。
    pub d: &'static [&'static str],
    /// 白色 + alpha 的像素（懒生成，每进程一次）。
    cache: OnceLock<Vec<Color32>>,
}}

/// 光栅化纹理的边长（像素）。
///
/// 图标最大用到约 45px（4K + 远距 2.8 倍），128 有近 3 倍余量，
/// 缩小时线性插值天然带抗锯齿；再大就只是浪费内存。
pub const TEXTURE_PX: usize = 128;

/// 垂直超采样倍数（水平方向由扫描线的小数端点给出抗锯齿）。
const SUPERSAMPLE: usize = 4;

impl Glyph {{
    const fn new(vw: f32, vh: f32, even_odd: bool, d: &'static [&'static str]) -> Self {{
        Self {{
            vw,
            vh,
            even_odd,
            d,
            cache: OnceLock::new(),
        }}
    }}

    /// 图案的宽高比。
    pub fn aspect(&self) -> f32 {{
        self.vw / self.vh
    }}

    /// 白色 + alpha 的像素，尺寸 [`TEXTURE_PX`] × `TEXTURE_PX / aspect`。
    pub fn pixels(&self) -> &[Color32] {{
        self.cache.get_or_init(|| {{
            let rule = if self.even_odd {{
                neo_theme::svg::FillRule::EvenOdd
            }} else {{
                neo_theme::svg::FillRule::NonZero
            }};
            let mut contours = Vec::new();
            for d in self.d {{
                contours.extend(neo_theme::svg::parse_subpaths(d));
            }}
            let w = TEXTURE_PX;
            let h = ((w as f32 / self.aspect()).round() as usize).max(1);
            let cov = neo_theme::svg::rasterize(
                &contours,
                (self.vw, self.vh),
                w,
                h,
                SUPERSAMPLE,
                rule,
            );
            cov.iter()
                .map(|c| {{
                    let a = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                    Color32::from_white_alpha(a)
                }})
                .collect()
        }})
    }}
}}

'''

body = []
for const_name, upstream, neo in MAP:
    v = icons.get(upstream)
    if not v:
        raise SystemExit(f"上游缺少 {upstream}")
    vw, vh = (float(x) for x in v["viewBox"].split()[2:4])
    even_odd = any(p["fillRule"] == "evenodd" for p in v["paths"])
    ds = []
    for p in v["paths"]:
        d = p["d"]
        tf = p.get("transform")
        if tf:
            import re
            m = re.fullmatch(r"translate\(\s*([-\d.]+)[ ,]+([-\d.]+)\s*\)", tf)
            if not m:
                raise SystemExit(f"{upstream}: 未支持的 transform `{tf}`")
            d = bake(d, float(m.group(1)), float(m.group(2)))
        ds.append(d)
    paths = ",\n    ".join(f'"{d}"' for d in ds)
    body.append(
        f"/// `{upstream}` —— 对应 Neo 的 `Icon::{neo}`。\n"
        f"pub static {const_name}: Glyph = Glyph::new(\n"
        f"    {vw},\n    {vh},\n    {str(even_odd).lower()},\n    &[\n    {paths},\n],\n);\n"
    )

io.open(OUT, "w", encoding="utf-8", newline="\n").write(head + "\n".join(body))
print(f"生成 {OUT}：{len(MAP)} 个图标")
for const_name, upstream, neo in MAP:
    v = icons[upstream]
    print(f"  {neo:<12} ← {upstream:<26} paths={len(v['paths'])} vb={v['viewBox']}")
