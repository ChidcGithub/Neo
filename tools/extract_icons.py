"""从 dsh-client-ui-primitives 的 bundle 里抽出全部图标的几何。

上游图标是 React 组件：viewBox 16×16 + fill=currentColor 的 path。
抽出来存 JSON，供对照与生成 Rust 常量表。
"""

import io
import json
import os
import re

SRC = (
    r"D:\My things\Learn\高二\Harness\dsh-client-ui-primitives@0.0.1-rc.1"
    r"\package\lib\index.js"
)
OUT = r"D:\My things\Learn\高二\Harness\icons.json"

s = io.open(SRC, encoding="utf-8").read()

# 每个图标以 `const IconXxx = ({ size = N, className }) => jsx(s)("svg", {` 开头
decl = re.compile(r"const (Icon[A-Za-z0-9_]+)\d* = \(\{ size = (\d+), className \}\) => jsxs?\(")
starts = [(m.group(1), int(m.group(2)), m.start()) for m in decl.finditer(s)]
print(f"找到 {len(starts)} 个图标声明")

icons = {}
for i, (base, default_size, pos) in enumerate(starts):
    end = starts[i + 1][2] if i + 1 < len(starts) else len(s)
    body = s[pos:end]

    vb = re.search(r'viewBox: "([^"]+)"', body)
    view_box = vb.group(1) if vb else "0 0 16 16"

    paths = [
        {"d": d, "fill_rule": (fr if fr in ("evenodd", "nonzero") else None)}
        for d, fr in re.findall(
            r'jsxs?\("path", \{([^}]*)\}', body
        )
        for _ in [0]
        for d, fr in [(re.search(r'd: "([^"]+)"', _[0] and fr or ""), None)]
    ] if False else []

    # path 元素：抓 d + 可能的 fillRule
    paths = []
    for pm in re.finditer(r'jsxs?\("path", \{([^}]*)\}', body):
        inner = pm.group(1)
        dm = re.search(r'd: "([^"]+)"', inner)
        if not dm:
            continue
        fr = re.search(r'fillRule: "(\w+)"', inner)
        tf = re.search(r'transform: "([^"]+)"', inner)
        paths.append({
            "d": dm.group(1),
            "fillRule": fr.group(1) if fr else None,
            # 上游有 5 个图标用 translate 把图案摆进 viewBox；
            # 漏掉它字形就会偏移（FolderClose 会偏出左上角）。
            "transform": tf.group(1) if tf else None,
        })

    circles = []
    for cm in re.finditer(r'jsxs?\("circle", \{([^}]*)\}', body):
        inner = cm.group(1)
        nums = {
            k: re.search(rf"{k}: ([-\d.]+)", inner)
            for k in ("cx", "cy", "r")
        }
        if all(nums.values()):
            circles.append({k: float(v.group(1)) for k, v in nums.items()})

    rects = []
    for rm in re.finditer(r'jsxs?\("rect", \{([^}]*)\}', body):
        inner = rm.group(1)
        got = {}
        for k in ("x", "y", "width", "height", "rx"):
            mm = re.search(rf"{k}: ([-\d.]+)", inner)
            if mm:
                got[k] = float(mm.group(1))
        if "width" in got and "height" in got:
            rects.append(got)

    others = sorted(
        set(re.findall(r'jsxs?\("([a-z]+)"', body)) - {"path", "circle", "rect"}
    )

    icons[base] = {
        "defaultSize": default_size,
        "viewBox": view_box,
        "paths": paths,
        "circles": circles,
        "rects": rects,
        "otherElements": others,
    }

io.open(OUT, "w", encoding="utf-8").write(json.dumps(icons, ensure_ascii=False, indent=1))

n_path = sum(1 for v in icons.values() if v["paths"])
n_circle = sum(1 for v in icons.values() if v["circles"])
n_rect = sum(1 for v in icons.values() if v["rects"])
n_other = {k: v["otherElements"] for k, v in icons.items() if v["otherElements"]}
print(f"有 path 的 {n_path}，有 circle 的 {n_circle}，有 rect 的 {n_rect}")
print("其它元素:", n_other)
vbs = {}
for v in icons.values():
    vbs[v["viewBox"]] = vbs.get(v["viewBox"], 0) + 1
print("viewBox 分布:", vbs)
print("多 path 的:", [k for k, v in icons.items() if len(v["paths"]) > 1][:10])
print("→", OUT)
