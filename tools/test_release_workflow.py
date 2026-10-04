"""Offline release-workflow regressions using disposable payloads and Git repositories."""
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

if __package__:
    from .check_release import validate_payload
else:
    from check_release import validate_payload


WORKFLOW = Path(__file__).resolve().parents[1] / ".github" / "workflows" / "release.yml"
PWSH = shutil.which("pwsh")
GIT = shutil.which("git")


def pwsh_step(name):
    """Extract a named literal run block, not a general-purpose YAML parser."""
    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    matches = [
        (index, match)
        for index, line in enumerate(lines)
        if (match := re.fullmatch(r"( *)- name: " + re.escape(name), line))
    ]
    if len(matches) != 1:
        raise AssertionError(f"Expected exactly one workflow step named {name!r}")
    start, match = matches[0]
    step_indent = len(match[1])
    end = start + 1
    while end < len(lines):
        line = lines[end]
        if line.strip() and len(line) - len(line.lstrip()) <= step_indent:
            break
        end += 1
    step = lines[start + 1:end]
    field_prefix = " " * (step_indent + 2)
    if field_prefix + "shell: pwsh" not in step:
        raise AssertionError(f"{name!r} must use pwsh")
    run_header = field_prefix + "run: |"
    if step.count(run_header) != 1:
        raise AssertionError(f"{name!r} must have one literal run block")
    body = []
    for line in step[step.index(run_header) + 1:]:
        if line.strip() and len(line) - len(line.lstrip()) <= len(field_prefix):
            break
        body.append(line)
    script = textwrap.dedent("\n".join(body)).strip() + "\n"
    if not script.strip():
        raise AssertionError(f"{name!r} has an empty run block")
    return script


class ReleaseWorkflowStaticTests(unittest.TestCase):
    def test_filtered_runtime_preparation_precedes_copy_and_never_falls_back(self):
        text = WORKFLOW.read_text(encoding='utf-8')
        self.assertLess(text.index('name: Fetch MinGit runtime'), text.index('name: Prepare GCM-free MinGit distribution'))
        self.assertLess(text.index('name: Prepare GCM-free MinGit distribution'), text.index('name: Assemble package'))
        prepare = pwsh_step('Prepare GCM-free MinGit distribution')
        self.assertIn('--source .cache/runtime/gitbash --output target/package/runtime-distribution --trusted-output-root target/package', prepare)
        self.assertNotIn('--source-companion', prepare)
        assembly = pwsh_step('Assemble package')
        self.assertNotIn('Copy-Item -Recurse .cache\\runtime\\gitbash', assembly)
        self.assertIn('target\\package\\runtime-distribution\\runtime\\gitbash', assembly)
        for name in ('MANIFEST.json', 'MODIFICATIONS.md', 'gitbash-distribution-policy.json'):
            self.assertIn(name, assembly)

    def test_distribution_gate_is_first_release_action_after_checkout(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        check, release = workflow.split("\n  release:\n", 1)
        steps = re.split(r"(?m)^      - name: ", release)[1:]
        self.assertTrue(steps[0].startswith("Checkout\n"))
        gate = steps[1]
        self.assertTrue(gate.startswith("Verify distribution approval (fail closed)\n"))
        self.assertIn("        run: python tools/check_distribution_review.py --policy tools/distribution-review.json\n", gate)
        self.assertNotRegex(gate, r"(?m)^\s+(?:if|continue-on-error):")
        self.assertNotIn("continue-on-error:", release)
        self.assertNotIn("always()", release)
        self.assertNotIn("check_distribution_review.py --policy", check)
        for name in ("Build", "Download STT models", "Fetch MinGit runtime", "Assemble package",
                     "Zip portable package", "Build installer (NSIS)", "Create release"):
            self.assertGreater(release.index("- name: " + name + "\n"), release.index("- name: Verify distribution approval"))
        for module in ("tools.test_distribution_review", "tools.test_audit_native_link", "tools.test_check_release"):
            self.assertIn(module, check)
        self.assertIn("PYTHONPATH: tools", check)

    def test_workflow_does_not_depend_on_private_descriptions(self):
        workflow = WORKFLOW.read_text(encoding="utf-8").replace("\\", "/")
        for name in ("docs/distribution/README.md", "docs/README.md", "docs/licenses/README.md",
                     "docs/native-build.md"):
            self.assertNotIn(name, workflow)
        self.assertIn('Copy-Item -Recurse docs/licenses "$pkg/docs/licenses"', workflow)

    def test_native_preparation_is_required_and_download_cache_only(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        check = workflow.split("\n  check:\n", 1)[1].split("\n  drawing-check:\n", 1)[0]
        release = workflow.split("\n  release:\n", 1)[1]
        for job in (check, release):
            steps = re.split(r"(?m)^      - name: ", job)[1:]
            prepare = next(step for step in steps if step.startswith("Prepare mandatory no-TTS Sherpa native\n"))
            self.assertIn("--budget-seconds 600 --validation-seconds 180 --max-download-mib 256", prepare)
            self.assertNotRegex(prepare, r"(?m)^\s+(?:if|continue-on-error):")
            first_cargo = re.search(r"\bcargo\s+(?:check|test|build)\b", job)
            self.assertIsNotNone(first_cargo)
            self.assertLess(job.index("tools/prepare_sherpa_ci.py"), first_cargo.start())
            cache = next(step for step in steps if step.startswith("Cache Sherpa download archives and CMake wheel\n"))
            self.assertNotIn("target/", cache)
            self.assertNotIn("source-lock.json", cache)
            self.assertNotIn("native/install", cache)
            self.assertNotIn("restore-keys", cache)
            for forbidden in ("SHERPA_ONNX_LIB_DIR", "NEO_SHERPA_ASR_ONLY", "GITHUB_PATH", "upload-artifact", "download-artifact"):
                self.assertNotIn(forbidden, job)
        self.assertIn("python -B -m unittest tools.test_prepare_sherpa_ci -v", check)
        self.assertLess(check.index("tools.test_prepare_sherpa_ci"), check.index("tools/prepare_sherpa_ci.py --"))
        self.assertLess(release.index("check_distribution_review.py --policy"), release.index("Cache Sherpa download"))

    def test_drawing_root_package_tests_do_not_use_pythonpath_workaround(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        step = workflow.split("      - name: Drawing release root-package regressions\n", 1)[1].split("\n  #", 1)[0]
        self.assertIn("python -B -m unittest tools.test_assemble_drawing_release tools.test_release_workflow -v", step)
        self.assertNotIn("PYTHONPATH", step)
        self.assertNotIn("env:", step)

    def test_drawing_check_uses_lock_pin_without_release_approval_or_upload(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        drawing = workflow.split("\n  drawing-check:\n", 1)[1].split("\n  release:\n", 1)[0]
        self.assertIn("runs-on: windows-2022", drawing)
        self.assertLess(drawing.index("Checkout main for drawing lock"), drawing.index("lock-outputs"))
        self.assertLess(drawing.index("lock-outputs"), drawing.index("Checkout pinned drawing for CI"))
        self.assertIn("ref: ${{ steps.drawing-pin.outputs.commit }}", drawing)
        self.assertIn("repository: ${{ steps.drawing-pin.outputs.repository }}", drawing)
        self.assertIn("path: target/package/drawing-src", drawing)
        self.assertIn("cargo +${{ steps.drawing-pin.outputs.rust }} test --locked -p board-protocol --lib --target x86_64-pc-windows-msvc", drawing)
        self.assertIn('target/package/drawing-tests"', drawing)
        self.assertIn("rustup toolchain install ${{ steps.drawing-pin.outputs.rust }}", drawing)
        self.assertIn("assemble_drawing_release.py build --source target/package/drawing-src --target-dir target/package/drawing-build", drawing)
        for forbidden in ("upload-artifact", "check-approval", "verify-review", "--ignored", "--include-ignored", "--workspace", "--gui", "continue-on-error", "models/"):
            self.assertNotIn(forbidden, drawing)
        self.assertNotRegex(drawing, r"(?m)^\s+if:")

    def test_release_drawing_gates_precede_build_and_assembly_precedes_archive(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        release = workflow.split("\n  release:\n", 1)[1]
        self.assertIn("needs: [check, drawing-check]", release)
        self.assertIn("runs-on: windows-2022", release)
        steps = re.split(r"(?m)^      - name: ", release)[1:]
        self.assertTrue(steps[2].startswith("Verify drawing lock approval (fail closed)\n"))
        order = ["check_distribution_review.py --policy", "assemble_drawing_release.py check-approval",
                 "assemble_drawing_release.py lock-outputs", "Checkout pinned drawing for release",
                 "assemble_drawing_release.py verify-review", "Determine version", "Build\n",
                 "assemble_drawing_release.py build", "Assemble package\n",
                 "Audit packaged PE imports (external CRT prerequisite)", "assemble_drawing_release.py assemble", "Zip portable package"]
        self.assertEqual([release.index(value) for value in order], sorted(release.index(value) for value in order))
        self.assertIn("ref: ${{ steps.drawing-pin.outputs.commit }}", release)
        self.assertIn("path: target/package/drawing-src", release)
        for name in ("Verify drawing lock approval", "Verify drawing reviewed source", "Assemble approved drawing release"):
            step = next(step for step in steps if step.startswith(name))
            self.assertNotRegex(step, r"(?m)^\s+(?:if|continue-on-error):")
        for forbidden in ("package_combined.py", "download-artifact", "--mode evaluation", "public_approved =", "--force", "--skip"):
            self.assertNotIn(forbidden, release)
        self.assertIn("tools.test_assemble_drawing_release", workflow)

    def test_release_audits_external_crt_without_collecting_or_installing_it(self):
        script = pwsh_step("Audit packaged PE imports (external CRT prerequisite)")
        self.assertIn('python tools/check_release.py --package dist/neo --dumpbin "$($dumpbin.FullName)" --crt-policy external', script)
        self.assertIn("if (-not $dumpbin)", script)
        self.assertIn("if ($LASTEXITCODE -ne 0)", script)
        for forbidden in ("$crt", "--redist-dir", "\\Redist\\", "Copy-Item", "Invoke-WebRequest", "vc_redist"):
            self.assertNotIn(forbidden, script)
        notes = pwsh_step("Generate changelog")
        self.assertIn("MSVC CRT DLLs are not bundled", notes)
        self.assertIn("Microsoft Visual C++ v14 x64 Redistributable >= 14.51.36247.0", notes)
        self.assertIn("https://aka.ms/vc14/vc_redist.x64.exe", notes)
        self.assertIn("neither package downloads or installs it automatically", notes)
        self.assertIn("clean-machine compatibility remains unverified", notes)
        self.assertNotIn("imported app-local MSVC CRT DLLs", notes)
        self.assertNotIn("解压即用", notes)

    def test_stt_download_and_extraction_stay_in_cache(self):
        script = pwsh_step("Download STT models")
        self.assertIn(r"-OutFile .cache\stt\sv.tar.bz2", script)
        self.assertIn(r"tar -xjf .cache\stt\sv.tar.bz2 -C .cache\stt\extracted", script)
        self.assertIn("if ($LASTEXITCODE -ne 0)", script)
        self.assertIn(r"-OutFile .cache\stt\silero_vad.onnx", script)
        self.assertNotIn("assets-stt", script)

    def test_release_runtime_and_stt_pins_are_exact_and_checked_before_use(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn(
            "        run: python tools/fetch_runtime.py --mirror github --version 2.55.0.windows.5 "
            "--sha256 56d7b226b7693196cfc71fef26568f536c4a021ab6c37ff2db4287bed908e96e\n", workflow)
        script = pwsh_step("Download STT models")
        self.assertIn("$ErrorActionPreference = 'Stop'", script)
        for variable, path, digest, failure in (
            ("sv", r".cache\stt\sv.tar.bz2", "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e", "SenseVoice archive"),
            ("vad", r".cache\stt\silero_vad.onnx", "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6", "Silero"),
        ):
            self.assertIn(f"${variable}Expected = '{digest}'", script)
            hash_line = f"${variable}Hash = (Get-FileHash -Algorithm SHA256 -LiteralPath {path}).Hash.ToLowerInvariant()"
            check = f"if (${variable}Hash -cne ${variable}Expected) {{ throw '{failure} SHA-256 mismatch' }}"
            self.assertIn(hash_line, script)
            self.assertIn(check, script)
            self.assertLess(script.index("-OutFile " + path), script.index(hash_line))
            self.assertLess(script.index(hash_line), script.index(check))
            self.assertLess(script.index(check), script.index("tar -xjf"))
            self.assertLess(script.index(check), script.index("Copy-Item"))
        self.assertIn(r"Set-Content -Encoding utf8 .cache\stt\member-sha256.json", script)
        self.assertIn("Get-ChildItem -LiteralPath $extracted -Recurse -File", script)
        self.assertIn("Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName", script)
        self.assertNotIn("docs-pri", script)

    def test_installer_art_default_workflow_and_nsis_agree(self):
        root = WORKFLOW.parents[2]
        tree = ast.parse((root / "tools/make_installer_art.py").read_text(encoding="utf-8"))
        argument = next(node for node in ast.walk(tree) if isinstance(node, ast.Call)
                        and isinstance(node.func, ast.Attribute) and node.func.attr == "add_argument"
                        and node.args and isinstance(node.args[0], ast.Constant) and node.args[0].value == "--out")
        default = next(keyword.value for keyword in argument.keywords if keyword.arg == "default")
        self.assertEqual([node.value for node in default.args[1:]], ["target", "package", "installer-art"])
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("--out target/package/installer-art", workflow)
        self.assertIn(r"Test-Path target\package\installer-art\neo.ico", workflow)
        self.assertIn('!define ART "target\\package\\installer-art"',
                      (root / "tools/installer.nsi").read_text(encoding="utf-8"))
        self.assertNotIn("build/installer-art", workflow)

    def test_gh_release_create_verifies_existing_tag(self):
        # Inspect the actual argument array; a comment mentioning the flag is not enough.
        script = pwsh_step("Create release")
        code = "\n".join(line for line in script.splitlines() if not line.lstrip().startswith("#"))
        args = re.search(r"\$args\s*=\s*@\((.*?)\)", code, re.DOTALL)
        self.assertIsNotNone(args, "Missing gh argument array")
        self.assertRegex(args[1], r"^\s*'release'\s*,\s*'create'\s*,")
        self.assertRegex(args[1], r"(?:^|,)\s*'--verify-tag'\s*(?:,|$)")
        self.assertRegex(code, r"(?m)^\s*gh\s+@args\s*$")


@unittest.skipUnless(PWSH, "pwsh is not installed")
class SttDownloadTests(unittest.TestCase):
    def test_actual_script_checks_downloads_before_extract_and_copy(self):
        fixtures = {"sv": b"mock archive", "vad": b"mock vad"}
        pins = {"sv": "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e",
                "vad": "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"}
        mocks = r'''
            function Invoke-WebRequest {
              param($Uri, $OutFile)
              $content = if ($Uri.EndsWith('.tar.bz2')) { 'mock archive' } else { 'mock vad' }
              [IO.File]::WriteAllText((Join-Path $PWD $OutFile), $content)
            }
            function tar {
              $root = '.cache/stt/extracted/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17'
              New-Item -ItemType Directory $root | Out-Null
              [IO.File]::WriteAllText((Join-Path $PWD "$root/model.int8.onnx"), 'mock model')
              [IO.File]::WriteAllText((Join-Path $PWD "$root/tokens.txt"), 'mock tokens')
              [IO.File]::WriteAllText((Join-Path $PWD "$root/README"), 'mock notice')
              $global:LASTEXITCODE = 0
            }
        '''
        for bad in ("sv", "vad", None):
            with self.subTest(bad=bad), tempfile.TemporaryDirectory(prefix="neo-stt-pins-") as td:
                root = Path(td)
                script = pwsh_step("Download STT models")
                # Static tests above pin real upstream hashes; only fixture hashes change here.
                for name, pin in pins.items():
                    if name != bad:
                        script = script.replace(pin, hashlib.sha256(fixtures[name]).hexdigest())
                local = root / "assets-stt/local.onnx"
                local.parent.mkdir()
                local.write_bytes(b"local input unchanged")
                path = root / "download.ps1"
                path.write_text(textwrap.dedent(mocks) + script, encoding="utf-8")
                result = subprocess.run([PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(path)],
                                        cwd=root, capture_output=True, text=True, errors="replace", timeout=30)
                self.assertEqual(local.read_bytes(), b"local input unchanged")
                cache = root / ".cache/stt"
                if bad:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("SHA-256 mismatch", result.stdout + result.stderr)
                    self.assertFalse((cache / "extracted").exists())
                    self.assertFalse((cache / "models").exists())
                    self.assertFalse((cache / "member-sha256.json").exists())
                    continue
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                manifest = json.loads((cache / "member-sha256.json").read_text(encoding="utf-8-sig"))
                self.assertEqual(manifest["sense_voice_archive_sha256"], hashlib.sha256(fixtures["sv"]).hexdigest())
                self.assertEqual(manifest["silero_sha256"], hashlib.sha256(fixtures["vad"]).hexdigest())
                self.assertEqual(len(manifest["members"]), 4)
                for member in manifest["members"]:
                    base = cache / "extracted" if member["archive"] else cache
                    data = (base / member["path"]).read_bytes()
                    self.assertEqual(member["size"], len(data))
                    self.assertEqual(member["sha256"], hashlib.sha256(data).hexdigest())
                self.assertEqual({p.relative_to(cache / "models").as_posix(): p.read_bytes()
                                  for p in (cache / "models").rglob("*") if p.is_file()},
                                 {"sense-voice/model.int8.onnx": b"mock model", "sense-voice/tokens.txt": b"mock tokens",
                                  "vad/silero_vad.onnx": fixtures["vad"]})


@unittest.skipUnless(PWSH, "pwsh is not installed")
class AssemblePackageTests(unittest.TestCase):
    @patch('tools.check_release.verify_distribution')
    def test_actual_assembly_separates_models_dlls_cache_docs_and_languages(self, filtered):
        with tempfile.TemporaryDirectory(prefix="neo-package-layout-") as td:
            root = Path(td)
            sources = {
                "target/x86_64-pc-windows-msvc/release/neo.exe": "neo.exe",
                "crates/neo-wake/assets/melspectrogram.onnx": "resources/models/wake/melspectrogram.onnx",
                "crates/neo-wake/assets/embedding_model.onnx": "resources/models/wake/embedding_model.onnx",
                "crates/neo-wake/assets/hi_neo.onnx": "resources/models/wake/hi_neo.onnx",
                "crates/neo-wake/assets/onnxruntime.dll": "runtime/onnx/onnxruntime.dll",
                "crates/neo-wake/assets/onnxruntime_providers_shared.dll": "runtime/onnx/onnxruntime_providers_shared.dll",
                ".cache/stt/models/sense-voice/model.int8.onnx": "resources/models/stt/sense-voice/model.int8.onnx",
                ".cache/stt/models/sense-voice/tokens.txt": "resources/models/stt/sense-voice/tokens.txt",
                ".cache/stt/models/vad/silero_vad.onnx": "resources/models/stt/vad/silero_vad.onnx",
                "target/package/runtime-distribution/runtime/gitbash/usr/bin/bash.exe": "runtime/gitbash/usr/bin/bash.exe",
                **{f'target/package/runtime-distribution/{n}': f'docs/runtime-distribution/{n}' for n in
                   ('MANIFEST.json', 'MODIFICATIONS.md', 'gitbash-distribution-policy.json')},
                'docs/licenses/models/hi_neo-model.json': 'docs/licenses/models/hi_neo-model.json',
                'docs/licenses/models/hi_neo-MIT.txt': 'docs/licenses/models/hi_neo-MIT.txt',
                "resources/lang/zh-CN.lang": "resources/lang/zh-CN.lang",
                "resources/lang/en-US.lang": "resources/lang/en-US.lang",
                "LICENSE": "LICENSE", "NOTICE": "NOTICE",
                "docs/licenses/cargo-notices.txt": "docs/licenses/cargo-notices.txt",
                "docs/licenses/models/sherpa-sense-README.md": "docs/licenses/models/sherpa-sense-README.md",
                "docs/licenses/models/sensevoice-model-card.md": "docs/licenses/models/sensevoice-model-card.md",
                "docs/licenses/runtime/gitbash-SOURCE.md": "docs/licenses/runtime/gitbash-SOURCE.md",
                "docs/licenses/assets/font/LICENSE.txt": "docs/licenses/assets/font/LICENSE.txt",
                "docs/licenses/runtime/onnx/NOTICE.txt": "docs/licenses/runtime/onnx/NOTICE.txt",
            }
            excluded = [".cache/runtime/gitbash/RAW-GCM.dll", "README.md", "crates/neo-wake/assets/README.md", "crates/neo-wake/assets/experimental.onnx",
                        ".cache/stt/sv.tar.bz2", ".cache/stt/extracted/unused.txt",
                        ".cache/stt/member-sha256.json", ".cache/stt/silero_vad.onnx",
                        ".cache/runtime/.gitbash-install.lock", ".cache/runtime/other/file.txt",
                        "runtime/gitbash/old-cache.txt", "assets-stt/old-model.onnx",
                        "resources/lang/README.md", "resources/lang/draft.json", "resources/lang/fr-FR.lang",
                        "resources/lang/nested/zh-CN.lang", "docs-pri/legal-review.md",
                        "docs-pri/licenses/cargo-inventory.json", "docs-pri/licenses/cargo-inventory.md",
                        "docs-pri/licenses/assets-audit.md", "docs-pri/licenses/runtime-audit.md",
                        "docs-pri/licenses/assets/README.md", "docs-pri/licenses/runtime/README.md",
                        ".cache/licenses/private-evidence.txt"]
            for name in [*sources, *excluded]:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            for language, translation in (("zh-CN", "你好，{name}"), ("en-US", "Hello, {name}")):
                (root / f"resources/lang/{language}.lang").write_text(
                    json.dumps({"你好，{name}": translation}, ensure_ascii=False), encoding="utf-8")
            model = (root / 'crates/neo-wake/assets/hi_neo.onnx').read_bytes()
            (root / 'docs/licenses/models/hi_neo-model.json').write_text(json.dumps({
                'path': 'resources/models/wake/hi_neo.onnx', 'sha256': hashlib.sha256(model).hexdigest(),
                'size': len(model), 'license': 'MIT', 'license_file': 'hi_neo-MIT.txt'}), encoding='utf-8')
            self.assertFalse((root / "docs/distribution/README.md").exists())
            self.assertFalse((root / "docs/native-build.md").exists())
            self.assertFalse(list((root / "docs/licenses").rglob("README.md")))
            expected = {dest: (root / src).read_bytes() for src, dest in sources.items()}
            script = root / "assemble.ps1"
            script.write_text(pwsh_step("Assemble package"), encoding="utf-8")
            command = [PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(script)]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            package = root / "dist/neo"
            actual = {p.relative_to(package).as_posix(): p.read_bytes() for p in package.rglob("*") if p.is_file()}
            self.assertEqual(actual, expected)
            self.assertFalse((package / "docs-pri").exists())
            for name in ("cargo-inventory.json", "cargo-inventory.md", "assets-audit.md", "runtime-audit.md"):
                self.assertFalse((package / "docs/licenses" / name).exists())
            for name in excluded:
                self.assertNotIn((root / name).read_bytes(), actual.values(), name)
            validate_payload(package)
            # A retry must not merge a dirty package, remove user files or change prior payloads.
            (package / "custom.txt").write_bytes(b"keep")
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Package directory must be clean", result.stdout + result.stderr)
            self.assertEqual((package / "custom.txt").read_bytes(), b"keep")
            for name, data in actual.items():
                self.assertEqual((package / name).read_bytes(), data)
            # Explicit copies fail immediately; missing public legal files in a copied directory
            # must fail the payload check even when PowerShell itself succeeds.
            required = ["resources/lang/zh-CN.lang", "resources/lang/en-US.lang", "LICENSE", "NOTICE",
                        "docs/licenses/cargo-notices.txt"]
            for name in required:
                with self.subTest(missing_resource=name):
                    shutil.rmtree(package)
                    (root / name).unlink()
                    result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
                    if name.startswith("docs/licenses/"):
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    else:
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(Path(name).name, result.stdout + result.stderr)
                    with self.assertRaisesRegex(ValueError, "Required release resource missing/empty") as error:
                        validate_payload(package)
                    if name.startswith("docs/licenses/"):
                        self.assertIn(str(package / name), str(error.exception))
                    (root / name).write_bytes(expected[name])
            shutil.rmtree(package)
            shutil.rmtree(root / "docs/licenses")
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("licenses", result.stdout + result.stderr)
            with self.assertRaisesRegex(ValueError, "Required release resource missing/empty"):
                validate_payload(package)


@unittest.skipUnless(PWSH, "pwsh is not installed")
@unittest.skipUnless(GIT, "git is not installed")
class DetermineVersionTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="neo-release-workflow-")
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        empty = root / "empty"
        empty.mkdir()
        config = root / "gitconfig"
        config.touch()
        self.output = root / "github-output"
        self.output.touch()
        self.script = root / "determine-version.ps1"
        self.script.write_text(pwsh_step("Determine version"), encoding="utf-8")

        # Do not inherit repository redirects, user hooks/signing, or CI credentials.
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.upper().startswith(("GIT_", "GITHUB_", "GH_"))
        }
        self.env.update({
            "GIT_EDITOR": "true",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(config),
            "GIT_ATTR_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ALLOW_PROTOCOL": "",
            "GITHUB_OUTPUT": str(self.output),
            "NO_COLOR": "1",
        })
        self.git("init", "--initial-branch=main", f"--template={empty}")
        for key, value in (
            ("user.name", "Release workflow test"),
            ("user.email", "release-test@example.invalid"),
            ("core.hooksPath", str(empty)),
            ("commit.gpgSign", "false"),
            ("tag.gpgSign", "false"),
        ):
            self.git("config", "--local", key, value)
        self.version = "1.2.3"
        (self.repo / "Cargo.toml").write_text(
            f'[workspace.package]\nversion = "{self.version}"\n', encoding="utf-8"
        )
        self.git("add", "Cargo.toml")
        self.git("commit", "-m", "Initial release fixture")

    def git(self, *args):
        result = subprocess.run(
            [GIT, "--no-pager", *args], cwd=self.repo, env=self.env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stdout)
        return result.stdout.strip()

    def determine(self, *, manual=False, ref_name=None):
        env = self.env.copy()
        env.update({
            "GITHUB_EVENT_NAME": "workflow_dispatch" if manual else "push",
            "GITHUB_REF_TYPE": "branch" if manual else "tag",
            "GITHUB_REF_NAME": ref_name or ("main" if manual else f"v{self.version}"),
            "GITHUB_SHA": self.git("rev-parse", "--verify", "HEAD"),
        })
        env["GITHUB_REF"] = (
            "refs/heads/" if manual else "refs/tags/"
        ) + env["GITHUB_REF_NAME"]
        return subprocess.run(
            [PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(self.script)],
            cwd=self.repo, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", timeout=30,
        )

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(
            self.output.read_text(encoding="utf-8-sig").splitlines(),
            [f"version={self.version}", f"tag=v{self.version}", "prerelease=false"],
        )

    def assert_rejected(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stdout)
        self.assertEqual(self.output.read_text(encoding="utf-8-sig"), "", result.stdout)

    def test_matching_tag_succeeds(self):
        self.git("tag", f"v{self.version}")
        self.assert_success(self.determine())

    def test_annotated_tag_succeeds(self):
        self.git("tag", "-a", f"v{self.version}", "-m", "Annotated release fixture")
        self.assertEqual(self.git("cat-file", "-t", f"refs/tags/v{self.version}"), "tag")
        self.assert_success(self.determine())

    def test_tag_version_mismatch_is_rejected(self):
        # Both tags exist at HEAD, so only the ref-name/version guard can reject this.
        self.git("tag", f"v{self.version}")
        self.git("tag", "v9.9.9")
        self.assert_rejected(self.determine(ref_name="v9.9.9"), "does not match Cargo version")

    def test_manual_without_tag_is_rejected(self):
        self.assert_rejected(self.determine(manual=True), f"Create tag v{self.version}")

    def test_manual_tag_at_wrong_commit_is_rejected(self):
        self.git("tag", f"v{self.version}")
        self.git("commit", "--allow-empty", "-m", "Different commit with the same Cargo version")
        self.assert_rejected(self.determine(manual=True), "must point at the checked-out workflow commit")

    def test_manual_with_correct_tag_succeeds(self):
        self.git("tag", f"v{self.version}")
        self.assert_success(self.determine(manual=True))


if __name__ == "__main__":
    unittest.main()
