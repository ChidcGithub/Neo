"""Offline release-workflow regressions using only disposable Git repositories."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest


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
