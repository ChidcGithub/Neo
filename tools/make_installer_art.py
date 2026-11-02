"""Generate NSIS installer art from the tracked Neo brand PNGs.

Products (default output: ``target/package/installer-art/``, ignored by Git):
  neo.ico      multi-size icon: large ``full`` mark, small ``neo + spark`` mark
  welcome.bmp  164x314 welcome/finish sidebar (brand gradient + white lockup)
  header.bmp   150x57 header strip (white background + black wordmark)
  preview.png  contact sheet for manual inspection; not used by the installer

Only Pillow and the checked-in ``resources/icons`` exports are required.
"""

import argparse
import os
import struct

from PIL import Image, ImageDraw, ImageFont

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ICONS = os.path.join(REPO, "resources", "icons")

BRAND = (0x4D, 0x6B, 0xFE)
BRAND_LIGHT = (0x74, 0x8A, 0xFF)
BRAND_DEEP = (0x2E, 0x3A, 0xA6)

SS = 3


def load_icon(name):
    return Image.open(os.path.join(ICONS, name)).convert("RGBA")


def fit_icon(image, size):
    copy = image.copy()
    copy.thumbnail((size, size), Image.LANCZOS)
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    canvas.alpha_composite(copy, ((size - copy.width) // 2, (size - copy.height) // 2))
    return canvas


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

def ico_bmp_frame(image):
    """Encode one ICO frame as a 32-bit BMP DIB (NSIS-safe, no PNG frames)."""
    size = image.width
    pixels = image.load()
    header = struct.pack(
        "<IiiHHIIiiII",
        40,
        size,
        size * 2,
        1,
        32,
        0,
        size * size * 4 + ((size + 31) // 32 * 4) * size,
        0,
        0,
        0,
        0,
    )
    body = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            r, g, b, a = pixels[x, y]
            body += bytes((b, g, r, a))
    mask_stride = (size + 31) // 32 * 4
    body += bytes(mask_stride * size)
    return header + bytes(body)


def make_ico(path):
    """ICO with the full lockup at large sizes and the wordmark at small sizes."""
    frames = []
    large = load_icon("app_black.png")
    small = load_icon("window_black.png")
    for size, source in [
        (16, small),
        (20, small),
        (24, small),
        (32, large),
        (40, large),
        (48, large),
        (64, large),
        (128, large),
        (256, large),
    ]:
        frames.append((size, ico_bmp_frame(fit_icon(source, size))))

    out = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    offset = 6 + 16 * len(frames)
    for size, payload in frames:
        out += struct.pack(
            "<BBBBHHII",
            0 if size == 256 else size,
            0 if size == 256 else size,
            0,
            0,
            1,
            32,
            len(payload),
            offset,
        )
        offset += len(payload)
    for _, payload in frames:
        out += payload
    with open(path, "wb") as handle:
        handle.write(out)


def make_welcome(path):
    """164x314 welcome/finish sidebar: gradient, glow, white full lockup."""
    w, h = 164 * SS, 314 * SS
    img = Image.new("RGB", (w, h))
    dr = ImageDraw.Draw(img)
    stops = [(0.0, BRAND_LIGHT), (0.55, BRAND), (1.0, BRAND_DEEP)]
    for y in range(h):
        t = y / (h - 1)
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t0 <= t <= t1:
                k = (t - t0) / (t1 - t0)
                dr.line(
                    [(0, y), (w, y)],
                    fill=tuple(round(c0[i] + (c1[i] - c0[i]) * k) for i in range(3)),
                )
                break
    img = img.convert("RGBA")

    cx, cy = w // 2, int(h * 0.33)
    glow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gd.ellipse(
        [cx - 66 * SS, cy - 66 * SS, cx + 66 * SS, cy + 66 * SS],
        fill=(255, 255, 255, 16),
    )
    gd.ellipse(
        [cx - 80 * SS, cy - 80 * SS, cx + 80 * SS, cy + 80 * SS],
        outline=(255, 255, 255, 40),
        width=SS,
    )
    img.alpha_composite(glow)

    lockup = load_icon("app_white.png")
    lockup = fit_icon(lockup, 112 * SS)
    img.alpha_composite(lockup, (cx - lockup.width // 2, cy - lockup.height // 2))

    dr = ImageDraw.Draw(img)
    f_tag = load_font(["msyh.ttc", "simhei.ttf", "simsun.ttc"], 11 * SS)
    if f_tag:
        draw_centered(
            dr,
            cx,
            int(h * 0.88),
            "教室大屏 AI 助手",
            f_tag,
            (255, 255, 255, 208),
        )

    img.resize((164, 314), Image.LANCZOS).convert("RGB").save(path, format="BMP")


def make_header(path):
    """150x57 header strip: white background, black wordmark at the right."""
    w, h = 150 * SS, 57 * SS
    img = Image.new("RGBA", (w, h), (255, 255, 255, 255))
    mark = fit_icon(load_icon("window_black.png"), 38 * SS)
    img.alpha_composite(mark, (w - mark.width - 10 * SS, (h - mark.height) // 2))
    img.resize((150, 57), Image.LANCZOS).convert("RGB").save(path, format="BMP")


def make_preview(art_dir):
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

    make_ico(os.path.join(args.out, "neo.ico"))
    make_welcome(os.path.join(args.out, "welcome.bmp"))
    make_header(os.path.join(args.out, "header.bmp"))
    make_preview(args.out)
    for name in ("neo.ico", "welcome.bmp", "header.bmp", "preview.png"):
        p = os.path.join(args.out, name)
        print(f"  {name}: {os.path.getsize(p)} bytes")


if __name__ == "__main__":
    main()
