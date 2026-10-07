"""文档一致性检查的合成夹具回归测试；离线、不执行被检查代码。"""
import io
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

if __package__:
    from . import check_docs_consistency as c
else:
    import check_docs_consistency as c


TOML = """[workspace]
members = ["crates/neo-tools"]

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.95"
license = "Apache-2.0"
"""

TOOLS = "\n".join(
    '    Tool { name: "%s", title: "t", purpose: "p", risk: Risk::%s, params: &[], preview: p, run: r },'
    % (name, risk)
    for name, risk in (
        ("read_file", "Read"), ("web_search", "Read"), ("ask_user", "Read"),
        ("open_file", "Open"), ("write_file", "Write"), ("bash", "Exec"),
    )
)

README = """# Neo

[![platform](https://img.shields.io/badge/platform-Windows%2010%202004%2B-0078D6)](#requirements)
[![rust](https://img.shields.io/badge/rust-1.95%2B-orange)](#build-from-source)

## Feature Highlights

6 tools: files and shell.

## Requirements

**Windows 10 2004 (20H1), build 19041, or later** with WDA_EXCLUDEFROMCAPTURE.
**Rust 1.95+**

## Download & Install

The workspace is preparing **`0.1.0`**; this is not an announcement.

## Build from Source

install Rust 1.95+, Python 3.11+ and the Visual Studio C++ build tools.

## Architecture

Tools[neo-tools<br/>6 gated tools]

## Tool Chain

| Risk | Tools | Policy |
|---|---|---|
| `read` | read_file · web_search | run immediately |
| `open` | open_file | run immediately |
| `ask` | ask_user | the popup is the answer |
| `write` | write_file | confirmation |
| `exec` | bash | confirmation |

## Versioning & Releasing

GitHub Release auto changelog.

## License

[Apache-2.0](LICENSE) — Copyright (c) 2026 Chidc. See NOTICE.
"""

NOTICE = "Neo\nCopyright (c) 2026 Chidc\n\nSee docs/licenses/.\n"
LICENSE = "Apache License\n   Version 2.0, January 2004\n"
POLICY = "pub enum Risk { Read, Open, Write, Exec }\n"


def make_root(base):
    root = Path(base) / "repo"
    (root / "crates/neo-tools/src/tools").mkdir(parents=True)
    (root / "crates/neo-tools/src").mkdir(parents=True, exist_ok=True)
    (root / "crates/neo-app/src").mkdir(parents=True)
    (root / "resources/lang").mkdir(parents=True)
    (root / "docs/licenses/models").mkdir(parents=True)
    (root / "docs/licenses/assets").mkdir(parents=True)
    (root / "docs/licenses/runtime").mkdir(parents=True)
    (root / "docs/licenses/supplemental").mkdir(parents=True)
    (root / "Cargo.toml").write_text(TOML, encoding="utf-8")
    (root / "crates/neo-tools/src/tools/mod.rs").write_text(TOOLS, encoding="utf-8")
    (root / "crates/neo-tools/src/policy.rs").write_text(POLICY, encoding="utf-8")
    (root / "README.md").write_text(README, encoding="utf-8")
    (root / "NOTICE").write_text(NOTICE, encoding="utf-8")
    (root / "LICENSE").write_text(LICENSE, encoding="utf-8")
    (root / "docs/licenses/cargo-notices.txt").write_text("notices", encoding="utf-8")
    (root / "docs/licenses/models/hi_neo-model.json").write_text(
        json.dumps({"path": "resources/models/wake/hi_neo.onnx", "license": "MIT",
                    "license_file": "hi_neo-MIT.txt", "sha256": "00", "size": 1}),
        encoding="utf-8")
    (root / "docs/licenses/models/hi_neo-MIT.txt").write_text("MIT", encoding="utf-8")
    (root / "docs/licenses/assets/OFL.txt").write_text("font", encoding="utf-8")
    (root / "docs/licenses/runtime/ort-LICENSE").write_text("ort", encoding="utf-8")
    (root / "docs/licenses/supplemental/index.json").write_text("[]", encoding="utf-8")
    catalog = {"你好": "hello", "再见": "bye"}
    for name in ("zh-CN.lang", "en-US.lang"):
        (root / "resources/lang" / name).write_text(
            json.dumps(catalog, ensure_ascii=False), encoding="utf-8")
    (root / "crates/neo-app/src/app.rs").write_text("fn main() {}\n", encoding="utf-8")
    return root


class FixtureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="neo-docconsistency-")
        self.addCleanup(temporary.cleanup)
        self.root = make_root(temporary.name)
        for target in ("socket.socket", "urllib.request.urlopen",
                       "subprocess.Popen", "os.system"):
            self.enterContext(patch(target, side_effect=AssertionError("no network/exec")))

    def errors(self):
        return c.run(self.root)

    def test_clean_fixture_passes(self):
        self.assertEqual(self.errors(), [])

    def test_version_drift_detected(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8")
                        .replace("preparing **`0.1.0`**", "preparing **`9.9.9`**"),
                        encoding="utf-8")
        self.assertTrue(any("version mismatch" in e for e in self.errors()))

    def test_rust_version_drift_detected(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8").replace("1.95", "1.80"),
                        encoding="utf-8")
        self.assertTrue(any("Rust 1.95" in e for e in self.errors()))

    def test_tool_count_drift_detected(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8")
                        .replace("6 tools", "7 tools").replace("6 gated", "7 gated"),
                        encoding="utf-8")
        self.assertTrue(any("7 tools" in e and "registry has 6" in e
                            for e in self.errors()))

    def test_tool_table_drift_detected(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8")
                        .replace("`exec` | bash", "`exec` | bash · format_disk"),
                        encoding="utf-8")
        self.assertTrue(any("tool names differ" in e for e in self.errors()))

    def test_risk_row_drift_detected(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8")
                        .replace("`read` | read_file · web_search",
                                 "`read` | read_file · web_search · bash")
                        .replace("`exec` | bash", "`exec` | (none)"),
                        encoding="utf-8")
        self.assertTrue(any("tool names differ" in e or "risk" in e
                            for e in self.errors()))

    def test_missing_license_file_detected(self):
        (self.root / "docs/licenses/cargo-notices.txt").unlink()
        self.assertTrue(any("cargo-notices.txt" in e for e in self.errors()))

    def test_empty_license_dir_detected(self):
        for path in (self.root / "docs/licenses/models").iterdir():
            path.unlink()
        self.assertTrue(any("docs/licenses/models" in e for e in self.errors()))

    def test_lang_key_drift_detected(self):
        path = self.root / "resources/lang/en-US.lang"
        path.write_text(json.dumps({"你好": "hello"}), encoding="utf-8")
        self.assertTrue(any("keys differ" in e for e in self.errors()))

    def test_empty_english_translation_detected(self):
        path = self.root / "resources/lang/en-US.lang"
        path.write_text(json.dumps({"你好": "hello", "再见": "  "}), encoding="utf-8")
        self.assertTrue(any("empty translations" in e for e in self.errors()))

    def test_dbg_macro_detected(self):
        path = self.root / "crates/neo-app/src/app.rs"
        path.write_text('fn main() { dbg!(1); }\n', encoding="utf-8")
        self.assertTrue(any("dbg!" in e for e in self.errors()))

    def test_todo_detected_outside_exemptions(self):
        path = self.root / "crates/neo-app/src/app.rs"
        path.write_text("// TODO: remove before release\nfn main() {}\n", encoding="utf-8")
        self.assertTrue(any("TODO" in e for e in self.errors()))

    def test_todo_exempt_in_vendored_flex(self):
        flex = self.root / "crates/neo-ui/src/flex"
        flex.mkdir(parents=True)
        (flex / "mod.rs").write_text("// TODO: upstream comment\n", encoding="utf-8")
        self.assertEqual(self.errors(), [])

    def test_changelog_policy_guard(self):
        path = self.root / "README.md"
        path.write_text(path.read_text(encoding="utf-8").replace("auto changelog", "notes"),
                        encoding="utf-8")
        self.assertTrue(any("changelog" in e for e in self.errors()))

    def test_missing_readme_fails_closed(self):
        (self.root / "README.md").unlink()
        self.assertTrue(self.errors())


class CliTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="neo-docconsistency-cli-")
        self.addCleanup(temporary.cleanup)
        self.root = make_root(temporary.name)

    def test_cli_success_and_failure_codes(self):
        output = io.StringIO()
        with patch("sys.stdout", output):
            self.assertEqual(c.main(["--root", str(self.root)]), 0)
        self.assertIn("doc consistency", output.getvalue())
        self.assertIn("no license verdict", output.getvalue())
        (self.root / "NOTICE").write_text("no copyright line\n", encoding="utf-8")
        output = io.StringIO()
        with patch("sys.stdout", output):
            self.assertEqual(c.main(["--root", str(self.root)]), 1)
        self.assertIn("NOT license compliance", output.getvalue())

    def test_real_repository_is_consistent(self):
        """对真实仓库跑一遍：发现漂移就失败，防止文档与代码再次分叉。"""
        errors = c.run(c.ROOT)
        self.assertEqual(errors, [])


if __name__ == "__main__":
    unittest.main()
