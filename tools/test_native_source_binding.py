"""Offline recipe failures with real validators and synthetic build artifacts.

Only the external dumpbin/vswhere boundary is substituted. No real compilation,
network, source ZIP rewrite, receipt recovery or release approval occurs.
"""
import copy
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from tools import native_source_binding as binding

build = binding.build
native = binding.native


def ci_generator():
    return binding.ci.GENERATORS[17]


class RecipeBindingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.attempt = self.root / "native"
        self.lib = self.attempt / "install/lib"
        self.lib.mkdir(parents=True)
        self.cache = self.root / "cache"
        self.cache.mkdir()
        self.patch(build, "CACHE", self.cache)
        self.work = self.root / "validation"
        self.work.mkdir()
        self.source = self.attempt / "source" / f"sherpa-onnx-{build.COMMIT}"
        self.source.mkdir(parents=True)
        cmake = f'set(SHERPA_ONNX_VERSION "{build.VERSION}")\n'
        cmake += "\n".join(f"option({k} fixture OFF)" for k in build.OPTIONS if k.startswith("SHERPA_"))
        cmake += "\nif(SHERPA_ONNX_ENABLE_TTS)\ninclude(espeak-ng-for-piper)\nendif()\n"
        self.source_files = {"CMakeLists.txt": cmake.encode(), **{
            name: b"synthetic pinned source" for name in
            ("offline-sense-voice-model.cc", "silero-vad-model.cc", "voice-activity-detector.cc")}}
        for name, data in self.source_files.items():
            (self.source / name).write_bytes(data)
        archive = self.archive(build.SOURCE_FILE, self.source.name, self.source_files)
        self.patch(build, "SOURCE_SHA256", build.digest(archive))
        self.source_lock = {"version": build.VERSION, "commit": build.COMMIT, "url": build.SOURCE_URL,
                            "archive": build.SOURCE_FILE, **build.record(archive), "trust": "synthetic test only"}
        build.write_json(self.cache / "source-lock.json", self.source_lock)
        dependencies = {}
        paths = {}
        for name, (url, _, filename) in build.DEPS.items():
            tree = self.attempt / "deps" / name / ("eigen-5.0.1" if name == "eigen" else name)
            tree.mkdir(parents=True)
            files = {"header.h": b"synthetic original source", "CMakeLists.txt": b"# pinned CMake"}
            if name == "eigen":
                files["Eigen/Core"] = b"synthetic Eigen header"
            for relative, data in files.items():
                dest = tree / relative
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(data)
            archive = self.archive(filename, tree.name, files)
            dependencies[name] = (url, build.digest(archive), filename)
            paths[name] = tree
        self.patch(build, "DEPS", dependencies)
        specs = copy.deepcopy(native.SOURCES)
        specs["eigen-5.0.1.tar.gz"].update(sha256=dependencies["eigen"][1], files=3)
        self.patch(native, "SOURCES", specs)
        self.eigen = paths["eigen"]
        self.inputs = paths
        self.bundle = self.root / "source.zip"
        self.write_bundle()
        self.builddir = self.attempt / "build"
        self.replies = self.builddir / ".cmake/api/v1/reply"
        self.replies.mkdir(parents=True)
        values = {**build.OPTIONS, "CMAKE_HOME_DIRECTORY": str(self.source),
                  "CMAKE_INSTALL_PREFIX": str(self.lib.parent),
                  **{"FETCHCONTENT_SOURCE_DIR_" + k.upper(): str(v) for k, v in paths.items()}}
        self.cache_text = "\n".join(f"{k}:STRING={v}" for k, v in values.items())
        (self.builddir / "CMakeCache.txt").write_text(self.cache_text)
        build.write_json(self.replies / "index-fixture.json", {"objects": [{"kind": "codemodel", "jsonFile": "model.json"}]})
        build.write_json(self.replies / "model.json", {"paths": {"source": str(self.source), "build": str(self.builddir)},
                         "configurations": [{"name": "Release", "targets": [{"jsonFile": "api.json"}, {"jsonFile": "core.json"}]}]})
        build.write_json(self.replies / "api.json", {"name": "sherpa-onnx-c-api", "sources": []})
        build.write_json(self.replies / "core.json", {"name": "sherpa-onnx-core", "sources": [
            {"path": name, "compileGroupIndex": 0} for name in
            ("offline-sense-voice-model.cc", "silero-vad-model.cc", "voice-activity-detector.cc")],
            "compileGroups": [{"sourceIndexes": [0, 1, 2], "language": "CXX",
                               "includes": [{"path": str(self.source)}, {"path": str(self.eigen)}]}]})
        build.write_json(self.attempt / "graph.json", build.check_graph(self.builddir))
        build.write_json(self.attempt / "omitted-source-links.json", [])
        license = self.lib.parent / "licenses/sherpa/LICENSE"
        license.parent.mkdir(parents=True)
        license.write_text("synthetic license")
        libraries, reports = {}, {}
        for name in build.INSTALLED_LIBS:
            library = self.lib / (name + ".lib")
            library.write_bytes(b"!<arch>\nsynthetic native output")
            libraries[library.name] = build.record(library)
            report = self.attempt / "symbols" / (name + ".symbols.txt")
            report.parent.mkdir(exist_ok=True)
            report.write_bytes(self.symbols())
            reports[report.name] = build.record(report)
        configure = ["synthetic-cmake", "-S", str(self.source), "-B", str(self.builddir),
                     "-G", ci_generator(), "-A", "x64", "-DCMAKE_INSTALL_PREFIX=" + str(self.lib.parent)]
        configure += [f"-D{k}={v}" for k, v in build.OPTIONS.items()]
        configure += [f"-DFETCHCONTENT_SOURCE_DIR_{k.upper()}={v}" for k, v in paths.items()]
        commands = {s + ".command.json": {"returncode": 0, "started": 17, "args": args}
                    for s, args in (("configure", configure),
                                    ("build", ["synthetic-cmake", "--build", str(self.builddir), "--config", "Release", "--parallel", "2"]),
                                    ("install", ["synthetic-cmake", "--install", str(self.builddir), "--config", "Release"]))}
        self.receipt = {"source_lock": self.source_lock, "options": copy.deepcopy(build.OPTIONS),
                        "dependency_archives": {k: {"url": v[0], "sha256": v[1]} for k, v in build.DEPS.items()},
                        "builder": build.record(Path(build.__file__)), "commands": commands,
                        "licenses": {"licenses/sherpa/LICENSE": build.record(license)},
                        "symbol_reports": reports, "graph": build.record(self.attempt / "graph.json"),
                        "omitted_source_links": build.record(self.attempt / "omitted-source-links.json")}
        self.manifest = {"schema": 1, "status": "native-validated", "version": build.VERSION,
                         "source_commit": build.COMMIT, "source_sha256": build.SOURCE_SHA256,
                         "target": "x86_64-pc-windows-msvc", "configuration": "Release",
                         "options": copy.deepcopy(build.OPTIONS), "libraries": libraries}
        self.save_receipt()
        self.pin = binding.expected_binding()
        self.dump_text = self.symbols()
        self.runner = self.patch(build.subprocess, "run", side_effect=self.dumpbin)
        self.discovery = self.patch(binding.ci, "discover_vs", return_value=("fixture", "fixture", "synthetic-dumpbin"))

    def patch(self, obj, name, *args, **kwargs):
        patcher = patch.object(obj, name, *args, **kwargs)
        self.addCleanup(patcher.stop)
        return patcher.start()

    def archive(self, filename, root, files):
        path = self.cache / filename
        if filename.endswith(".zip"):
            with zipfile.ZipFile(path, "w") as out:
                for name, data in files.items():
                    out.writestr(root + "/" + name, data)
        else:
            mode = "w:bz2" if filename.endswith(".bz2") else "w:gz"
            with tarfile.open(path, mode) as out:
                for name, data in files.items():
                    info = tarfile.TarInfo(root + "/" + name)
                    info.size = len(data)
                    out.addfile(info, io.BytesIO(data))
        return path

    def write_bundle(self, record=None):
        with zipfile.ZipFile(self.bundle, "w") as out:
            out.writestr("source-records.json", json.dumps(record or {"sources": native.SOURCES}))
            out.write(self.cache / "eigen-5.0.1.tar.gz", "sources/eigen-5.0.1.tar.gz")

    @staticmethod
    def symbols():
        return (b"000 000 SECT1 notype External | SherpaOnnxCreateOfflineRecognizer\n"
                b"001 000 SECT1 notype External | SherpaOnnxCreateVoiceActivityDetector\n")

    def dumpbin(self, args, **kwargs):
        self.assertEqual(args[:2], ["synthetic-dumpbin", "/symbols"])
        self.assertGreater(kwargs["timeout"], 0)
        kwargs["stdout"].write(self.dump_text)
        return subprocess.CompletedProcess(args, 0)

    def save_receipt(self):
        for name, command in self.receipt["commands"].items():
            build.write_json(self.attempt / name, command)
        build.write_json(self.lib / "neo-asr-receipt.json", self.receipt)
        self.manifest["receipt"] = build.record(self.lib / "neo-asr-receipt.json")
        build.write_json(self.lib / "neo-sherpa-asr.json", self.manifest)

    def verify(self):
        binding.verify(self.pin, self.bundle, self.lib, self.work)

    def test_fresh_receipt_paths_timestamp_graph_and_libraries_are_not_historical_pins(self):
        before = {p: p.read_bytes() for p in self.attempt.rglob("*") if p.is_file()}
        self.verify()
        self.assertEqual(before, {p: p.read_bytes() for p in self.attempt.rglob("*") if p.is_file()})
        self.assertEqual(self.runner.call_count, len(build.INSTALLED_LIBS))
        for name, value in self.receipt["commands"].items():
            value.update(started=987654321, finished=987654322)
        library = self.lib / "sherpa-onnx-core.lib"
        library.write_bytes(b"!<arch>\nnew compiler output with different hash")
        self.manifest["libraries"][library.name] = build.record(library)
        self.save_receipt()
        self.assertNotEqual(build.digest(self.lib / "neo-asr-receipt.json"), native.RECEIPT_SHA)
        self.verify()
        self.assertEqual(list(self.work.iterdir()), [])

    def test_manifest_and_receipt_tamper(self):
        for path in (self.lib / "neo-asr-receipt.json", self.lib / "sherpa-onnx-core.lib",
                     self.attempt / "symbols/onnxruntime.symbols.txt", self.attempt / "graph.json"):
            with self.subTest(path=path):
                original = path.read_bytes()
                path.write_bytes(original + b"tamper")
                with self.assertRaisesRegex(ValueError, "hash/size"):
                    self.verify()
                path.write_bytes(original)
        self.runner.assert_not_called()

    def test_current_builder_and_recipe_pins_required(self):
        self.receipt["builder"]["sha256"] = "0" * 64
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "builder"):
            self.verify()
        for change in ({"recipe_sha256": "0" * 64}, {"schema": 2}, {"extra": True}):
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "review"):
                binding.check_binding({**self.pin, **change})
        with patch.object(build, "OPTIONS", {**build.OPTIONS, "SHERPA_ONNX_ENABLE_TTS": "ON"}), \
                self.assertRaisesRegex(ValueError, "review"):
            binding.check_binding(self.pin)

    def test_source_record_and_archive_tamper(self):
        self.receipt["source_lock"] = {**self.source_lock, "size": 1}
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "source record"):
            self.verify()
        self.receipt["source_lock"] = self.source_lock
        self.save_receipt()
        for filename in (build.SOURCE_FILE, build.DEPS["onnxruntime"][2]):
            path = self.cache / filename
            original = path.read_bytes()
            path.write_bytes(b"tamper")
            with self.subTest(filename=filename), self.assertRaisesRegex(ValueError, "hash/size|archive changed"):
                self.verify()
            path.write_bytes(original)
        self.write_bundle({"sources": {}})
        with self.assertRaisesRegex(ValueError, "source record"):
            self.verify()

    def test_complete_eigen_tree_changed_missing_or_extra(self):
        header = self.eigen / "header.h"
        for mode in ("changed", "missing", "extra"):
            with self.subTest(mode=mode):
                if mode == "changed":
                    header.write_bytes(b"changed")
                elif mode == "missing":
                    header.unlink()
                else:
                    (self.eigen / "extra.h").write_bytes(b"extra")
                with self.assertRaisesRegex(ValueError, "complete pinned archive"):
                    self.verify()
                header.write_bytes(b"synthetic original source")
        self.runner.assert_not_called()

    def test_tts_options_manifest_receipt_and_actual_cache(self):
        for target in (self.manifest, self.receipt):
            target["options"]["SHERPA_ONNX_ENABLE_TTS"] = "ON"
            self.save_receipt()
            with self.assertRaisesRegex(ValueError, "options"):
                self.verify()
            target["options"]["SHERPA_ONNX_ENABLE_TTS"] = "OFF"
        self.save_receipt()
        (self.builddir / "CMakeCache.txt").write_text(self.cache_text.replace(
            "SHERPA_ONNX_ENABLE_TTS:STRING=OFF", "SHERPA_ONNX_ENABLE_TTS:STRING=ON"))
        with self.assertRaisesRegex(ValueError, "no-TTS"):
            self.verify()

    def test_graph_semantics_and_current_codemodel_not_just_receipt_hash(self):
        detail = binding.load_json(self.replies / "core.json")
        for name in ("offline-tts.cc", "harmless-new-source.cc"):
            changed = copy.deepcopy(detail)
            changed["sources"].append({"path": name})
            build.write_json(self.replies / "core.json", changed)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "Forbidden TTS|differs from receipt graph"):
                self.verify()
        build.write_json(self.replies / "core.json", {"name": "sherpa-onnx-core", "sources": []})
        with self.assertRaisesRegex(ValueError, "Missing required"):
            self.verify()

    def test_rehashed_forged_graph_still_requires_actual_cmake_graph(self):
        build.write_json(self.attempt / "graph.json", [{"name": "claimed-safe", "sources": []}])
        self.receipt["graph"] = build.record(self.attempt / "graph.json")
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "differs from receipt graph"):
            self.verify()
        self.runner.assert_not_called()

    def test_source_tree_substitution_cannot_hide_behind_clean_eigen_copy(self):
        cache = self.builddir / "CMakeCache.txt"
        cache.write_text(self.cache_text.replace(str(self.eigen), str(self.root / "other-eigen")))
        with self.assertRaisesRegex(ValueError, "source/path mismatch"):
            self.verify()
        cache.write_text(self.cache_text)
        model = binding.load_json(self.replies / "model.json")
        model["paths"]["source"] = str(self.root / "other-sherpa")
        build.write_json(self.replies / "model.json", model)
        with self.assertRaisesRegex(ValueError, "source/path mismatch"):
            self.verify()
        self.runner.assert_not_called()

    def test_effective_includes_reject_unreviewed_missing_and_shadow_eigen(self):
        original = binding.load_json(self.replies / "core.json")
        for includes in ([{"path": "C:/unreviewed-eigen"}], [],
                         [{"path": "C:/unreviewed-eigen"}, {"path": str(self.eigen)}],
                         [{"path": str(self.eigen)}, {"path": "C:/unreviewed-eigen"}]):
            detail = copy.deepcopy(original)
            detail["compileGroups"][0]["includes"] = includes
            build.write_json(self.replies / "core.json", detail)
            with self.subTest(includes=includes), self.assertRaisesRegex(ValueError, "effective input|Eigen effective include"):
                self.verify()
        shadow = self.inputs["json"] / "Eigen"
        shadow.mkdir()
        (shadow / "Core").write_bytes(b"unreviewed Eigen")
        for order in ([self.inputs["json"], self.eigen], [self.eigen, self.inputs["json"]]):
            detail = copy.deepcopy(original)
            detail["compileGroups"][0]["includes"] = [{"path": str(p)} for p in order]
            build.write_json(self.replies / "core.json", detail)
            with self.subTest(order=order), self.assertRaisesRegex(ValueError, "Eigen include shadow"):
                self.verify()
        self.runner.assert_not_called()

    def test_nonexistent_upstream_include_under_reviewed_root_is_not_a_provider(self):
        detail = binding.load_json(self.replies / "core.json")
        detail["compileGroups"][0]["includes"].insert(0, {
            "path": str(self.inputs["kaldi_native_fbank"] / "kissfft")})
        build.write_json(self.replies / "core.json", detail)
        self.verify()

    def test_raw_include_flags_and_missing_compile_groups_rejected(self):
        original = binding.load_json(self.replies / "core.json")
        for flag in ('/IC:/unreviewed-eigen', '-I "C:/unreviewed-eigen"',
                     '/external:IC:/unreviewed-eigen', '/FIevil.h', '@overrides.rsp'):
            detail = copy.deepcopy(original)
            detail["compileGroups"][0]["compileCommandFragments"] = [{"fragment": flag}]
            build.write_json(self.replies / "core.json", detail)
            with self.subTest(flag=flag), self.assertRaisesRegex(ValueError, "compiler flag"):
                self.verify()
        original.pop("compileGroups")
        build.write_json(self.replies / "core.json", original)
        with self.assertRaisesRegex(ValueError, "compile groups"):
            self.verify()

    def test_compile_group_cannot_hide_source_or_quoted_include_shadow(self):
        original = binding.load_json(self.replies / "core.json")
        changed = copy.deepcopy(original)
        changed["sources"][0].pop("compileGroupIndex")
        build.write_json(self.replies / "core.json", changed)
        with self.assertRaisesRegex(ValueError, "missing effective compile group"):
            self.verify()
        changed = copy.deepcopy(original)
        changed["compileGroups"][0]["sourceIndexes"] = [1, 2]
        build.write_json(self.replies / "core.json", changed)
        with self.assertRaisesRegex(ValueError, "source index mismatch"):
            self.verify()
        # Quoted includes search beside the source before the explicit /I list.
        tree = self.inputs["kaldi_decoder"]
        (tree / "Eigen").mkdir()
        (tree / "Eigen/Core").write_bytes(b"shadow")
        changed = copy.deepcopy(original)
        changed["sources"].append({"path": str(tree / "header.h"), "compileGroupIndex": 0})
        changed["compileGroups"][0]["sourceIndexes"].append(3)
        build.write_json(self.replies / "core.json", changed)
        build.write_json(self.attempt / "graph.json", build.check_graph(self.builddir))
        self.receipt["graph"] = build.record(self.attempt / "graph.json")
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "Eigen include shadow"):
            self.verify()

    def test_complete_sherpa_source_cmake_changes_and_shadow_files(self):
        path = self.source / "offline-sense-voice-model.cc"
        original = path.read_bytes()
        for mode in ("changed", "missing", "extra-shadow", "cmake"):
            with self.subTest(mode=mode):
                if mode == "changed":
                    path.write_bytes(b"changed compile input")
                elif mode == "missing":
                    path.unlink()
                elif mode == "extra-shadow":
                    (self.source / "Eigen").mkdir()
                    (self.source / "Eigen/Core").write_bytes(b"shadow")
                else:
                    with (self.source / "CMakeLists.txt").open("ab") as stream:
                        stream.write(b"\ninclude_directories(BEFORE C:/unreviewed-eigen)\n")
                with self.assertRaisesRegex(ValueError, "Sherpa source differs"):
                    self.verify()
                path.write_bytes(original)
                if mode == "extra-shadow":
                    (self.source / "Eigen/Core").unlink()
                    (self.source / "Eigen").rmdir()
        self.runner.assert_not_called()

    def test_dependency_cmake_changed_missing_extra_and_compiled_source(self):
        tree = self.inputs["kaldi_decoder"]
        cmake = tree / "CMakeLists.txt"
        for mode in ("changed", "missing", "extra"):
            with self.subTest(mode=mode):
                if mode == "changed":
                    cmake.write_bytes(b"include(unreviewed)")
                elif mode == "missing":
                    cmake.unlink()
                else:
                    (tree / "override.cmake").write_bytes(b"# extra")
                with self.assertRaisesRegex(ValueError, "Dependency CMake/source input"):
                    self.verify()
                cmake.write_bytes(b"# pinned CMake")
        (tree / "override.cmake").unlink()
        detail = binding.load_json(self.replies / "core.json")
        detail["sources"].append({"path": str(tree / "header.h"), "compileGroupIndex": 0})
        detail["compileGroups"][0]["sourceIndexes"].append(3)
        build.write_json(self.replies / "core.json", detail)
        build.write_json(self.attempt / "graph.json", build.check_graph(self.builddir))
        self.receipt["graph"] = build.record(self.attempt / "graph.json")
        self.save_receipt()
        (tree / "header.h").write_bytes(b"changed compiled input")
        with self.assertRaisesRegex(ValueError, "Dependency CMake/source input changed"):
            self.verify()

    def test_sherpa_known_omitted_links_match_archive_and_receipt(self):
        archive = self.cache / build.SOURCE_FILE
        link = self.source.name + "/scripts/fixture-link"
        with tarfile.open(archive, "w:gz") as out:
            for name, data in self.source_files.items():
                member = tarfile.TarInfo(self.source.name + "/" + name)
                member.size = len(data)
                out.addfile(member, io.BytesIO(data))
            member = tarfile.TarInfo(link)
            member.type = tarfile.SYMTYPE
            member.linkname = "/original/developer/path"
            out.addfile(member)
        omitted = [{"path": link, "target": member.linkname}]
        build.write_json(self.attempt / "omitted-source-links.json", omitted)
        deadline = binding.time.monotonic() + 30
        binding.check_sherpa_source(self.source, self.attempt, self.work, deadline)
        build.write_json(self.attempt / "omitted-source-links.json", [])
        with self.assertRaisesRegex(ValueError, "omitted source links"):
            binding.check_sherpa_source(self.source, self.attempt, self.work, deadline)
        # Native/CMake symlinks are not part of the builder's allowed omissions.
        with tarfile.open(archive, "w:gz") as out:
            member.name = self.source.name + "/cmake/evil.cmake"
            out.addfile(member)
        with self.assertRaisesRegex(ValueError, "links/devices"):
            binding.check_sherpa_source(self.source, self.attempt, self.work, deadline)

    def test_self_consistent_configure_args_cannot_override_builder_recipe(self):
        command = self.receipt["commands"]["configure.command.json"]
        original = command["args"][:]
        cases = []
        for index, value in ((2, str(self.root / "other-source")), (4, str(self.root / "other-build")),
                             (6, "Ninja"), (8, "Win32")):
            args = original[:]
            args[index] = value
            cases.append(args)
        for key, value in (("CMAKE_INSTALL_PREFIX", str(self.root / "other-install")),
                           ("FETCHCONTENT_SOURCE_DIR_EIGEN", str(self.root / "other-eigen")),
                           ("SHERPA_ONNX_ENABLE_TTS", "ON"), ("BUILD_SHARED_LIBS", "ON")):
            cases.append([f"-D{key}={value}" if a.startswith(f"-D{key}=") else a for a in original])
        cases += [original + ["-DSHERPA_ONNX_ENABLE_TTS=OFF"],
                  original + ["-DCMAKE_PROJECT_INCLUDE=evil.cmake"],
                  original + ["--preset", "evil"], original[:-1], ["cmake", "--version"]]
        for args in cases:
            command["args"] = args
            self.save_receipt()
            with self.subTest(args=args), self.assertRaisesRegex(ValueError, "command|configure|source/path"):
                self.verify()
        self.runner.assert_not_called()

    def test_self_consistent_build_install_args_require_directory_release_and_structure(self):
        for stage in ("build", "install"):
            command = self.receipt["commands"][stage + ".command.json"]
            original = command["args"][:]
            cases = [original + ["--prefix", str(self.root / "other")], original[:-1]]
            for index, value in ((0, "different-cmake"), (1, "--version"),
                                 (2, str(self.root / "other-build")), (4, "Debug")):
                args = original[:]
                args[index] = value
                cases.append(args)
            for args in cases:
                command["args"] = args
                self.save_receipt()
                with self.subTest(stage=stage, args=args), self.assertRaisesRegex(ValueError, "command args|source/path"):
                    self.verify()
            command["args"] = original
        self.runner.assert_not_called()

    def test_cmake_location_vs_version_and_instance_are_not_machine_pins(self):
        for command in self.receipt["commands"].values():
            command["args"][0] = "C:/different-host/tools/cmake.exe"
        args = self.receipt["commands"]["configure.command.json"]["args"]
        args[6] = binding.ci.GENERATORS[18]
        args.insert(10, "-DCMAKE_GENERATOR_INSTANCE=C:/different-vs,version=18.8.12023.21")
        self.save_receipt()
        self.verify()

    def test_failed_or_timed_out_command_even_with_fresh_receipt_hash(self):
        for field, value in (("returncode", 1), ("timed_out", True)):
            for stage in ("configure", "build", "install"):
                command = self.receipt["commands"][stage + ".command.json"]
                original = dict(command)
                command[field] = value
                self.save_receipt()
                with self.subTest(stage=stage, field=field), self.assertRaisesRegex(ValueError, "successful native stage"):
                    self.verify()
                command.clear()
                command.update(original)
        self.runner.assert_not_called()

    def test_command_file_must_match_receipt(self):
        build.write_json(self.attempt / "build.command.json", {"returncode": 1})
        with self.assertRaisesRegex(ValueError, "command receipt changed"):
            self.verify()

    def test_library_missing_extra_and_wrong_archive(self):
        library = self.lib / "sherpa-onnx-core.lib"
        original = library.read_bytes()
        library.unlink()
        with self.assertRaisesRegex(ValueError, "library set"):
            self.verify()
        library.write_bytes(original)
        extra = self.lib / "old-tts.lib"
        extra.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "library set"):
            self.verify()
        extra.unlink()
        library.write_bytes(b"not a native library")
        self.manifest["libraries"][library.name] = build.record(library)
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "Not a native archive"):
            self.verify()

    def test_actual_dumpbin_tts_missing_api_failure_and_timeout(self):
        self.dump_text += b"002 000 SECT1 notype External | espeak_Initialize\n"
        with self.assertRaisesRegex(ValueError, "TTS/eSpeak"):
            self.verify()
        self.dump_text = b"no defined API\n"
        with self.assertRaisesRegex(ValueError, "Required defined C API"):
            self.verify()
        self.runner.side_effect = lambda args, **kwargs: subprocess.CompletedProcess(args, 7)
        with self.assertRaisesRegex(ValueError, "Command failed"):
            self.verify()
        self.runner.side_effect = subprocess.TimeoutExpired("dumpbin", 1)
        with self.assertRaisesRegex(ValueError, "timed out"):
            self.verify()
        self.assertEqual(list(self.work.iterdir()), [])

    def test_dependency_pin_and_source_options_are_checked(self):
        self.receipt["dependency_archives"]["eigen"]["sha256"] = "0" * 64
        self.save_receipt()
        with self.assertRaisesRegex(ValueError, "dependency pins"):
            self.verify()
        self.receipt["dependency_archives"]["eigen"]["sha256"] = build.DEPS["eigen"][1]
        self.save_receipt()
        (self.source / "CMakeLists.txt").write_text("changed source")
        with self.assertRaisesRegex(ValueError, "source version"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
