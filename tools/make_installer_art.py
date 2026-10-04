"""生成 NSIS 安装包的美术资源：图标 / 欢迎页侧栏 / 页眉小图。

鲸鱼几何直接解析自 ``crates/neo-app/src/brand/whale_path.rs`` —— 与应用内
托盘图标、hero 标志是同一份 SVG 路径（上游 FishLogo），这里不复制数据，
避免两处各自演化。

产物（默认输出到 ``target/package/installer-art/``，已被 .gitignore 排除）：
  neo.ico      多尺寸图标（16..256，品牌蓝鲸鱼，透明底）
  welcome.bmp  164x314 欢迎/完成页左侧竖幅（品牌渐变 + 白鲸 + 字标）
  header.bmp   150x57  页眉右侧小图（白底 + 品牌蓝鲸鱼）
  preview.png  三件套合成预览（仅供人工检查，安装包不使用）

依赖：Pillow。字体用 Windows 自带的 Segoe UI Bold / 微软雅黑；
找不到对应字体时自动省略文字（不会跑挂）。
"""

import argparse
import io
import os
import re

from PIL import Image, ImageDraw, ImageFont

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WHALE_RS = os.path.join(REPO, "crates", "neo-app", "src", "brand", "whale_path.rs")

BRAND = (0x4D, 0x6B, 0xFE)          # 品牌蓝（与托盘图标一致）
BRAND_LIGHT = (0x74, 0x8A, 0xFF)    # 渐变上端：亮品牌蓝
BRAND_DEEP = (0x2E, 0x3A, 0xA6)     # 渐变下端：深靛蓝

SS = 3  # 装饰位图的超采样倍数（抗锯齿）


# ---------------------------------------------------------------- 鲸鱼几何

def load_whale():
    """从 whale_path.rs 抽出路径与 viewBox（单一事实源）。"""
    src = io.open(WHALE_RS, encoding="utf-8").read()
    d = re.search(r'FISH_LOGO_PATH:\s*&str\s*=\s*"([^"]+)"', src).group(1)
    vw = float(re.search(r"VIEWBOX_W:\s*f32\s*=\s*([\d.]+)", src).group(1))
    vh = float(re.search(r"VIEWBOX_H:\s*f32\s*=\s*([\d.]+)", src).group(1))
    return parse_path(d), vw, vh


def parse_path(d):
    """展平绝对 M/C/L/Z 路径为闭合折线列表（文件头注释保证只有这些命令）。"""
    subs = []
    cur = start = None
    for cmd, argstr in re.findall(r"([MCLZ])([^MCLZ]*)", d):
        nums = [float(x) for x in re.findall(r"-?\d*\.?\d+(?:[eE][-+]?\d+)?", argstr)]
        if cmd == "M":
            cur = [(nums[0], nums[1])]
            start = cur[0]
            subs.append(cur)
        elif cmd == "C":
            for i in range(0, len(nums), 6):
                p0 = cur[-1]
                p1 = (nums[i], nums[i + 1])
                p2 = (nums[i + 2], nums[i + 3])
                p3 = (nums[i + 4], nums[i + 5])
                for s in range(1, 25):
                    t = s / 24.0
                    mt = 1.0 - t
                    cur.append((
                        mt**3 * p0[0] + 3 * mt * mt * t * p1[0]
                        + 3 * mt * t * t * p2[0] + t**3 * p3[0],
                        mt**3 * p0[1] + 3 * mt * mt * t * p1[1]
                        + 3 * mt * t * t * p2[1] + t**3 * p3[1],
                    ))
        elif cmd == "L":
            for i in range(0, len(nums), 2):
                cur.append((nums[i], nums[i + 1]))
        elif cmd == "Z":
            cur.append(start)
    return subs


def whale_mask(subs, vw, vh, w, h, ss=4):
    """奇偶填充遮罩：先画身体，再把镂空子路径（眼/鳍）抹成 0。

    几何上子路径互不重叠（Rust 侧有测试钉住），所以「先身体后挖洞」
    与奇偶规则等价。
    """
    wbig, hbig = w * ss, h * ss
    img = Image.new("L", (wbig, hbig), 0)
    dr = ImageDraw.Draw(img)
    sx, sy = wbig / vw, hbig / vh
    for i, sp in enumerate(subs):
        dr.polygon([(x * sx, y * sy) for x, y in sp], fill=255 if i == 0 else 0)
    return img.resize((w, h), Image.LANCZOS)


def whale_layer(subs, vw, vh, w, color):
    """鲸鱼上色图层（RGBA，尺寸 w × w/aspect）。"""
    h = int(round(w / (vw / vh)))
    layer = Image.new("RGBA", (w, h), color + (255,))
    layer.putalpha(whale_mask(subs, vw, vh, w, h))
    return layer


# ---------------------------------------------------------------- 字体

FONTS_DIR = os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts")


def load_font(candidates, size):
    for name in candidates:
        path = os.path.join(FONTS_DIR, name)
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
    return None


def draw_centered(dr, xy_cx, y, text, font, fill):
    bbox = dr.textbbox((0, 0), text, font=font)
    dr.text((xy_cx - (bbox[2] - bbox[0]) / 2 - bbox[0], y), text, font=font, fill=fill)


# ---------------------------------------------------------------- 三件套

def make_ico(subs, vw, vh, path):
    """多尺寸 ICO：品牌蓝鲸鱼，透明底（与系统托盘图标同形）。"""
    size = 256
    inner = int(round(size * 0.88))
    layer = whale_layer(subs, vw, vh, inner, BRAND)
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    img.paste(layer, ((size - inner) // 2, (size - layer.height) // 2), layer)
    img.save(path, format="ICO",
             sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])


def make_welcome(subs, vw, vh, path):
    """欢迎/完成页左侧竖幅 164x314：品牌渐变 + 光晕 + 白鲸 + 字标。"""
    w, h = 164 * SS, 314 * SS

    # 竖向三段渐变：亮品牌蓝 → 品牌蓝 → 深靛蓝
    img = Image.new("RGB", (w, h))
    dr = ImageDraw.Draw(img)
    stops = [(0.0, BRAND_LIGHT), (0.55, BRAND), (1.0, BRAND_DEEP)]
    for y in range(h):
        t = y / (h - 1)
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t0 <= t <= t1:
                k = (t - t0) / (t1 - t0)
                dr.line([(0, y), (w, y)],
                        fill=tuple(round(c0[i] + (c1[i] - c0[i]) * k) for i in range(3)))
                break
    img = img.convert("RGBA")

    # 鲸鱼背后两圈柔光
    cx, cy = w // 2, int(h * 0.33)
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse([cx - 66 * SS, cy - 66 * SS, cx + 66 * SS, cy + 66 * SS],
               fill=(255, 255, 255, 16))
    gd.ellipse([cx - 80 * SS, cy - 80 * SS, cx + 80 * SS, cy + 80 * SS],
               outline=(255, 255, 255, 40), width=SS)
    img.alpha_composite(glow)

    # 白色鲸鱼（镂空透出渐变）
    whale_w = 96 * SS
    whale = whale_layer(subs, vw, vh, whale_w, (255, 255, 255))
    img.alpha_composite(whale, (cx - whale_w // 2, cy - whale.height // 2))

    dr = ImageDraw.Draw(img)
    f_word = load_font(["segoeuib.ttf", "msyhbd.ttc", "arialbd.ttf"], 28 * SS)
    if f_word:
        draw_centered(dr, cx, int(h * 0.56), "Neo", f_word, (255, 255, 255, 255))
    f_tag = load_font(["msyh.ttc", "simhei.ttf", "simsun.ttc"], 11 * SS)
    if f_tag:
        draw_centered(dr, cx, int(h * 0.90), "教室大屏 AI 助手", f_tag, (255, 255, 255, 208))

    img.resize((164, 314), Image.LANCZOS).convert("RGB").save(path, format="BMP")


def make_header(subs, vw, vh, path):
    """页眉右侧小图 150x57：白底（融入 MUI 页眉）+ 品牌蓝鲸鱼。"""
    w, h = 150 * SS, 57 * SS
    img = Image.new("RGBA", (w, h), (255, 255, 255, 255))
    whale_h = 34 * SS
    whale_w = int(round(whale_h * (vw / vh)))
    whale = whale_layer(subs, vw, vh, whale_w, BRAND)
    img.alpha_composite(whale, (w - whale_w - 12 * SS, (h - whale_h) // 2))
    img.resize((150, 57), Image.LANCZOS).convert("RGB").save(path, format="BMP")


def make_preview(art_dir):
    """三件套合成预览图（人工检查用）。"""
    welcome = Image.open(os.path.join(art_dir, "welcome.bmp")).convert("RGB")
    header = Image.open(os.path.join(art_dir, "header.bmp")).convert("RGB")
    ico = Image.open(os.path.join(art_dir, "neo.ico")).convert("RGBA")
    ico = ico.resize((128, 128), Image.LANCZOS)

    canvas = Image.new("RGB", (540, 340), (0xF0, 0xF0, 0xF0))
    canvas.paste(welcome, (16, 13))
    canvas.paste(header, (196, 13))
    canvas.paste(ico, (196, 90), ico)
    canvas.save(os.path.join(art_dir, "preview.png"))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default=os.path.join(REPO, "target", "package", "installer-art"))
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    subs, vw, vh = load_whale()
    make_ico(subs, vw, vh, os.path.join(args.out, "neo.ico"))
    make_welcome(subs, vw, vh, os.path.join(args.out, "welcome.bmp"))
    make_header(subs, vw, vh, os.path.join(args.out, "header.bmp"))
    make_preview(args.out)
    for name in ("neo.ico", "welcome.bmp", "header.bmp", "preview.png"):
        p = os.path.join(args.out, name)
        print(f"  {name}: {os.path.getsize(p)} bytes")


if __name__ == "__main__":
    main()
