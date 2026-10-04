"""Offline release-workflow regressions using disposable payloads and Git repositories."""
import ast
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest

from tools.check_release import validate_payload


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
    def test_stt_download_and_extraction_stay_in_cache(self):
        script = pwsh_step("Download STT models")
        self.assertIn(r"-OutFile .cache\stt\sv.tar.bz2", script)
        self.assertIn(r"tar -xjf .cache\stt\sv.tar.bz2 -C .cache\stt\extracted", script)
        self.assertIn("if ($LASTEXITCODE -ne 0)", script)
        self.assertIn(r"-OutFile .cache\stt\models\vad\silero_vad.onnx", script)
        self.assertNotIn("assets-stt", script)

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
class AssemblePackageTests(unittest.TestCase):
    def test_actual_assembly_separates_models_dlls_cache_docs_and_languages(self):
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
                ".cache/runtime/gitbash/usr/bin/bash.exe": "runtime/gitbash/usr/bin/bash.exe",
                "resources/lang/zh-CN.lang": "resources/lang/zh-CN.lang",
                "resources/lang/en-US.lang": "resources/lang/en-US.lang",
                "docs/distribution/README.md": "docs/README.md", "LICENSE": "LICENSE",
            }
            excluded = ["README.md", "crates/neo-wake/assets/README.md", "crates/neo-wake/assets/experimental.onnx",
                        ".cache/stt/sv.tar.bz2", ".cache/stt/extracted/unused.txt",
                        ".cache/runtime/.gitbash-install.lock", ".cache/runtime/other/file.txt",
                        "runtime/gitbash/old-cache.txt", "assets-stt/old-model.onnx",
                        "resources/lang/README.md", "resources/lang/draft.json", "resources/lang/fr-FR.lang",
                        "resources/lang/nested/zh-CN.lang"]
            for name in [*sources, *excluded]:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            for language, translation in (("zh-CN", "你好，{name}"), ("en-US", "Hello, {name}")):
                (root / f"resources/lang/{language}.lang").write_text(
                    json.dumps({"你好，{name}": translation}, ensure_ascii=False), encoding="utf-8")
            expected = {dest: (root / src).read_bytes() for src, dest in sources.items()}
            script = root / "assemble.ps1"
            script.write_text(pwsh_step("Assemble package"), encoding="utf-8")
            command = [PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-File", str(script)]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            package = root / "dist/neo"
            actual = {p.relative_to(package).as_posix(): p.read_bytes() for p in package.rglob("*") if p.is_file()}
            self.assertEqual(actual, expected)
            validate_payload(package)
            # A retry must not merge a dirty package, remove user files or change prior payloads.
            (package / "custom.txt").write_bytes(b"keep")
            result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Package directory must be clean", result.stdout + result.stderr)
            self.assertEqual((package / "custom.txt").read_bytes(), b"keep")
            for name, data in actual.items():
                self.assertEqual((package / name).read_bytes(), data)
            # Each required language must independently fail the actual PowerShell copy.
            for language in ("zh-CN", "en-US"):
                with self.subTest(missing_language=language):
                    shutil.rmtree(package)
                    name = f"resources/lang/{language}.lang"
                    (root / name).unlink()
                    result = subprocess.run(command, cwd=root, capture_output=True, text=True, timeout=30)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn(f"{language}.lang", result.stdout + result.stderr)
                    with self.assertRaisesRegex(ValueError, "Required release resource missing/empty"):
                        validate_payload(package)
                    (root / name).write_bytes(expected[name])


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
