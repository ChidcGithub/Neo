#!/usr/bin/env python3
"""把「随包提供的 Git Bash 运行时」拉下来。

Neo 的 `bash` 工具需要一个**类 Unix 命令行环境**。一体机上不一定装了 Git，
所以运行时随包提供：下载 Git for Windows 的 **MinGit**（官方最小发行版），
解到 `runtime/gitbash/`，工具就能在没装 Git 的机器上跑起来。

    python tools/fetch_runtime.py                 # 拉最新版（按镜像顺序试）
    python tools/fetch_runtime.py --version 2.55.0.windows.5
    python tools/fetch_runtime.py --mirror github # 只用某个源
    python tools/fetch_runtime.py --force         # 已存在也重下

产物（不进版本库，见 .gitignore）：

    runtime/gitbash/
      bin/bash.exe        ← 工具优先用这个
      usr/bin/bash.exe
      mingw64/…
      cmd/git.exe

## 为什么要多源

GitHub 的 release 资产在国内经常下不动（本机实测直接 `TimeoutError`）。
所以**资产名从 GitHub API 问**（那个接口很快），**字节从镜像取**：

1. `npmmirror`（registry.npmmirror.com，国内快）
2. `github`（官方，海外快）

> ⚠️ 镜像上的**资产名**与 tag 不同：tag 是 `v2.55.0.windows.5`，
> 资产叫 `MinGit-2.55.0.5-64-bit.zip`（少一段 `.windows`）。
> 所以先问 API 拿准确文件名再拼地址 —— **别自己从 tag 拼资产名**（拼了会 404）。

只要 stdlib，不引第三方依赖。
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile

REPO = "git-for-windows/git"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEST = os.path.join(ROOT, "runtime", "gitbash")

UA = {"User-Agent": "neo-fetch-runtime"}

MIRRORS: dict[str, str] = {
    "npmmirror": "https://registry.npmmirror.com/-/binary/git-for-windows/{tag}/{asset}",
    "github": "https://github.com/{repo}/releases/download/{tag}/{asset}",
}
MIRROR_ORDER = ("npmmirror", "github")

# API 问不到时的兜底（本机实测能下到的版本）
FALLBACK_VERSION = "v2.55.0.windows.5"
FALLBACK_ASSET = "MinGit-2.55.0.5-64-bit.zip"

# 37 MB 在国内链路上可能要走一会儿 —— 别用默认的短超时。
SOCKET_TIMEOUT = 300


def fetch_bytes(url: str, timeout: int = SOCKET_TIMEOUT) -> bytes:
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read()


def pick_asset(version: str | None) -> tuple[str, str, str]:
    """问出（tag, 资产名, 官方地址）。问不到就退回已知版本。"""
    url = f"https://api.github.com/repos/{REPO}/releases"
    url = url if version is None else f"{url}/tags/v{version}"
    try:
        data = json.loads(fetch_bytes(url, timeout=60))
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
        print(f"问 GitHub API 失败（{e}）；退回已知版本 {FALLBACK_VERSION}")
        data = None

    if data is None:
        return (
            FALLBACK_VERSION,
            FALLBACK_ASSET,
            MIRRORS["github"].format(repo=REPO, tag=FALLBACK_VERSION, asset=FALLBACK_ASSET),
        )
    if isinstance(data, list):  # /releases 返回数组，可能含预发布
        data = next((r for r in data if not r.get("prerelease")), data[0])
    tag = data["tag_name"]
    for a in data["assets"]:
        name = a["name"]
        # 排除 busybox 变体：它没有完整的 usr/bin，观感差别大
        if (
            name.startswith("MinGit-")
            and "64-bit" in name
            and name.endswith(".zip")
            and "busybox" not in name
        ):
            return tag, name, a["browser_download_url"]
    raise SystemExit(f"在 {tag} 里找不到 64 位 MinGit 资产")


def download(tag: str, asset: str, only: str | None, tmp: str) -> str:
    """按镜像顺序试着把资产拉到 `tmp`，返回成功的源名。"""
    order = [only] if only else list(MIRROR_ORDER)
    errors = []
    for name in order:
        url = MIRRORS[name].format(repo=REPO, tag=tag, asset=asset)
        print(f"尝试 {name}：{url}")
        try:
            req = urllib.request.Request(url, headers=UA)
            with urllib.request.urlopen(req, timeout=SOCKET_TIMEOUT) as resp:
                total = int(resp.headers.get("Content-Length") or 0)
                got = 0
                with open(tmp, "wb") as f:
                    while True:
                        chunk = resp.read(262144)
                        if not chunk:
                            break
                        f.write(chunk)
                        got += len(chunk)
                        if total:
                            pct = got * 100 // total
                            print(
                                f"\r  {got / 1048576:6.1f}/{total / 1048576:.1f} MB ({pct}%)",
                                end="",
                                flush=True,
                            )
                print()
            if total and got != total:
                raise OSError(f"只收到 {got} 字节，期望 {total} 字节")
            return name
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            print(f"  {name} 失败：{e}")
            errors.append(f"{name}: {e}")
    raise SystemExit("所有镜像都失败了：\n  " + "\n  ".join(errors))


def main() -> int:
    ap = argparse.ArgumentParser(description="拉取随包的 Git Bash 运行时")
    ap.add_argument("--version", default=None, help="Git for Windows 版本，默认最新")
    ap.add_argument("--mirror", default=None, choices=sorted(MIRRORS), help="只用指定源")
    ap.add_argument("--force", action="store_true", help="已存在也重下")
    args = ap.parse_args()

    bash = os.path.join(DEST, "bin", "bash.exe")
    if os.path.exists(bash) and not args.force:
        print(f"已存在：{bash}")
        return 0

    tag, asset, official = pick_asset(args.version)
    print(f"版本 {tag}，资产 {asset}\n官方地址：{official}")

    with tempfile.TemporaryDirectory() as td:
        zip_path = os.path.join(td, asset)
        used = download(tag, asset, args.mirror, zip_path)
        print(f"下载完成（源：{used}）")

        if os.path.isdir(DEST):
            shutil.rmtree(DEST)
        os.makedirs(DEST, exist_ok=True)
        with zipfile.ZipFile(zip_path) as z:
            z.extractall(DEST)

    # 有些发行版的 zip 里多一层目录，拍平它
    entries = os.listdir(DEST)
    if len(entries) == 1 and os.path.isdir(os.path.join(DEST, entries[0])):
        inner = os.path.join(DEST, entries[0])
        for item in os.listdir(inner):
            shutil.move(os.path.join(inner, item), os.path.join(DEST, item))
        os.rmdir(inner)

    bash = ensure_bash_named(DEST)
    if bash is None:
        print("解包后没找到可用的 shell，请检查目录结构", file=sys.stderr)
        return 1

    # 真实冒烟：**跑一次**，确认它自己报的是 GNU bash（比查文件存不存在靠谱）
    version = smoke_test(bash)
    if version is None:
        print(f"警告：{bash} 起不来，运行时可能不完整", file=sys.stderr)
        return 1
    print(f"完成：{bash}\n  {version}")
    return 0


def ensure_bash_named(root: str) -> str | None:
    """让运行时里真的有一个叫 `bash.exe` 的壳。

    MinGit 里**没有 `bash.exe`** —— 它把 bash 装成了 `usr/bin/sh.exe`
    （同一个二进制：跑 `sh.exe -c 'echo $BASH_VERSION'` 会告诉你它是 bash）。
    直接拿 `sh.exe` 当宿主也能用，但那样 bash 会进 **POSIX 模式**（`argv[0]` 以
    `sh` 结尾时的默认行为）。所以这里给它补一个 `bash.exe` 的名字：

    - 优先**硬链接**（同一分区、不占第二份空间）；
    - 不支持就复制一份（约 2.4 MB，可接受）。

    返回 bash.exe 的路径。
    """
    for rel in ("usr/bin/bash.exe", "bin/bash.exe"):
        p = os.path.join(root, *rel.split("/"))
        if os.path.exists(p):
            return p
    src = os.path.join(root, "usr", "bin", "sh.exe")
    if not os.path.exists(src):
        return None
    dst = os.path.join(root, "usr", "bin", "bash.exe")
    try:
        os.link(src, dst)
        which = "硬链接"
    except OSError:
        shutil.copy2(src, dst)
        which = "复制"
    print(f"补出 bash.exe（{which}自 sh.exe）：{dst}")
    return dst


def smoke_test(bash: str) -> str | None:
    """跑一次，验证 bash 本体与随包的核心 Unix 工具都能被 PATH 找到。"""
    import subprocess

    env = os.environ.copy()
    env["PATH"] = os.path.dirname(bash) + os.pathsep + env.get("PATH", "")
    try:
        out = subprocess.run(
            [
                bash,
                "-c",
                "echo $BASH_VERSION; set -o | grep -E '^posix'; "
                "command -v ls >/dev/null && command -v grep >/dev/null && command -v sed >/dev/null",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as e:
        print(f"  冒烟失败：{e}", file=sys.stderr)
        return None
    if out.returncode != 0:
        print(f"  冒烟失败：退出码 {out.returncode}\n{out.stderr}", file=sys.stderr)
        return None
    return out.stdout.strip().replace("\n", " · ")


if __name__ == "__main__":
    raise SystemExit(main())
