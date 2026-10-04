"""Prepare a pinned Sherpa 1.13.8 TTS-free native build; never changes Cargo.

Python 3.12+, curl, CMake (VS x64 generator), MSVC dumpbin are required.
Commands: plan (offline), fetch-source (bounded HTTPS bootstrap pin), probe
(HEAD only), fetch-deps (explicit aggregate budget), build (offline, fresh).
All writes stay in .cache/sherpa-asr and target/sherpa-asr. Build/install never
run Neo. No success manifest is emitted until configure, compile, install,
CMake graph inspection and native symbol checks ALL succeed. This is not a
license clearance or a final Neo link audit. Use --help for the build stages.
"""
from __future__ import annotations

import argparse
import hashlib
import gzip
import json
import ntpath
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import time
import zipfile

ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".cache/sherpa-asr"
WORK = ROOT / "target/sherpa-asr"
VERSION = "1.13.8"
COMMIT = "11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf"
SOURCE_URL = f"https://codeload.github.com/k2-fsa/sherpa-onnx/tar.gz/{COMMIT}"
SOURCE_FILE = f"sherpa-onnx-{COMMIT}.tar.gz"
SOURCE_SHA256 = "0a8db6c55dd318f4a688faba85f7760b99a6c92e8ef8864479d418531bee1ac2"
LIBS = (
    "sherpa-onnx-c-api", "sherpa-onnx-core", "kaldi-decoder-core",
    "sherpa-onnx-kaldifst-core", "sherpa-onnx-fstfar", "sherpa-onnx-fst",
    "kaldi-native-fbank-core", "kissfft-float", "onnxruntime", "ssentencepiece_core",
)
# The C API CMakeLists also installs a C++ wrapper; hash it, but do not link it.
INSTALLED_LIBS = (*LIBS, "sherpa-onnx-cxx-api")
OPTIONS = {
    "BUILD_SHARED_LIBS": "OFF", "CMAKE_BUILD_TYPE": "Release",
    "SHERPA_ONNX_USE_STATIC_CRT": "ON", "CMAKE_MSVC_RUNTIME_LIBRARY": "MultiThreaded",
    "SHERPA_ONNX_ENABLE_C_API": "ON", "SHERPA_ONNX_ENABLE_TTS": "OFF",
    "SHERPA_ONNX_ENABLE_PYTHON": "OFF", "SHERPA_ONNX_ENABLE_JNI": "OFF",
    "SHERPA_ONNX_ENABLE_BINARY": "OFF", "SHERPA_ONNX_BUILD_C_API_EXAMPLES": "OFF",
    "SHERPA_ONNX_ENABLE_TESTS": "OFF", "SHERPA_ONNX_ENABLE_PORTAUDIO": "OFF",
    "SHERPA_ONNX_ENABLE_WEBSOCKET": "OFF", "SHERPA_ONNX_ENABLE_SPEAKER_DIARIZATION": "OFF",
    "SHERPA_ONNX_ENABLE_GPU": "OFF", "SHERPA_ONNX_ENABLE_DIRECTML": "OFF",
    "SHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE": "OFF",
    "FETCHCONTENT_FULLY_DISCONNECTED": "ON", "FETCHCONTENT_UPDATES_DISCONNECTED": "ON",
}
# Hashes are from fixed upstream CMake recipes, not inferred from library names.
DEPS = {
    "kaldi_native_fbank": ("https://github.com/csukuangfj/kaldi-native-fbank/archive/refs/tags/v1.22.3.tar.gz", "9176cc66fc7ce1edf85cf355b06e320c57db6297df74277f575183468893cf61", "kaldi-native-fbank-1.22.3.tar.gz"),
    "kaldi_decoder": ("https://github.com/k2-fsa/kaldi-decoder/archive/refs/tags/v0.3.0.tar.gz", "b9f34cfb4fd3b1344100eead79ef4d37aa15962274b9e3056de345021f76a1b0", "kaldi-decoder-0.3.0.tar.gz"),
    "kaldifst": ("https://github.com/k2-fsa/kaldifst/archive/refs/tags/v1.8.0.tar.gz", "3f247b7e5a2409071202f5e2bc6200060f66728c0a3443c03923ad2723e040b3", "kaldifst-1.8.0.tar.gz"),
    "openfst": ("https://github.com/csukuangfj/openfst/archive/refs/tags/v1.8.5-2026-07-09.tar.gz", "2ff712a32952fcb01d351121a6bc8ccf4fdc6b2aa06ce8df2b3095dedd518c0e", "openfst-1.8.5-2026-07-09.tar.gz"),
    "eigen": ("https://gitlab.com/libeigen/eigen/-/archive/5.0.1/eigen-5.0.1.tar.gz", "e9c326dc8c05cd1e044c71f30f1b2e34a6161a3b6ecf445d56b53ff1669e3dec", "eigen-5.0.1.tar.gz"),
    "kissfft": ("https://github.com/mborgerding/kissfft/archive/febd4caeed32e33ad8b2e0bb5ea77542c40f18ec.zip", "497103e664168ebe39580b757adbe616f6cf85a16572af581ca7bc42d0ab13fd", "kissfft-febd4caeed32e33ad8b2e0bb5ea77542c40f18ec.zip"),
    "simple-sentencepiece": ("https://github.com/pkufool/simple-sentencepiece/archive/refs/tags/v0.7.tar.gz", "1748a822060a35baa9f6609f84efc8eb54dc0e74b9ece3d82367b7119fdc75af", "simple-sentencepiece-0.7.tar.gz"),
    "json": ("https://github.com/nlohmann/json/archive/refs/tags/v3.12.0.tar.gz", "4b92eb0c06d10683f7447ce9406cb97cd4b453be18d7279320f7b2f025c10187", "json-3.12.0.tar.gz"),
    "onnxruntime": ("https://github.com/csukuangfj/onnxruntime-libs/releases/download/v1.28.2/onnxruntime-win-x64-static_lib-MT-Release-1.28.2.tar.bz2", "77c6cc2a419828f450a570851d7d6d2385523c411dbe858f841a4c7338a78881", "onnxruntime-win-x64-static_lib-MT-Release-1.28.2.tar.bz2"),
}
# CreateSpeaker / OfflineSpeaker are ASR symbols, not eSpeak markers.
RISK = re.compile(r"espeak(?!er)|piper|phonemize|offline-tts|(?:^|[/\\])ucd(?:[./\\]|$)", re.I)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def record(path):
    return {"sha256": digest(path), "size": path.stat().st_size}


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def run(args, log, timeout=180):
    """Bound each command; no retry. Never kills unrelated/existing processes."""
    meta = {"args": [str(a) for a in args], "started": time.time(), "timeout_seconds": timeout}
    log.parent.mkdir(parents=True, exist_ok=True)
    for previous in (log, log.with_suffix(".command.json")):
        if previous.exists():
            saved = previous.with_name(previous.name + ".previous")
            number = 1
            while saved.exists():
                saved = previous.with_name(previous.name + f".{number}.previous")
                number += 1
            previous.rename(saved)
    write_json(log.with_suffix(".command.json"), meta)
    try:
        with log.open("wb") as output:
            result = subprocess.run(args, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT,
                                    timeout=timeout, check=False)
        meta["returncode"] = result.returncode
        if result.returncode:
            raise ValueError(f"Command failed ({result.returncode}); see {log}")
    except subprocess.TimeoutExpired:
        meta["timed_out"] = True
        meta["cleanup_status"] = "requires-manual-review"
        meta["descendants_may_be_running"] = True
        meta["automatic_retry"] = False
        raise ValueError(
            f"Command timed out after {timeout}s; descendants may still be running; "
            f"cleanup requires manual review, no automatic retry; see {log}"
        ) from None
    finally:
        meta["finished"] = time.time()
        write_json(log.with_suffix(".command.json"), meta)


def download(url, destination, expected, max_bytes, timeout=60):
    if destination.exists():
        if expected is None or digest(destination) != expected:
            raise ValueError(f"Unpinned/mismatching existing archive: {destination}")
        return
    partial = destination.with_name(destination.name + ".partial")
    if partial.exists():
        raise ValueError(f"Previous partial download retained; explicit review needed: {partial}")
    args = ["curl", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https",
            "--connect-timeout", "10", "--max-time", str(timeout), "--max-filesize", str(max_bytes),
            "--output", str(partial), url]
    run(args, WORK / (destination.name + ".download.log"), timeout + 10)
    if partial.stat().st_size > max_bytes or (expected and digest(partial) != expected):
        raise ValueError(f"Downloaded archive failed size/hash check: {partial}")
    partial.rename(destination)


def safe_name(name):
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
        raise ValueError(f"Unsafe archive path: {name}")
    return path


def extract(archive, destination, max_expanded=2 * 1024**3, omitted_links=None):
    """Never follow links; optionally inventory non-build Sherpa example links.

    The pinned Sherpa tarball includes even absolute developer-machine links.
    Only source-only example/script roots may be omitted, never native code,
    CMake inputs or license files. Dependencies still reject every link.
    """
    if destination.exists():
        raise ValueError(f"Refusing reused source directory: {destination}")
    destination.mkdir(parents=True)
    total = 0
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            for member in source.infolist():
                name = safe_name(member.filename)
                mode = member.external_attr >> 16
                if mode & 0o170000 == 0o120000:
                    raise ValueError("Archive symlinks are not supported")
                total += member.file_size
                if total > max_expanded:
                    raise ValueError("Expanded archive exceeds budget")
                source.extract(member, destination)
    else:
        with tarfile.open(archive) as source:
            for member in source:
                name = safe_name(member.name)
                if member.issym() and omitted_links is not None:
                    excluded_roots = {
                        ".github", "Sources", "android", "c-api-examples",
                        "dart-api-examples", "flutter-examples", "harmony-os",
                        "ios-swiftui", "kotlin-api-examples", "rust-api-examples",
                        "scripts", "wasm",
                    }
                    if (len(name.parts) >= 3 and name.parts[0] == f"sherpa-onnx-{COMMIT}"
                            and name.parts[1] in excluded_roots
                            and not re.search(r"license|copying|notice|copyright|cmake", name.name, re.I)):
                        omitted_links.append({"path": member.name, "target": member.linkname})
                        continue
                if not (member.isfile() or member.isdir()):
                    raise ValueError(f"Archive links/devices are not supported: {member.name}")
                total += member.size
                if total > max_expanded:
                    raise ValueError("Expanded archive exceeds budget")
                source.extract(member, destination, filter="data")
    children = list(destination.iterdir())
    if len(children) != 1 or not children[0].is_dir():
        raise ValueError("Expected exactly one archive root")
    return children[0]


def fetch_source():
    lock = CACHE / "source-lock.json"
    if lock.exists():
        check_source()
        return
    archive = CACHE / SOURCE_FILE
    download(SOURCE_URL, archive, SOURCE_SHA256, 64 * 1024**2, timeout=120)
    # Verify archive completeness as well as the tracked, known SHA-256.
    with gzip.open(archive, "rb") as stream:
        while stream.read(1024 * 1024):
            pass  # Read through the trailer: truncated gzip must not acquire a pin.
    with tarfile.open(archive) as source:
        roots = {safe_name(m.name).parts[0] for m in source if m.name}
    if roots != {f"sherpa-onnx-{COMMIT}"}:
        raise ValueError("Source archive root does not match pinned commit")
    write_json(lock, {"version": VERSION, "commit": COMMIT, "url": SOURCE_URL,
                      "archive": SOURCE_FILE, **record(archive),
                      "trust": "verified against tracked known SHA-256; not an upstream signed digest"})


def check_source():
    lock = json.loads((CACHE / "source-lock.json").read_text(encoding="utf-8"))
    if (lock["version"], lock["commit"], lock["url"], lock["archive"]) != (VERSION, COMMIT, SOURCE_URL, SOURCE_FILE):
        raise ValueError("Source lock identity mismatch")
    if lock["sha256"] != SOURCE_SHA256 or record(CACHE / SOURCE_FILE) != {k: lock[k] for k in ("sha256", "size")}:
        raise ValueError("Source hash/size mismatch")
    return lock


def probe():
    budget = {}
    for name, (url, sha, filename) in DEPS.items():
        log = WORK / (name + ".head.txt")
        try:
            run(["curl", "--head", "--fail", "--location", "--proto", "=https",
                 "--proto-redir", "=https", "--connect-timeout", "10", "--max-time", "20", url], log, 25)
            sizes = re.findall(r"^content-length:\s*(\d+)", log.read_text(errors="replace"), re.I | re.M)
            size = int(sizes[-1]) if sizes else None
            budget[name] = {"url": url, "size": size, "sha256": sha, "archive": filename}
        except ValueError as exc:
            budget[name] = {"url": url, "size": None, "error": str(exc)}
        write_json(WORK / "download-budget.json", budget)
    print(json.dumps(budget, indent=2))


def fetch_deps(max_bytes):
    # Caller explicitly chooses aggregate cap, even when HEAD cannot give lengths.
    used = 0
    for name, (url, sha, filename) in DEPS.items():
        archive = CACHE / filename
        if archive.exists():
            if digest(archive) != sha:
                raise ValueError(f"Hash mismatch: {name}")
            continue
        if max_bytes <= used:
            raise ValueError("Aggregate download budget exhausted")
        download(url, archive, sha, max_bytes - used)
        used += archive.stat().st_size


def check_options(source):
    text = (source / "CMakeLists.txt").read_text(encoding="utf-8")
    if f'set(SHERPA_ONNX_VERSION "{VERSION}")' not in text:
        raise ValueError("Unexpected Sherpa source version")
    for option in OPTIONS:
        if option.startswith("SHERPA_") and not re.search(r"option\(\s*" + option + r"\s", text):
            raise ValueError(f"Unknown upstream CMake option: {option}")
    if not re.search(r"if\(SHERPA_ONNX_ENABLE_TTS\)\s*include\(espeak-ng-for-piper\)", text):
        raise ValueError("Upstream TTS dependency guard changed")


def cache_values(path):
    result = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"([^/#][^:=]*):[^=]+=(.*)$", line)
        if match:
            result[match[1]] = match[2]
    return result


def graph_source_name(name, source_root, build_root, dependency_root):
    """Strip only known checkout roots, never a target's own (possibly TTS) dir."""
    path = ntpath.normpath(name)
    if not ntpath.isabs(path):
        path = ntpath.normpath(ntpath.join(source_root, path))
    for root in (source_root, build_root, dependency_root):
        root = ntpath.normpath(root)
        try:
            if ntpath.normcase(ntpath.commonpath([root, path])) == ntpath.normcase(root):
                return ntpath.relpath(path, root).replace("\\", "/")
        except ValueError:
            pass  # Different drive; not a permitted source root.
    raise ValueError(f"CMake source outside known native roots: {name}")


def check_graph(build):
    replies = build / ".cmake/api/v1/reply"
    index = json.loads(next(replies.glob("index-*.json")).read_text(encoding="utf-8"))
    model = next(o for o in index["objects"] if o["kind"] == "codemodel")
    model = json.loads((replies / model["jsonFile"]).read_text(encoding="utf-8"))
    source_root = model["paths"]["source"]
    build_root = model["paths"]["build"]
    dependency_root = ntpath.join(ntpath.dirname(build_root), "deps")
    targets = []
    for config in model["configurations"]:
        if config["name"] != "Release":
            continue
        for target in config["targets"]:
            detail = json.loads((replies / target["jsonFile"]).read_text(encoding="utf-8"))
            names = [detail["name"]] + [s["path"] for s in detail.get("sources", [])]
            checked_names = [detail["name"]] + [
                graph_source_name(s["path"], source_root, build_root, dependency_root)
                for s in detail.get("sources", [])
            ]
            if any(RISK.search(n) for n in checked_names):
                raise ValueError(f"Forbidden TTS target/source in CMake graph: {detail['name']}")
            targets.append({"name": detail["name"], "sources": names[1:]})
    if not {"sherpa-onnx-c-api", "sherpa-onnx-core"}.issubset({t["name"] for t in targets}):
        raise ValueError("Missing Release core/C API targets")
    sources = "\n".join(s for t in targets for s in t["sources"])
    for required in ("offline-sense-voice-model.cc", "silero-vad-model.cc", "voice-activity-detector.cc"):
        if required not in sources:
            raise ValueError(f"Missing required ASR/VAD source: {required}")
    return targets


def remaining(deadline, cap):
    seconds = min(cap, deadline - time.monotonic())
    if seconds <= 0:
        raise ValueError("Native build budget exhausted; no automatic retry")
    return seconds


COFF_SYMBOL = re.compile(r"^\s*[0-9A-F]+\s+[0-9A-F]+\s+(?:SECT[0-9A-F]+|UNDEF|ABS|DEBUG)\s+[^|]*\|\s*(.*)$", re.I)
ARCHIVE_MEMBER = re.compile(r"^\s*Archive member name at [0-9A-F]+:\s*(.*)$", re.I)


def symbol_risk_text(line):
    """Inspect symbol/member identities, not the host path in Dump of file."""
    symbol = COFF_SYMBOL.match(line)
    if symbol:
        return symbol[1]
    member = ARCHIVE_MEMBER.match(line)
    if member:
        name = member[1].strip().rstrip("/")
        return ntpath.basename(name) if ntpath.isabs(name) else name
    return ""


def validate_artifacts(libdir, dumpbin, logs, deadline=None):
    found = {p.stem for p in libdir.glob("*.lib")}
    if found != set(INSTALLED_LIBS):
        raise ValueError(f"Unexpected library set (do not filter): {sorted(found)}")
    artifacts = {}
    for name in INSTALLED_LIBS:
        library = libdir / (name + ".lib")
        with library.open("rb") as stream:
            magic = stream.read(8)
        if library.stat().st_size <= 8 or magic != b"!<arch>\n":
            raise ValueError(f"Not a COFF archive: {library}")
        log = logs / (name + ".symbols.txt")
        run([dumpbin, "/symbols", str(library)], log,
            remaining(deadline, 60) if deadline is not None else 60)
        required = {"SherpaOnnxCreateOfflineRecognizer", "SherpaOnnxCreateVoiceActivityDetector"}
        defined = set()
        # ORT's full symbol report exceeds 1 GiB; never decode it all at once.
        with log.open(encoding="utf-8", errors="replace") as stream:
            for line in stream:
                if RISK.search(symbol_risk_text(line)):
                    raise ValueError(f"TTS/eSpeak/Piper marker remains in native input: {library}: {line.strip()}")
                if name == "sherpa-onnx-c-api":
                    match = re.search(r"SECT[0-9A-F]+.*External\s+\|\s+(\w+)", line)
                    if match:
                        defined.add(match[1])
        if name == "sherpa-onnx-c-api" and not required.issubset(defined):
            raise ValueError(f"Required defined C API missing: {sorted(required - defined)}")
        artifacts[library.name] = record(library)
    return artifacts


def build(args):
    deadline = time.monotonic() + args.budget_seconds
    if os.name != "nt":
        raise ValueError("Only Windows x64 MSVC native builds are supported")
    cmake = shutil.which(args.cmake)
    dumpbin = shutil.which(args.dumpbin)
    if not cmake or not dumpbin:
        raise ValueError("CMake and MSVC dumpbin must exist; no tools are auto-installed")
    lock = check_source()
    for name, (_, sha, filename) in DEPS.items():
        if not (CACHE / filename).is_file() or digest(CACHE / filename) != sha:
            raise ValueError(f"Missing or mismatching pinned dependency: {name}; no configure/download fallback")
    attempt = WORK / "native"
    if attempt.exists():
        raise ValueError("Native attempt already exists; retain/review it before explicitly choosing another attempt")
    attempt.mkdir()
    omitted_links = []
    source = extract(CACHE / SOURCE_FILE, attempt / "source", omitted_links=omitted_links)
    write_json(attempt / "omitted-source-links.json", omitted_links)
    check_options(source)
    inputs = {}
    for name, (_, sha, filename) in DEPS.items():
        inputs[name] = extract(CACHE / filename, attempt / "deps" / name)
    # Sherpa prepends its modules: these override the older child recipes.
    for name in ("eigen", "openfst"):
        recipe = (source / "cmake" / (name + ".cmake")).read_text(encoding="utf-8")
        if DEPS[name][0] not in recipe or DEPS[name][1] not in recipe:
            raise ValueError(f"Effective top-level recipe mismatch: {name}")
    recipes = "\n".join(p.read_text(encoding="utf-8", errors="replace")
                        for tree in [source, *inputs.values()]
                        for p in tree.rglob("*.cmake"))
    for name, (_, sha, _) in DEPS.items():
        if sha not in recipes:
            raise ValueError(f"Pinned dependency hash missing from verified recipes: {name}")
    build_dir = attempt / "build"
    query = build_dir / ".cmake/api/v1/query"
    query.mkdir(parents=True)
    (query / "codemodel-v2").touch()
    prefix = attempt / "install"
    command = [cmake, "-S", str(source), "-B", str(build_dir), "-G", args.generator,
               "-A", "x64", f"-DCMAKE_INSTALL_PREFIX={prefix}"]
    if args.vs_instance:
        command.append(f"-DCMAKE_GENERATOR_INSTANCE={args.vs_instance}")
    command += [f"-D{k}={v}" for k, v in OPTIONS.items()]
    command += [f"-DFETCHCONTENT_SOURCE_DIR_{k.upper()}={v}" for k, v in inputs.items()]
    run(command, attempt / "configure.log", remaining(deadline, 180))
    values = cache_values(build_dir / "CMakeCache.txt")
    if any(values.get(k) != v for k, v in OPTIONS.items()):
        raise ValueError("CMake cache does not match required no-TTS options")
    graph = check_graph(build_dir)
    write_json(attempt / "graph.json", graph)
    run([cmake, "--build", str(build_dir), "--config", "Release", "--parallel", "2"], attempt / "build.log", remaining(deadline, 600))
    run([cmake, "--install", str(build_dir), "--config", "Release"], attempt / "install.log", remaining(deadline, 180))
    finish_validation(attempt, source, inputs, lock, dumpbin, deadline)


def finish_validation(attempt, source, inputs, lock, dumpbin, deadline):
    prefix = attempt / "install"
    libdir = prefix / "lib"
    artifacts = validate_artifacts(libdir, dumpbin, attempt / "symbols", deadline)
    # Retain licenses for ALL downloaded inputs, including the prebuilt ORT input.
    license_records = {}
    for name, tree in {"sherpa": source, **inputs}.items():
        for path in tree.rglob("*"):
            if path.is_file() and re.match(r"^(license|copying|notice|copyright|thirdpartynotice)", path.name, re.I):
                dest = prefix / "licenses" / name / path.relative_to(tree)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, dest)
                license_records[dest.relative_to(prefix).as_posix()] = record(dest)
    if not any(k.startswith("licenses/sherpa/") for k in license_records):
        raise ValueError("Missing Sherpa license")
    receipt = {"source_lock": lock, "dependency_archives": {k: {"url": v[0], "sha256": v[1]} for k, v in DEPS.items()},
               "options": OPTIONS, "graph": record(attempt / "graph.json"), "licenses": license_records,
                              "omitted_source_links": record(attempt / "omitted-source-links.json"),
               "commands": {p.name: json.loads(p.read_text(encoding="utf-8")) for p in attempt.glob("*.command.json")},
               "symbol_reports": {p.name: record(p) for p in (attempt / "symbols").glob("*.symbols.txt")},
               "builder": record(Path(__file__))}
    write_json(libdir / "neo-asr-receipt.json", receipt)
    manifest = {"schema": 1, "status": "native-validated", "version": VERSION, "source_commit": COMMIT,
                "source_sha256": lock["sha256"], "target": "x86_64-pc-windows-msvc", "configuration": "Release",
                "options": OPTIONS, "libraries": artifacts, "receipt": record(libdir / "neo-asr-receipt.json")}
    # Last operation: failures never leave a ready manifest.
    remaining(deadline, 1)
    write_json(libdir / "neo-sherpa-asr.json", manifest)
    print(f"Native candidate validated: {libdir}; final Neo link audit still required, Cargo unchanged")


def validate_existing(args):
    """Recheck a completed build without rerunning configure/compile/install."""
    deadline = time.monotonic() + args.budget_seconds
    attempt = WORK / "native"
    if (attempt / "install/lib/neo-sherpa-asr.json").exists():
        raise ValueError("Validated manifest already exists; do not overwrite evidence")
    lock = check_source()
    for name, (_, sha, filename) in DEPS.items():
        if digest(CACHE / filename) != sha:
            raise ValueError(f"Dependency hash mismatch: {name}")
    for stage in ("configure", "build", "install"):
        receipt = json.loads((attempt / (stage + ".command.json")).read_text(encoding="utf-8"))
        if receipt.get("returncode") != 0 or receipt.get("timed_out"):
            raise ValueError(f"No successful stage: {stage}")
    values = cache_values(attempt / "build/CMakeCache.txt")
    if any(values.get(k) != v for k, v in OPTIONS.items()):
        raise ValueError("Existing CMake options mismatch")
    graph = check_graph(attempt / "build")
    if graph != json.loads((attempt / "graph.json").read_text(encoding="utf-8")):
        raise ValueError("Existing graph mismatch")
    source = attempt / "source" / f"sherpa-onnx-{COMMIT}"
    check_options(source)
    inputs = {name: next((attempt / "deps" / name).iterdir()) for name in DEPS}
    dumpbin = shutil.which(args.dumpbin)
    if not dumpbin:
        raise ValueError("MSVC dumpbin must exist")
    finish_validation(attempt, source, inputs, lock, dumpbin, deadline)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["plan", "fetch-source", "probe", "fetch-deps", "build", "validate"])
    parser.add_argument("--max-download-mib", type=int, default=0,
                        help="Explicit aggregate network budget for fetch-deps; default forbids downloads")
    parser.add_argument("--cmake", default="cmake")
    parser.add_argument("--dumpbin", default="dumpbin")
    parser.add_argument("--generator", default="Visual Studio 18 2026")
    parser.add_argument("--vs-instance", help="Explicit CMake VS instance path[,version=...]")
    parser.add_argument("--budget-seconds", type=int, default=600,
                        help="Total native preparation/configure/build/validation budget; no retry")
    args = parser.parse_args(argv)
    CACHE.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    try:
        if args.command == "plan":
            plan = {"version": VERSION, "commit": COMMIT, "source_url": SOURCE_URL,
                    "source_lock_exists": (CACHE / "source-lock.json").exists(), "options": OPTIONS,
                    "dependencies": {k: {"url": v[0], "sha256": v[1], "archive": v[2]} for k, v in DEPS.items()},
                    "cmake": shutil.which(args.cmake), "dumpbin": shutil.which(args.dumpbin),
                    "note": "No native manifest / no enable recommendation until complete successful build"}
            write_json(WORK / "plan.json", plan)
            print(json.dumps(plan, indent=2))
        elif args.command == "fetch-source":
            fetch_source()
        elif args.command == "probe":
            probe()
        elif args.command == "fetch-deps":
            if args.max_download_mib <= 0:
                raise ValueError("fetch-deps needs an explicit positive --max-download-mib after budget review")
            fetch_deps(args.max_download_mib * 1024**2)
        elif args.command == "validate":
            validate_existing(args)
        else:
            build(args)
    except (OSError, ValueError, KeyError, EOFError, tarfile.TarError, zipfile.BadZipFile, StopIteration) as exc:
        write_json(WORK / "last-failure.json", {"command": args.command, "error": str(exc),
                                               "status": "blocked", "time": time.time()})
        print(f"BLOCKED: {exc}")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
