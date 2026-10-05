"""Offline release-workflow regressions using disposable payloads and Git repositories."""
import ast
import hashlib
import json
import os
from pathlib import Path

import shutil
import subprocess
import tempfile
import textwrap
import unittest
from unittest.mock import patch

if __package__:
    from .check_release import validate_payload
    from .test_workflow_syntax import parse_workflow, yaml
else:
    from check_release import validate_payload
    from test_workflow_syntax import parse_workflow, yaml


WORKFLOW = Path(__file__).resolve().parents[1] / ".github" / "workflows" / "build.yml"
PWSH = shutil.which("pwsh")
GIT = shutil.which("git")


RELEASE_IF = "inputs.publish"


def workflow_steps():
    if yaml is None:
        raise unittest.SkipTest("Development test needs PyYAML==6.0.3")
    return parse_workflow(WORKFLOW.read_text(encoding="utf-8"))["jobs"]["check"]["steps"]


def workflow_step(name):
    matches = [step for step in workflow_steps() if step.get("name") == name]
    if len(matches) != 1:
        raise AssertionError(f"Expected exactly one workflow step named {name!r}")
    return matches[0]


def pwsh_step(name):
    """Execute fixtures from the actual parsed workflow, not indentation heuristics."""
    step = workflow_step(name)
    if step.get("shell") != "pwsh" or not step.get("run", "").strip():
        raise AssertionError(f"{name!r} must have a nonempty pwsh run block")
    return step["run"]


class ReleaseWorkflowStaticTests(unittest.TestCase):
    def test_filtered_runtime_preparation_precedes_copy_and_never_falls_back(self):
        names = [step.get('name') for step in workflow_steps()]
        self.assertLess(names.index('Fetch MinGit runtime'), names.index('Prepare GCM-free MinGit distribution'))
        self.assertLess(names.index('Prepare GCM-free MinGit distribution'), names.index('Assemble package'))
        prepare = pwsh_step('Prepare GCM-free MinGit distribution')
        self.assertIn('--source .cache/runtime/gitbash --output target/package/runtime-distribution --trusted-output-root target/package', prepare)
        self.assertNotIn('--source-companion', prepare)
        assembly = pwsh_step('Assemble package')
        self.assertNotIn('Copy-Item -Recurse .cache\\runtime\\gitbash', assembly)
        self.assertIn('target\\package\\runtime-distribution\\runtime\\gitbash', assembly)
        for name in ('MANIFEST.json', 'MODIFICATIONS.md', 'gitbash-distribution-policy.json'):
            self.assertIn(name, assembly)

    def test_release_uses_technical_checks_without_approval_policy_calls(self):
        steps = workflow_steps()
        names = [step.get('name') for step in steps]
        workflow = json.dumps(steps)
        check = '\n'.join(step.get('run', '') for step in steps if 'if' not in step)
        release = '\n'.join(step.get('run', '') for step in steps if RELEASE_IF in step.get('if', ''))
        self.assertEqual(names[0], 'Checkout')
        source = workflow_step('Verify pinned drawing source and legal bodies')
        self.assertEqual(source['run'], 'python tools/assemble_drawing_release.py verify-source --source target/package/drawing-src')
        self.assertNotIn('if', source)
        self.assertNotIn('continue-on-error', source)
        for forbidden in ("check_distribution_review.py", "distribution-review.json", "check-approval", "verify-review",
                          "Verify distribution approval", "Verify drawing lock approval"):
            self.assertNotIn(forbidden, workflow)
        self.assertNotRegex(release, r"--(?:policy|skip)[\w-]*\b")
        for step in steps:
            self.assertNotIn('continue-on-error', step)
            self.assertNotIn('always()', step.get('if', ''))

        for name in ("Build", "Download STT models", "Fetch MinGit runtime", "Assemble package",
                     "Zip portable package", "Build installer (NSIS)", "Create release"):
            self.assertGreater(names.index(name), names.index('Verify pinned drawing source and legal bodies'))
        for module in ("tools.test_distribution_review", "tools.test_audit_native_link", "tools.test_check_release"):
            self.assertIn(module, check)
        self.assertEqual(workflow_step('Installer and release checks (isolated, never install)')['env']['PYTHONPATH'], 'tools')

    def test_workflow_does_not_depend_on_private_descriptions(self):
        workflow = WORKFLOW.read_text(encoding="utf-8").replace("\\", "/")
        for name in ("docs/distribution/README.md", "docs/README.md", "docs/licenses/README.md",
                     "docs/native-build.md"):
            self.assertNotIn(name, workflow)
        self.assertIn('Copy-Item -Recurse docs/licenses "$pkg/docs/licenses"', workflow)

    def test_native_preparation_is_required_and_download_cache_only(self):
        steps = workflow_steps()
        names = [step.get('name') for step in steps]
        prepare = workflow_step('Prepare mandatory no-TTS Sherpa native')
        self.assertIn('--budget-seconds 1500 --validation-seconds 180 --max-download-mib 256', prepare['run'])
        self.assertNotIn('if', prepare)
        self.assertNotIn('continue-on-error', prepare)
        index = names.index(prepare['name'])
        for i, step in enumerate(steps):
            if 'cargo ' in step.get('run', ''):
                self.assertLess(index, i)
        cache = workflow_step('Restore Sherpa download archives and CMake wheel')['with']
        for forbidden in ('target/', 'source-lock.json', 'native/install'):
            self.assertNotIn(forbidden, cache['path'])
        self.assertNotIn('restore-keys', cache)
        for forbidden in ('SHERPA_ONNX_LIB_DIR', 'NEO_SHERPA_ASR_ONLY', 'GITHUB_PATH', 'upload-artifact', 'download-artifact'):
            self.assertNotIn(forbidden, json.dumps(steps))
        self.assertEqual(workflow_step('Native preparation offline regressions')['run'],
                         'python -B -m unittest tools.test_prepare_sherpa_ci -v')
        self.assertLess(names.index('Native preparation offline regressions'), index)
        self.assertLess(names.index('Verify pinned drawing source and legal bodies'), index)

    def test_drawing_root_package_tests_do_not_use_pythonpath_workaround(self):
        step = workflow_step('Drawing release root-package regressions')
        self.assertEqual(step['run'], 'python -B -m unittest tools.test_assemble_drawing_release tools.test_release_workflow -v')
        self.assertNotIn('env', step)

    def test_drawing_check_uses_lock_pin_without_release_approval_or_upload(self):
        steps = workflow_steps()
        names = [step.get('name') for step in steps]
        self.assertLess(names.index('Checkout'), names.index('Read drawing pin'))
        self.assertLess(names.index('Read drawing pin'), names.index('Checkout pinned drawing'))
        checkout = workflow_step('Checkout pinned drawing')['with']
        self.assertEqual(checkout['ref'], '${{ steps.drawing-pin.outputs.commit }}')
        self.assertEqual(checkout['repository'], '${{ steps.drawing-pin.outputs.repository }}')
        self.assertEqual(checkout['path'], 'target/package/drawing-src')
        protocol = workflow_step('Drawing synthetic protocol tests')
        self.assertEqual(protocol['working-directory'], 'target/package/drawing-src')
        self.assertIn('cargo +${{ steps.drawing-pin.outputs.rust }} test --locked -p board-protocol --lib --target x86_64-pc-windows-msvc', protocol['run'])
        self.assertIn('target/package/drawing-tests"', protocol['run'])
        self.assertIn('rustup toolchain install ${{ steps.drawing-pin.outputs.rust }}', workflow_step('Install drawing toolchain')['run'])
        build = workflow_step('Build drawing and check both windowless versions')
        self.assertEqual(build['run'], 'python tools/assemble_drawing_release.py build --source target/package/drawing-src --target-dir target/package/drawing-build')
        for name in ('Read drawing pin', 'Checkout pinned drawing', 'Verify pinned drawing source and legal bodies',
                     'Install drawing toolchain', protocol['name'], build['name']):
            step = workflow_step(name)
            self.assertNotIn('if', step)
            for forbidden in ('upload-artifact', 'check-approval', 'verify-review', '--ignored', '--include-ignored', '--workspace', '--gui', 'continue-on-error', 'models/'):
                self.assertNotIn(forbidden, json.dumps(step))

    def test_release_drawing_source_check_precedes_build_and_assembly_precedes_archive(self):
        steps = workflow_steps()
        names = [step.get('name') for step in steps]
        order = ['Determine version', 'Read drawing pin', 'Checkout pinned drawing',
                 'Verify pinned drawing source and legal bodies', 'Drawing synthetic protocol tests',
                 'Build drawing and check both windowless versions', 'Build', 'Assemble package',
                 'Audit packaged PE imports (external CRT prerequisite)', 'Assemble pinned drawing release', 'Zip portable package']
        self.assertEqual([names.index(value) for value in order], sorted(names.index(value) for value in order))
        source = workflow_step('Verify pinned drawing source and legal bodies')
        self.assertNotIn('if', source)
        assembly = workflow_step('Assemble pinned drawing release')
        self.assertEqual(assembly['if'], RELEASE_IF)
        for step in steps:
            self.assertNotIn('continue-on-error', step)
            if RELEASE_IF in step.get('if', ''):
                for forbidden in ('package_combined.py', 'download-artifact', '--mode evaluation', 'public_approved =', '--force', '--skip'):
                    self.assertNotIn(forbidden, step.get('run', ''))
        self.assertIn('tools.test_assemble_drawing_release', workflow_step('Drawing release root-package regressions')['run'])

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
        step = workflow_step("Download STT models")
        script = pwsh_step("Download STT models")
        self.assertEqual(int(step["timeout-minutes"]), 20)
        self.assertIn("$ProgressPreference = 'SilentlyContinue'", script)
        self.assertIn("curl.exe --fail --location --connect-timeout 20 --max-time 600 "
                      "--retry 2 --retry-max-time 900 --progress-bar --output $part $Uri", script)
        self.assertNotIn("Invoke-WebRequest", script)
        self.assertIn(r"tar -xjf .cache\stt\sv.tar.bz2 -C .cache\stt\extracted", script)
        self.assertIn("if ($LASTEXITCODE -ne 0)", script)
        self.assertNotIn("assets-stt", script)
        self.assertNotIn("Invoke-Expression", script)
        for name in ("Restore STT raw downloads", "Save STT raw downloads"):
            cache = workflow_step(name)["with"]
            self.assertEqual(cache["path"].splitlines(),
                             [".cache/stt/sv.tar.bz2", ".cache/stt/silero_vad.onnx"])

    def test_release_runtime_and_stt_pins_are_exact_and_checked_before_use(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn(
            "        run: python tools/fetch_runtime.py --mirror github --version 2.55.0.windows.5 "
            "--sha256 56d7b226b7693196cfc71fef26568f536c4a021ab6c37ff2db4287bed908e96e --archive-cache .cache/runtime-archives\n", workflow)
        script = pwsh_step("Download STT models")
        self.assertIn("$ErrorActionPreference = 'Stop'", script)
        for variable, path, digest, failure in (
            ("sv", r".cache\stt\sv.tar.bz2", "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e", "SenseVoice archive"),
            ("vad", r".cache\stt\silero_vad.onnx", "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6", "Silero"),
        ):
            self.assertIn(f"${variable}Expected = '{digest}'", script)
            call = f"{path} ${variable}Expected '{failure}'"
            self.assertIn(call, script)
            self.assertIn(f"${variable}Hash = Get-SttDownload", script)
            self.assertLess(script.index(call), script.index("tar -xjf"))
            self.assertLess(script.index(call), script.index("Copy-Item"))
        for path in ("$Path", "$part"):
            hash_line = f"$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath {path}).Hash.ToLowerInvariant()"
            self.assertIn(hash_line, script)
            self.assertLess(script.index(hash_line), script.index("Move-Item"))
        self.assertEqual(script.count("if ($hash -cne $Expected)"), 2)
        self.assertLess(script.index("if ($hash -cne $Expected)", script.index("curl.exe")),
                        script.index("Move-Item"))
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

    def test_source_stage_precedes_binaries_and_publish_uses_verified_draft_helper(self):
        steps = workflow_steps()
        names = [step.get('name') for step in steps]
        workflow = '\n'.join(step.get('run', '') for step in steps)
        release = '\n'.join(step.get('run', '') for step in steps if RELEASE_IF in step.get('if', ''))
        order = ["Assemble pinned drawing release", "Stage pinned source companions (fail closed)",
                 "Zip portable package", "Build installer (NSIS)", "Create release"]
        self.assertEqual([names.index(name) for name in order], sorted(names.index(name) for name in order))
        stage = pwsh_step("Stage pinned source companions (fail closed)")
        publish = pwsh_step("Create release")
        self.assertIn("tools/stage_source_companions.py stage", stage)
        self.assertIn("--distribution target/package/runtime-distribution", stage)
        self.assertIn("--native-lib target/sherpa-asr/native/install/lib", stage)
        self.assertIn("--work-root target/source-delivery", stage)
        self.assertIn("--budget-seconds 600 --max-download-mib 1024 --max-expanded-mib 2048", stage)
        self.assertIn("tools/stage_source_companions.py publish", publish)
        self.assertIn('--prerelease "$env:RELEASE_PRERELEASE"', publish)
        for script in (stage, publish):
            self.assertIn("--lock tools/source-companions.lock.json --package dist/neo --output dist", script)
            self.assertIn('--repository "$env:RELEASE_REPOSITORY" --tag "$env:RELEASE_TAG" --version "$env:RELEASE_VERSION"', script)
            self.assertIn("if ($LASTEXITCODE -ne 0) { throw", script)
        self.assertNotRegex(release, r"(?m)^\s+gh (?:release|@args)")
        self.assertIn("python -B -m unittest tools.test_stage_source_companions -v", workflow)
        self.assertNotIn("target/runtime-distribution/mingit-", release)
        self.assertNotIn("target/native-source/neo-native-sources.zip", release)


@unittest.skipUnless(PWSH, "pwsh is not installed")
class SherpaCleanTests(unittest.TestCase):
    def test_both_profiles_are_cleaned_and_each_failure_stops_the_step(self):
        for fail_call in (0, 1, 2):
            with self.subTest(fail_call=fail_call), tempfile.TemporaryDirectory(prefix='neo-clean-') as td:
                root = Path(td)
                mocks = '''
                    $script:calls = 0
                    function cargo {
                      Add-Content -LiteralPath calls.txt -Value ($args -join ' ')
                      $script:calls += 1
                      $global:LASTEXITCODE = if ($script:calls -eq FAIL_CALL) { 7 } else { 0 }
                    }
                '''.replace('FAIL_CALL', str(fail_call))
                script = root / 'clean.ps1'
                script.write_text(textwrap.dedent(mocks) + pwsh_step('Invalidate cached Sherpa Rust bindings'), encoding='utf-8')
                result = subprocess.run([PWSH, '-NoLogo', '-NoProfile', '-NonInteractive', '-File', str(script)],
                                        cwd=root, capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 7 if fail_call else 0, result.stdout + result.stderr)
                command = 'clean -p sherpa-onnx-sys -p sherpa-onnx --target x86_64-pc-windows-msvc'
                expected = [command] if fail_call == 1 else [command, command + ' --release']
                self.assertEqual((root / 'calls.txt').read_text(encoding='utf-8-sig').splitlines(), expected)


@unittest.skipUnless(PWSH, "pwsh is not installed")
class SttDownloadTests(unittest.TestCase):
    fixtures = {"sv": b"mock archive", "vad": b"mock vad"}
    filenames = {"sv": "sv.tar.bz2", "vad": "silero_vad.onnx"}
    pins = {"sv": "7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e",
            "vad": "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"}

    def run_script(self, *, cached=(), bad_cache=None, bad_download=None, network=None,
                   extract_failure=False, uppercase_pin=None, dirty=None, stale_part=False):
        temporary = tempfile.TemporaryDirectory(prefix="neo-stt-pins-")
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        cache = root / ".cache/stt"
        cache.mkdir(parents=True)
        for name in cached:
            (cache / self.filenames[name]).write_bytes(
                b"corrupt cache" if name == bad_cache else self.fixtures[name])
        if dirty:
            (cache / dirty).mkdir()
            (cache / dirty / "sentinel").write_bytes(b"do not reuse")
        if stale_part:
            (cache / "sv.tar.bz2.part").write_bytes(b"interrupted download")
        script = pwsh_step("Download STT models")
        # Read the original workflow each time; only upstream pins become fixture pins.
        for name, pin in self.pins.items():
            digest = hashlib.sha256(self.fixtures[name]).hexdigest()
            script = script.replace(pin, digest.upper() if name == uppercase_pin else digest)
        (root / "config.json").write_text(json.dumps({
            "bad_download": bad_download, "network": network, "extract_failure": extract_failure,
        }), encoding="utf-8")
        mocks = r'''
            $config = Get-Content config.json -Raw | ConvertFrom-Json
            function Get-FileHash {
              param($Algorithm, $LiteralPath)
              $name = ($LiteralPath.Replace('\', '/') -split '/')[-1]
              Add-Content events.txt "hash:$name"
              Microsoft.PowerShell.Utility\Get-FileHash -Algorithm $Algorithm -LiteralPath $LiteralPath
            }
            function Move-Item {
              param($LiteralPath, $Destination)
              $name = ($Destination.Replace('\', '/') -split '/')[-1]
              Add-Content events.txt "move:$name"
              if (-not $LiteralPath.EndsWith('.part')) { throw 'Expected partial source' }
              if (Test-Path -LiteralPath $Destination) { throw 'Raw destination already exists' }
              Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
            }
            function Invoke-WebRequest { throw 'Unexpected Invoke-WebRequest' }
            function curl.exe {
              $expected = @('--fail', '--location', '--connect-timeout', '20', '--max-time', '600',
                            '--retry', '2', '--retry-max-time', '900', '--progress-bar', '--output')
              if ($args.Count -ne 14) { throw 'Unexpected curl argument count' }
              for ($i = 0; $i -lt $expected.Count; $i++) {
                if ($args[$i] -cne $expected[$i]) { throw "Unexpected curl argument at $i" }
              }
              $outFile = $args[12]
              $uri = $args[13]
              $name = if ($uri.EndsWith('.tar.bz2')) { 'sv' } else { 'vad' }
              Add-Content events.txt "curl:$name"
              $leaf = if ($name -eq 'sv') { 'sv.tar.bz2' } else { 'silero_vad.onnx' }
              if (-not $outFile.EndsWith("$leaf.part")) { throw 'Expected partial output' }
              if (Test-Path -LiteralPath $outFile) { throw 'Stale partial not removed' }
              if (Test-Path -LiteralPath ".cache/stt/$leaf") { throw 'Cache hit downloaded again' }
              $content = if ($name -eq 'sv') { 'mock archive' } else { 'mock vad' }
              if ($config.bad_download -eq $name) { $content = 'corrupt download' }
              [IO.File]::WriteAllText((Join-Path $PWD $outFile), $content)
              # Even a complete-looking body must be rejected after a network error.
              $global:LASTEXITCODE = if ($config.network -eq $name) { 28 } else { 0 }
            }
            function tar {
              Add-Content events.txt 'tar'
              if ($config.extract_failure) { $global:LASTEXITCODE = 2; return }
              $root = '.cache/stt/extracted/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17'
              New-Item -ItemType Directory $root | Out-Null
              [IO.File]::WriteAllText((Join-Path $PWD "$root/model.int8.onnx"), 'mock model')
              [IO.File]::WriteAllText((Join-Path $PWD "$root/tokens.txt"), 'mock tokens')
              [IO.File]::WriteAllText((Join-Path $PWD "$root/README"), 'mock notice')
              $global:LASTEXITCODE = 0
            }
        '''
        local = root / "assets-stt/local.onnx"
        local.parent.mkdir()
        local.write_bytes(b"local input unchanged")
        path = root / "download.ps1"
        path.write_text(textwrap.dedent(mocks) + script, encoding="utf-8")
        result = subprocess.run([PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(path)],
                                cwd=root, capture_output=True, text=True, errors="replace", timeout=30)
        self.assertEqual(local.read_bytes(), b"local input unchanged")
        events = root / "events.txt"
        return cache, result, events.read_text(encoding="utf-8-sig").splitlines() if events.exists() else []

    def assert_success(self, cache, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(list(cache.glob("*.part")), [])
        manifest = json.loads((cache / "member-sha256.json").read_text(encoding="utf-8-sig"))
        self.assertEqual(manifest["sense_voice_archive_sha256"], hashlib.sha256(self.fixtures["sv"]).hexdigest())
        self.assertEqual(manifest["silero_sha256"], hashlib.sha256(self.fixtures["vad"]).hexdigest())
        self.assertEqual(len(manifest["members"]), 4)
        for member in manifest["members"]:
            base = cache / "extracted" if member["archive"] else cache
            data = (base / member["path"]).read_bytes()
            self.assertEqual(member["size"], len(data))
            self.assertEqual(member["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual({p.relative_to(cache / "models").as_posix(): p.read_bytes()
                          for p in (cache / "models").rglob("*") if p.is_file()},
                         {"sense-voice/model.int8.onnx": b"mock model", "sense-voice/tokens.txt": b"mock tokens",
                          "vad/silero_vad.onnx": self.fixtures["vad"]})

    def assert_download_failure(self, cache, result, events, message):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(message, result.stdout + result.stderr)
        self.assertNotIn("tar", events)
        self.assertFalse((cache / "extracted").exists())
        self.assertFalse((cache / "models").exists())
        self.assertFalse((cache / "member-sha256.json").exists())
        self.assertEqual(list(cache.glob("*.part")), [])

    def test_actual_script_checks_downloads_before_extract_and_copy(self):
        for bad in ("sv", "vad", None):
            with self.subTest(bad=bad):
                cache, result, events = self.run_script(bad_download=bad)
                if bad:
                    self.assert_download_failure(cache, result, events, "SHA-256 mismatch")
                    self.assertFalse((cache / self.filenames[bad]).exists())
                    self.assertNotIn("move:" + self.filenames[bad], events)
                    self.assertEqual([e for e in events if e.startswith("curl:")],
                                     ["curl:sv"] if bad == "sv" else ["curl:sv", "curl:vad"])
                    continue
                self.assert_success(cache, result)
                self.assertEqual(events[:7], ["curl:sv", "hash:sv.tar.bz2.part", "move:sv.tar.bz2",
                                             "curl:vad", "hash:silero_vad.onnx.part", "move:silero_vad.onnx", "tar"])

    def test_cache_hits_are_rehashed_without_curl(self):
        for cached in (("sv", "vad"), ("sv",), ("vad",)):
            with self.subTest(cached=cached):
                cache, result, events = self.run_script(cached=cached)
                self.assert_success(cache, result)
                self.assertEqual([e for e in events if e.startswith("curl:")],
                                 ["curl:" + name for name in self.fixtures if name not in cached])
                for name in cached:
                    self.assertLess(events.index("hash:" + self.filenames[name]), events.index("tar"))
                    self.assertNotIn("move:" + self.filenames[name], events)

    def test_bad_cache_fails_without_redownload_or_extraction(self):
        for bad in self.fixtures:
            with self.subTest(bad=bad):
                cache, result, events = self.run_script(cached=("sv", "vad"), bad_cache=bad)
                self.assert_download_failure(cache, result, events, "cached SHA-256 mismatch")
                self.assertIn("hash:" + self.filenames[bad], events)
                self.assertFalse(any(e.startswith(("curl:", "move:")) for e in events))
                self.assertEqual((cache / self.filenames[bad]).read_bytes(), b"corrupt cache")

    def test_network_exit_fails_before_hash_promotion_or_extraction(self):
        for failed in self.fixtures:
            with self.subTest(failed=failed):
                cache, result, events = self.run_script(network=failed)
                self.assert_download_failure(cache, result, events, "download failed (curl exit 28)")
                self.assertFalse((cache / self.filenames[failed]).exists())
                self.assertNotIn("hash:" + self.filenames[failed] + ".part", events)
                self.assertNotIn("move:" + self.filenames[failed], events)
                self.assertEqual([e for e in events if e.startswith("curl:")],
                                 ["curl:sv"] if failed == "sv" else ["curl:sv", "curl:vad"])

    def test_extract_failure_stops_before_manifest_and_copy(self):
        cache, result, events = self.run_script(extract_failure=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("STT model extraction failed", result.stdout + result.stderr)
        self.assertEqual(events[-1], "tar")
        self.assertFalse((cache / "member-sha256.json").exists())
        self.assertEqual([p for p in (cache / "models").rglob("*") if p.is_file()], [])
        self.assertEqual(list(cache.glob("*.part")), [])
        for name, content in self.fixtures.items():
            self.assertEqual((cache / self.filenames[name]).read_bytes(), content)

    def test_sha_comparison_is_case_sensitive_for_cache_and_download(self):
        for cached in ((), ("sv", "vad")):
            for name in self.fixtures:
                with self.subTest(cached=cached, name=name):
                    cache, result, events = self.run_script(cached=cached, uppercase_pin=name)
                    self.assert_download_failure(cache, result, events, "SHA-256 mismatch")
                    self.assertNotIn("move:" + self.filenames[name], events)

    def test_dirty_extraction_and_model_directories_are_rejected(self):
        for dirty in ("extracted", "models"):
            with self.subTest(dirty=dirty):
                cache, result, events = self.run_script(cached=("sv", "vad"), dirty=dirty)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("STT extraction and model directories must be clean", result.stdout + result.stderr)
                self.assertEqual(events, [])
                self.assertEqual((cache / dirty / "sentinel").read_bytes(), b"do not reuse")
                self.assertFalse((cache / "member-sha256.json").exists())

    def test_stale_partial_is_discarded_not_resumed(self):
        cache, result, events = self.run_script(stale_part=True)
        self.assert_success(cache, result)
        self.assertEqual(events[0], "curl:sv")


@unittest.skipUnless(PWSH, "pwsh is not installed")
class AssemblePackageTests(unittest.TestCase):
    @patch('tools.check_release.verify_distribution')
    def test_actual_assembly_separates_models_dlls_cache_docs_and_languages(self, filtered):
        with tempfile.TemporaryDirectory(prefix="neo-package-layout-") as td:
            root = Path(td)
            sources = {
                "target/ci-rust/x86_64-pc-windows-msvc/release/neo.exe": "neo.exe",
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
