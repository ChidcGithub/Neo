"""Fetch official portable CMake 4.2.3; no installer or global PATH changes.

Checks the ZIP against Kitware's versioned official SHA-256 list, or --wheel
against official PyPI 4.2.3 metadata (HTTPS, not an independent signature).
Total requested payload cap: 80 MiB + 1 MiB. The wheel download is bounded to
480 seconds; no pip installation. Partial files remain for review.
"""
import argparse
import json
from pathlib import Path
import re
from urllib.parse import urlparse
import zipfile

if __package__:
    from . import build_sherpa_asr as build
else:
    import build_sherpa_asr as build

VERSION = "4.2.3"
CACHE = build.ROOT / ".cache/tools"
BASE = "https://cmake.org/files/v4.2"
ARCHIVE = f"cmake-{VERSION}-windows-x86_64.zip"
CHECKSUMS = f"cmake-{VERSION}-SHA-256.txt"


def main():
    CACHE.mkdir(parents=True, exist_ok=True)
    checksums = CACHE / CHECKSUMS
    if not checksums.exists():
        build.download(f"{BASE}/{CHECKSUMS}", checksums, None, 1024**2)
    matches = re.findall(r"^([0-9a-f]{64})\s+" + re.escape(ARCHIVE) + r"$",
                         checksums.read_text(), re.M)
    if len(matches) != 1:
        raise ValueError("Official checksums lack exactly one Windows x64 ZIP entry")
    archive = CACHE / ARCHIVE
    build.download(f"{BASE}/{ARCHIVE}", archive, matches[0], 80 * 1024**2)
    destination = CACHE / f"cmake-{VERSION}"
    root = build.extract(archive, destination)
    executable = root / "bin/cmake.exe"
    build.run([str(executable), "--version"], build.WORK / "cmake-version.log", 10)
    build.write_json(CACHE / "cmake-tool.json", {
        "version": VERSION, "archive_url": f"{BASE}/{ARCHIVE}",
        "checksums_url": f"{BASE}/{CHECKSUMS}", "checksums": build.record(checksums),
        "archive": build.record(archive), "executable": str(executable),
        "verification": "official HTTPS SHA-256 list; not signature verification",
    })
    print(executable)


def wheel():
    """Use the upstream PyPI wheel as a portable archive, never pip install."""
    CACHE.mkdir(parents=True, exist_ok=True)
    metadata_url = f"https://pypi.org/pypi/cmake/{VERSION}/json"
    metadata = CACHE / f"cmake-{VERSION}-pypi.json"
    if not metadata.exists():
        build.download(metadata_url, metadata, None, 1024**2, timeout=60)
    info = json.loads(metadata.read_text(encoding="utf-8"))
    if info["info"]["version"] != VERSION or info["info"]["name"] != "cmake":
        raise ValueError("Unexpected PyPI package identity")
    wheels = [entry for entry in info["urls"]
              if entry["filename"].endswith("-win_amd64.whl")
              and entry["packagetype"] == "bdist_wheel" and not entry.get("yanked")]
    if len(wheels) != 1:
        raise ValueError("Expected exactly one official Windows x64 wheel")
    entry = wheels[0]
    url = urlparse(entry["url"])
    sha = entry["digests"]["sha256"]
    if (url.scheme != "https" or url.netloc != "files.pythonhosted.org"
            or not re.fullmatch(r"[0-9a-f]{64}", sha)
            or build.safe_name(entry["filename"]).name != entry["filename"]):
        raise ValueError("Unexpected wheel URL, filename or checksum")
    archive = CACHE / entry["filename"]
    build.download(entry["url"], archive, sha, 80 * 1024**2, timeout=480)
    if archive.stat().st_size != entry["size"]:
        raise ValueError("Wheel size differs from official metadata")
    destination = CACHE / f"cmake-{VERSION}-wheel"
    if destination.exists():
        raise ValueError("Retain/review existing wheel extraction before another attempt")
    with zipfile.ZipFile(archive) as source:
        members = source.infolist()
        if sum(m.file_size for m in members) > 512 * 1024**2:
            raise ValueError("Wheel expansion exceeds 512 MiB")
        for member in members:
            build.safe_name(member.filename)
            mode = member.external_attr >> 16
            if mode & 0o170000 not in (0, 0o100000, 0o040000):
                raise ValueError("Wheel contains a link or special file")
        destination.mkdir()
        source.extractall(destination)
    executable = destination / "cmake/data/bin/cmake.exe"
    build.run([str(executable), "--version"], build.WORK / "cmake-wheel-version.log", 10)
    if f"cmake version {VERSION}" not in (build.WORK / "cmake-wheel-version.log").read_text():
        raise ValueError("Extracted CMake version mismatch")
    build.write_json(CACHE / "cmake-wheel-tool.json", {
        "version": VERSION, "metadata_url": metadata_url, "metadata": build.record(metadata),
        "archive_url": entry["url"], "archive": build.record(archive),
        "executable": str(executable), "executable_identity": build.record(executable),
        "verification": "SHA-256 and size from official PyPI HTTPS metadata; no pip installation",
    })
    print(executable)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wheel", action="store_true", help="Use official PyPI Windows x64 wheel")
    args = parser.parse_args()
    wheel() if args.wheel else main()
