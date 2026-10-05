"""Recipe-bound fresh native evidence; never rebuild, rewrite receipts or approve.

The lock pins a canonical recipe, including the current builder and validators.
Build timestamps, host paths and output hashes are deliberately not recipe pins:
all current outputs must instead pass the existing validators and be rehashed.
Run `python -m tools.native_source_binding` to print the binding for review.
"""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import stat
import tarfile
import tempfile
import time
import zipfile

if __package__:
    from . import build_sherpa_asr as build
    from . import native_source_bundle as native
    from . import prepare_sherpa_ci as ci
    from .runtime_sources import check_path, file_record, load_json, safe_relative
else:
    import build_sherpa_asr as build
    import native_source_bundle as native
    import prepare_sherpa_ci as ci
    from runtime_sources import check_path, file_record, load_json, safe_relative

ROOT = Path(__file__).resolve().parents[1]
RECIPE_FILES = (
    "tools/build_sherpa_asr.py", "tools/prepare_sherpa_ci.py",
    "vendor/sherpa-onnx-sys/neo_asr.rs", "tools/native_source_bundle.py",
    "tools/native_source_binding.py", "tools/runtime_sources.py",
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def recipe():
    return {
        "schema": 1, "version": build.VERSION, "source_commit": build.COMMIT,
        "source_sha256": build.SOURCE_SHA256, "source_url": build.SOURCE_URL,
        "target": "x86_64-pc-windows-msvc", "configuration": "Release",
        "options": build.OPTIONS,
        "dependency_archives": {k: {"url": v[0], "sha256": v[1], "archive": v[2]}
                                for k, v in build.DEPS.items()},
        "libraries": sorted(name + ".lib" for name in build.INSTALLED_LIBS),
        "validators": {name: file_record(ROOT / name) for name in RECIPE_FILES},
    }


def expected_binding():
    data = json.dumps(recipe(), sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("utf-8")
    return {"kind": "recipe", "schema": 1, "recipe_sha256": hashlib.sha256(data).hexdigest()}


def check_binding(binding):
    require(binding == expected_binding(),
            "Native recipe differs from reviewed lock; a new binding needs explicit review")


def regular_tree(root, deadline):
    """Check before descent, including Windows reparse points and hard links."""
    check_path(root)
    require(root.is_dir(), f"Missing native evidence tree: {root}")
    for path in root.iterdir():
        build.remaining(deadline, 180)
        check_path(path)
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            regular_tree(path, deadline)
        else:
            require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1,
                    f"Non-regular/linked native evidence: {path}")


def same_path(value, path):
    require(isinstance(value, str) and Path(value).resolve() == path.resolve(),
            f"Native build source/path mismatch: {path}")


def check_commands(receipt, source, builddir, prefix, inputs):
    commands = receipt["commands"]
    args = commands["configure.command.json"].get("args")
    require(isinstance(args, list) and all(isinstance(a, str) and a for a in args)
            and len(args) >= 10, "Invalid native configure command args")
    require(args[1:2] == ["-S"] and args[3:4] == ["-B"]
            and args[5:6] == ["-G"] and args[6] in ci.GENERATORS.values()
            and args[7:9] == ["-A", "x64"], "Native configure command structure mismatch")
    same_path(args[2], source)
    same_path(args[4], builddir)
    definitions = {}
    for arg in args[9:]:
        require(arg.startswith("-D") and "=" in arg, "Unexpected native configure argument")
        key, value = arg[2:].split("=", 1)
        require(key not in definitions, "Duplicate native configure definition")
        definitions[key] = value
    expected = {**build.OPTIONS, "CMAKE_INSTALL_PREFIX": str(prefix),
                **{"FETCHCONTENT_SOURCE_DIR_" + k.upper(): str(v) for k, v in inputs.items()}}
    instance = definitions.pop("CMAKE_GENERATOR_INSTANCE", None)
    require(instance is None or bool(instance.strip()), "Empty native VS instance")
    require(set(definitions) == set(expected), "Native configure definition set mismatch")
    for key, value in expected.items():
        if key == "CMAKE_INSTALL_PREFIX" or key.startswith("FETCHCONTENT_SOURCE_DIR_"):
            same_path(definitions[key], Path(value))
        else:
            require(definitions[key] == value, f"Native configure option mismatch: {key}")
    for stage, suffix in (("build", ["--parallel", "2"]), ("install", [])):
        command = commands[stage + ".command.json"].get("args")
        require(isinstance(command, list) and len(command) == 5 + len(suffix)
                and command[0] == args[0] and command[1] == "--" + stage
                and command[3:] == ["--config", "Release", *suffix],
                f"Native {stage} command args mismatch")
        same_path(command[2], builddir)


def tree_records(root, deadline):
    regular_tree(root, deadline)
    result = {}
    for path in root.rglob("*"):
        if path.is_file():
            build.remaining(deadline, 180)
            result[path.relative_to(root).as_posix()] = file_record(path)
    return result


def check_sherpa_source(source, attempt, work, deadline):
    # Reuse the builder's exact symlink omission policy, rather than inventing a
    # second allowlist. Never execute the extracted CMake or source files.
    with tempfile.TemporaryDirectory(prefix="native-source-", dir=work) as temporary:
        omitted = []
        original = build.extract(build.CACHE / build.SOURCE_FILE, Path(temporary) / "source",
                                 omitted_links=omitted)
        require(original.name == source.name, "Sherpa source archive root mismatch")
        require(tree_records(source, deadline) == tree_records(original, deadline),
                "Sherpa source differs from complete pinned archive")
        require(json.loads((attempt / "omitted-source-links.json").read_text(encoding="utf-8")) == omitted,
                "Sherpa omitted source links differ from pinned archive")


def cmake_input(name):
    name = name.lower()
    return name.rsplit("/", 1)[-1] == "cmakelists.txt" or name.endswith((".cmake", ".cmake.in"))


def check_dependency_inputs(name, tree, used, deadline):
    """Pin all dependency CMake inputs and each source compiled from that tree."""
    regular_tree(tree, deadline)
    selected = {p.relative_to(tree).as_posix() for p in tree.rglob("*")
                if p.is_file() and cmake_input(p.relative_to(tree).as_posix())} | used
    expected = {}
    archive = build.CACHE / build.DEPS[name][2]

    def record(member_name, stream):
        path = build.safe_name(member_name)
        require(path.parts and path.parts[0] == tree.name, "Dependency archive root mismatch")
        relative = "/".join(path.parts[1:])
        if relative in selected or cmake_input(relative):
            build.remaining(deadline, 180)
            require(relative not in expected and stream is not None, "Duplicate/non-regular dependency input")
            with stream() as content:
                digest = hashlib.file_digest(content, "sha256").hexdigest()
            expected[relative] = digest

    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as opened:
            for member in opened.infolist():
                if not member.is_dir():
                    record(member.filename, lambda m=member: opened.open(m))
    else:
        with tarfile.open(archive) as opened:
            for member in opened:
                if not member.isdir():
                    record(member.name, (lambda m=member: opened.extractfile(m)) if member.isfile() else None)
    require(set(expected) == selected, f"Dependency CMake/source input set mismatch: {name}")
    for relative, digest in expected.items():
        require(file_record(tree / safe_relative(relative))["sha256"] == digest,
                f"Dependency CMake/source input changed: {name}/{relative}")
        build.remaining(deadline, 180)


def check_effective_inputs(model, replies, source, inputs, deadline):
    roots = [source, *inputs.values()]
    eigen = inputs["eigen"].resolve()
    used = {name: set() for name in inputs}
    core_seen = False

    def input_path(value, *, include=False):
        path = Path(value)
        path = (path if path.is_absolute() else source / path)
        check_path(path)
        path = path.resolve()
        require(any(path.is_relative_to(root.resolve()) for root in roots),
                f"Unreviewed native effective input: {value}")
        # Upstream kaldi-native-fbank lists a nonexistent kissfft subdirectory.
        # It cannot provide a header, but must still stay inside reviewed roots.
        require(include or path.exists(), f"Missing native effective input: {value}")
        return path

    def no_shadow(directory):
        # Eigen uses both <Eigen/...> and <unsupported/Eigen/...>. A source's
        # directory can precede /I for quoted includes, regardless of /I order.
        for relative in ("Eigen", "unsupported/Eigen"):
            candidate = directory / relative
            check_path(candidate)
            require(not candidate.exists() or candidate.resolve().is_relative_to(eigen),
                    f"Eigen include shadow before verified tree: {candidate}")

    for config in model["configurations"]:
        if config["name"] != "Release":
            continue
        for target in config["targets"]:
            detail = load_json(replies / safe_relative(target["jsonFile"]))
            groups = detail.get("compileGroups", [])
            if detail["name"] == "sherpa-onnx-core":
                require(groups, "Missing core effective compile groups")
                core_seen = True
            sources = detail.get("sources", [])
            for index, entry in enumerate(sources):
                if "compileGroupIndex" not in entry:
                    require(Path(entry["path"]).suffix.lower() not in (".c", ".cc", ".cpp", ".cxx", ".c++"),
                            "Native compile source missing effective compile group")
                    continue
                require(type(entry["compileGroupIndex"]) is int
                        and 0 <= entry["compileGroupIndex"] < len(groups), "Invalid source compile group")
                require(index in groups[entry["compileGroupIndex"]].get("sourceIndexes", []),
                        "Native compile group/source index mismatch")
                path = input_path(entry["path"])
                require(path.is_file(), "Compiled native source is not a file")
                no_shadow(path.parent)
                for name, tree in inputs.items():
                    if path.is_relative_to(tree.resolve()):
                        used[name].add(path.relative_to(tree.resolve()).as_posix())
            for group_index, group in enumerate(groups):
                indices = group.get("sourceIndexes", [])
                require(indices and len(set(indices)) == len(indices)
                        and all(type(i) is int and 0 <= i < len(sources)
                                and sources[i].get("compileGroupIndex") == group_index for i in indices),
                        "Native compile group/source index mismatch")
                # CMake represents normal /I switches in includes. Reject raw
                # switches/response files that could override that ordered list.
                for fragment in group.get("compileCommandFragments", []):
                    require(not re.search(r'(?:^|\s)(?:[-/]I|[-/]external:I|[-/]FI|[-/]imsvc|-(?:isystem|iquote|include)|@)',
                                          fragment["fragment"], re.I),
                            "Unreviewed native include/forced-include compiler flag")
                includes = [input_path(item["path"], include=True) for item in group.get("includes", [])]
                require(all(not p.exists() or p.is_dir() for p in includes), "Native include is not a directory")
                if detail["name"] == "sherpa-onnx-core":
                    require(eigen in includes, "Missing verified Eigen effective include")
                # Reject alternate Eigen providers even after the genuine tree:
                # absent headers must not fall through to another Eigen version.
                for directory in includes:
                    no_shadow(directory)
    require(core_seen, "Missing core effective compile groups")
    for name, tree in inputs.items():
        check_dependency_inputs(name, tree, used[name], deadline)


def verify(binding, bundle, libdir, work, budget_seconds=180):
    check_binding(binding)
    deadline = time.monotonic() + budget_seconds
    attempt = libdir.parent.parent
    require(libdir == attempt / "install/lib", "Expected native install/lib layout")
    # validate_install rejects missing/extra libraries and rehashes every library,
    # receipt, graph, symbol report and license. Check paths before it opens them.
    for tree in (libdir, attempt / "symbols", libdir.parent / "licenses"):
        regular_tree(tree, deadline)
    for name in ("graph.json", "omitted-source-links.json"):
        check_path(attempt / name)
    ci.validate_install(libdir, build.remaining(deadline, budget_seconds))
    manifest = load_json(libdir / "neo-sherpa-asr.json")
    receipt = load_json(libdir / "neo-asr-receipt.json")
    require(receipt["builder"] == file_record(Path(build.__file__)),
            "Native receipt builder differs from current pinned builder")
    for stage in ("configure", "build", "install"):
        name = stage + ".command.json"
        require(load_json(attempt / name) == receipt["commands"][name],
                f"Native command receipt changed: {stage}")

    check_path(build.CACHE / "source-lock.json")
    check_path(build.CACHE / build.SOURCE_FILE)
    lock = build.check_source()
    require(receipt["source_lock"] == lock, "Native source record differs from current source lock")
    for name, (_, digest, filename) in build.DEPS.items():
        require(file_record(build.CACHE / filename)["sha256"] == digest,
                f"Native dependency archive changed: {name}")
        build.remaining(deadline, budget_seconds)

    source = attempt / "source" / f"sherpa-onnx-{build.COMMIT}"
    check_path(source / "CMakeLists.txt")
    build.check_options(source)
    builddir = attempt / "build"
    check_path(builddir / "CMakeCache.txt")
    values = build.cache_values(builddir / "CMakeCache.txt")
    require(all(values.get(k) == v for k, v in build.OPTIONS.items()),
            "Native CMake cache no-TTS options mismatch")
    same_path(values.get("CMAKE_HOME_DIRECTORY"), source)
    same_path(values.get("CMAKE_INSTALL_PREFIX"), libdir.parent)
    inputs = {}
    for name in build.DEPS:
        directory = attempt / "deps" / name
        check_path(directory)
        children = list(directory.iterdir())
        require(len(children) == 1 and children[0].is_dir(), f"Unexpected dependency source root: {name}")
        check_path(children[0])
        same_path(values.get("FETCHCONTENT_SOURCE_DIR_" + name.upper()), children[0])
        inputs[name] = children[0]
    check_commands(receipt, source, builddir, libdir.parent, inputs)

    replies = builddir / ".cmake/api/v1/reply"
    regular_tree(replies, deadline)
    indices = list(replies.glob("index-*.json"))
    require(len(indices) == 1, "Ambiguous/missing native CMake graph index")
    index = load_json(indices[0])
    models = [o for o in index["objects"] if o["kind"] == "codemodel"]
    require(len(models) == 1, "Ambiguous/missing native codemodel")
    model = load_json(replies / safe_relative(models[0]["jsonFile"]))
    same_path(model["paths"]["source"], source)
    same_path(model["paths"]["build"], builddir)
    for config in model["configurations"]:
        for target in config["targets"]:
            safe_relative(target["jsonFile"])
    require(build.check_graph(builddir) == json.loads((attempt / "graph.json").read_text(encoding="utf-8")),
            "Native current CMake graph differs from receipt graph")

    # native.verify checks the immutable ZIP's historical source-records, NOT the
    # hash of this fresh build. Tie its exact original archive to today's tree.
    filename = "eigen-5.0.1.tar.gz"
    spec = native.SOURCES[filename]
    with zipfile.ZipFile(bundle) as archive:
        record = json.loads(archive.read("source-records.json"))
        require(record["sources"] == native.SOURCES, "Native companion source record mismatch")
        inventory = native.archive_inventory(archive.read("sources/" + filename), filename,
                                             spec["sha256"], spec["root"])
    require(len(inventory) == spec["files"], "Native Eigen source count mismatch")
    eigen = attempt / "deps/eigen" / spec["root"]
    regular_tree(eigen, deadline)
    native.compare_tree(eigen, inventory)
    check_sherpa_source(source, attempt, work, deadline)
    check_effective_inputs(model, replies, source, inputs, deadline)

    # Re-run symbol checks against actual libraries; never trust a self-consistent
    # manifest/report pair, nor overwrite the builder's original symbol evidence.
    _, _, dumpbin = ci.discover_vs()
    with tempfile.TemporaryDirectory(prefix="native-symbols-", dir=work) as temporary:
        libraries = build.validate_artifacts(libdir, dumpbin, Path(temporary), deadline)
    require(libraries == manifest["libraries"], "Native libraries changed during symbol validation")
    require(file_record(attempt / "graph.json") == receipt["graph"], "Native graph changed during validation")
    build.remaining(deadline, budget_seconds)


if __name__ == "__main__":
    print(json.dumps(expected_binding(), indent=2))
