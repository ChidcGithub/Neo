#!/usr/bin/env python3
"""把「随包提供的 Git Bash 运行时」拉下来。

Neo 的 `bash` 工具需要一个**类 Unix 命令行环境**。一体机上不一定装了 Git，
所以运行时随包提供：下载 Git for Windows 的 **MinGit**（官方最小发行版），
缓存到 `.cache/runtime/gitbash/`，发行时复制到 `runtime/gitbash/`。

    python tools/fetch_runtime.py                 # 拉最新版（按镜像顺序试）
    python tools/fetch_runtime.py --version 2.55.0.windows.5
    python tools/fetch_runtime.py --mirror github # 只用某个源
    python tools/fetch_runtime.py --force         # 已存在也重下

产物（不进版本库，见 .gitignore）：

    .cache/runtime/gitbash/
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
import stat
import time
import tempfile
import urllib.error
import urllib.request
import zipfile

REPO = "git-for-windows/git"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEST = os.path.join(ROOT, ".cache", "runtime", "gitbash")

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
MAX_DOWNLOAD_BYTES = 256 * 1024 * 1024
MAX_API_BYTES = 8 * 1024 * 1024
MAX_EXTRACT_BYTES = 1024 * 1024 * 1024
MAX_ZIP_MEMBERS = 50000
DOWNLOAD_DEADLINE = 900


def check_local_path(path: str) -> None:
    # 拒绝路径任一级重解析点，避免缓存命中或目录替换触及外部目录。
    current = os.path.abspath(path)
    while True:
        try:
            info = os.lstat(current)
        except FileNotFoundError:
            pass
        else:
            if stat.S_ISLNK(info.st_mode) or getattr(info, "st_file_attributes", 0) & 0x400:
                raise ValueError(f"不允许重解析路径：{current}")
        parent = os.path.dirname(current)
        if parent == current:
            return
        current = parent


def extract_runtime(archive: zipfile.ZipFile, stage: str) -> None:
    members = archive.infolist()
    if len(members) > MAX_ZIP_MEMBERS or sum(i.file_size for i in members) > MAX_EXTRACT_BYTES:
        raise ValueError("压缩包超过解压预算")
    seen = set()
    for info in members:
        parts = info.filename.replace("\\", "/").removesuffix("/").split("/")
        mode = (info.external_attr >> 16) & 0o170000
        key = "/".join(parts).casefold()
        # zipfile 在 Windows 会把非法字符替换为下划线；必须在替换前拒绝，避免覆盖别名。
        if (info.orig_filename != info.filename
                or not parts or any(not p or p in (".", "..") or any(c in p for c in ':<>|"?*') or p.endswith((".", " "))
                                    or any(ord(c) < 32 for c in p)
                                    or p.split(".")[0].upper() in {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in "123456789¹²³"), *(f"LPT{i}" for i in "123456789¹²³")}
                                    for p in parts)
                or key in seen or mode not in (0, stat.S_IFREG, stat.S_IFDIR)
                or info.external_attr & 0x400 or info.flag_bits & 1):
            raise ValueError(f"不安全的压缩包路径或属性：{info.filename}")
        seen.add(key)
    # 只使用受预算约束的内置解压，不调用外部解压器。
    archive.extractall(stage)


def fetch_bytes(url: str, timeout: int = SOCKET_TIMEOUT) -> bytes:
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        data = resp.read(MAX_API_BYTES + 1)
        if len(data) > MAX_API_BYTES:
            raise ValueError("API 响应超过预算")
        return data


def pick_asset(version: str | None) -> tuple[str, str, str]:
    """问出（tag, 资产名, 官方地址）；显式版本绝不降级到其他版本。"""
    requested = "v" + version.removeprefix("v") if version else None
    url = f"https://api.github.com/repos/{REPO}/releases"
    url = url if requested is None else f"{url}/tags/{requested}"
    try:
        data = json.loads(fetch_bytes(url, timeout=60))
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
        if requested and requested != FALLBACK_VERSION:
            raise SystemExit(f"无法查询指定版本 {requested}：{e}；未更改现有运行时") from e
        print(f"问 GitHub API 失败（{e}）；使用已知版本 {FALLBACK_VERSION}")
        data = None

    if data is None:
        if requested and requested != FALLBACK_VERSION:
            raise SystemExit(f"指定版本 {requested} 无可用元数据")
        return (
            FALLBACK_VERSION,
            FALLBACK_ASSET,
            MIRRORS["github"].format(repo=REPO, tag=FALLBACK_VERSION, asset=FALLBACK_ASSET),
        )
    if isinstance(data, list):  # /releases 返回数组，可能含预发布
        data = next((r for r in data if not r.get("prerelease")), None)
    if not isinstance(data, dict) or not isinstance(data.get("assets"), list):
        raise SystemExit("GitHub API 未返回有效 release")
    tag = data.get("tag_name")
    if not isinstance(tag, str) or (requested and tag != requested):
        raise SystemExit(f"release 版本不匹配：请求 {requested}，返回 {tag}")
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
            started = time.monotonic()
            req = urllib.request.Request(url, headers=UA)
            with urllib.request.urlopen(req, timeout=SOCKET_TIMEOUT) as resp:
                total = int(resp.headers.get("Content-Length") or 0)
                if total < 0 or total > MAX_DOWNLOAD_BYTES:
                    raise ValueError("下载超过预算或长度无效")
                got = 0
                with open(tmp, "wb") as f:
                    while True:
                        chunk = resp.read(262144)
                        if not chunk:
                            break
                        got += len(chunk)
                        if got > MAX_DOWNLOAD_BYTES or time.monotonic() - started > DOWNLOAD_DEADLINE:
                            raise OSError("下载超过大小或时间预算")
                        f.write(chunk)
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
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
            print(f"  {name} 失败：{e}")
            errors.append(f"{name}: {e}")
    raise SystemExit("所有镜像都失败了：\n  " + "\n  ".join(errors))


def main() -> int:
    ap = argparse.ArgumentParser(description="拉取随包的 Git Bash 运行时")
    ap.add_argument("--version", default=None, help="Git for Windows 版本，默认最新")
    ap.add_argument("--mirror", default=None, choices=sorted(MIRRORS), help="只用指定源")
    ap.add_argument("--force", action="store_true", help="已存在也重下")
    args = ap.parse_args()

    try:
        for rel in ("", "bin/bash.exe", "usr/bin/bash.exe", ".neo-version"):
            check_local_path(os.path.join(DEST, rel))
    except (OSError, ValueError) as e:
        print(f"运行时路径无效：{e}", file=sys.stderr)
        return 1
    bash = find_bash(DEST)
    cached_tag = None
    try:
        with open(os.path.join(DEST, ".neo-version"), encoding="utf-8") as f:
            cached_tag = f.read(256).strip()
    except (OSError, UnicodeError):
        pass
    requested = "v" + args.version.removeprefix("v") if args.version else None
    if bash and not args.force and (requested is None or requested == cached_tag):
        print(f"已存在：{bash}")
        return 0

    tag, asset, official = pick_asset(args.version)
    print(f"版本 {tag}，资产 {asset}\n官方地址：{official}")
    parent = os.path.dirname(DEST)
    os.makedirs(parent, exist_ok=True)
    lock = os.path.join(parent, ".gitbash-install.lock")
    try:
        lock_fd = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        raise SystemExit(f"另一个安装正在进行或上次被强制终止，请检查 {lock}")
    try:
        with tempfile.TemporaryDirectory(prefix=".gitbash-stage-", dir=parent) as td:
            zip_path = os.path.join(td, "runtime.zip")
            used = download(tag, asset, args.mirror, zip_path)
            print(f"下载完成（源：{used}）")
            stage = os.path.join(td, "payload")
            os.mkdir(stage)
            with zipfile.ZipFile(zip_path) as z:
                extract_runtime(z, stage)
            entries = os.listdir(stage)
            if len(entries) == 1 and os.path.isdir(os.path.join(stage, entries[0])):
                inner = os.path.join(stage, entries[0])
                # usr 本身是有效根目录中的一部分，不应拍平。
                if entries[0] not in ("usr", "bin", "mingw64", "cmd"):
                    for item in os.listdir(inner):
                        shutil.move(os.path.join(inner, item), os.path.join(stage, item))
                    os.rmdir(inner)
            bash = ensure_bash_named(stage)
            version = smoke_test(bash) if bash else None
            if version is None:
                print("暂存运行时验证失败，现有运行时未更改", file=sys.stderr)
                return 1
            with open(os.path.join(stage, ".neo-version"), "w", encoding="utf-8") as f:
                f.write(tag)
            replace_runtime(stage, DEST)
            print(f"完成：{find_bash(DEST)}\n  {version}")
        return 0
    except (OSError, ValueError, zipfile.BadZipFile) as e:
        print(f"运行时安装失败：{e}", file=sys.stderr)
        return 1
    finally:
        os.close(lock_fd)
        os.unlink(lock)


def replace_runtime(stage: str, dest: str) -> None:
    # 网络下载和验证可能耗时数分钟，不能沿用下载前的路径检查。
    check_local_path(stage)
    check_local_path(dest)
    backup = tempfile.mkdtemp(prefix=".gitbash-backup-", dir=os.path.dirname(dest))
    os.rmdir(backup)
    moved = False
    try:
        if os.path.lexists(dest):
            os.replace(dest, backup)
            moved = True
        os.replace(stage, dest)
    except BaseException:
        if moved:
            # 不删除备份；恢复失败时仍保留完整旧版本，并明确报告位置。
            try:
                os.replace(backup, dest)
            except OSError as rollback_error:
                raise RuntimeError(f"回滚失败；完整旧运行时保存在 {backup}") from rollback_error
        raise
    if moved:
        try:
            shutil.rmtree(backup)
        except OSError:
            print(f"新版本已安装；旧备份暂无法清理：{backup}", file=sys.stderr)


def find_bash(root: str) -> str | None:
    for rel in ("bin/bash.exe", "usr/bin/bash.exe"):
        path = os.path.join(root, *rel.split("/"))
        if os.path.isfile(path):
            return path
    return None


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
    existing = find_bash(root)
    if existing:
        return existing
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

    # 禁止继承导出的 Bash 函数、SHELLOPTS 和 DLL/启动脚本注入变量。
    env = {k: v for k, v in os.environ.items()
           if k.upper() in {"SYSTEMROOT", "WINDIR", "SYSTEMDRIVE", "TEMP", "TMP"}}
    bindir = os.path.dirname(bash)
    root = os.path.dirname(bindir)
    if os.path.basename(root).lower() == "usr":
        root = os.path.dirname(root)
    env["PATH"] = os.pathsep.join((bindir, os.path.join(root, "usr", "bin"),
                                  os.path.join(root, "mingw64", "bin")))
    try:
        check_local_path(bash)
        out = subprocess.run(
            [
                bash,
                "--noprofile",
                "--norc",
                "-c",
                "test -n \"$BASH_VERSION\" && ! shopt -qo posix && "
                "command -v ls >/dev/null && command -v grep >/dev/null && "
                "command -v sed >/dev/null && printf '%s\\n' \"$BASH_VERSION\"",
            ],
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
            cwd=root,
        )
    except (OSError, ValueError, subprocess.SubprocessError) as e:
        print(f"  冒烟失败：{e}", file=sys.stderr)
        return None
    if out.returncode != 0:
        print(f"  冒烟失败：退出码 {out.returncode}\n{out.stderr}", file=sys.stderr)
        return None
    return out.stdout.strip().replace("\n", " · ")


if __name__ == "__main__":
    raise SystemExit(main())
