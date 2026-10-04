"""Isolated regression tests; never download or execute a real runtime."""
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import wave
import zipfile

SPEC = importlib.util.spec_from_file_location("fetch_runtime", Path(__file__).with_name("fetch_runtime.py"))
runtime = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runtime)


class RuntimeLayoutTests(unittest.TestCase):
    def test_default_cache_is_inside_source_cache_not_distribution_runtime(self):
        self.assertEqual(Path(runtime.DEST), Path(runtime.ROOT) / ".cache/runtime/gitbash")


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.parent = Path(self.temp.name)
        self.dest = self.parent / "gitbash"
        self.dest.mkdir()
        (self.dest / "old.txt").write_bytes(b"old runtime")
        self.addCleanup(patch.stopall)
        patch.object(runtime, "DEST", str(self.dest)).start()
        patch.object(runtime.urllib.request, "urlopen", side_effect=AssertionError("network forbidden")).start()
        patch.object(subprocess, "run", side_effect=AssertionError("real runtime execution forbidden")).start()

    def main(self, *args):
        with patch("sys.argv", ["fetch_runtime.py", *args]):
            return runtime.main()

    def old_intact(self):
        self.assertEqual((self.dest / "old.txt").read_bytes(), b"old runtime")
        self.assertEqual(sorted(p.name for p in self.parent.iterdir()), ["gitbash"])

    def mock_release(self, members=None):
        patch.object(runtime, "pick_asset", return_value=("v9", "runtime.zip", "mock://asset")).start()
        members = members if members is not None else {"usr/bin/sh.exe": b"mock bash", "cmd/git.exe": b"mock git"}

        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            for name, contents in members.items():
                archive.writestr(name if isinstance(name, zipfile.ZipInfo) else zipfile.ZipInfo(name), contents)
        data = buffer.getvalue()
        self.archive_sha256 = hashlib.sha256(data).hexdigest()

        def download(tag, asset, mirror, path):
            Path(path).write_bytes(data)
            return "mock"

        return patch.object(runtime, "download", side_effect=download).start()

    def test_cache_hit_for_both_bash_layouts(self):
        for rel in ["bin/bash.exe", "usr/bin/bash.exe"]:
            path = self.dest / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"cached")
            with patch.object(runtime, "pick_asset", side_effect=AssertionError("cache missed")):
                self.assertEqual(self.main(), 0)
            path.unlink()

    def test_explicit_version_requires_matching_cache(self):
        path = self.dest / "bin/bash.exe"
        path.parent.mkdir()
        path.write_bytes(b"cached")
        (self.dest / ".neo-version").write_text("v2.0")
        with patch.object(runtime, "pick_asset", side_effect=SystemExit("requested version unavailable")) as pick:
            self.assertEqual(self.main("--version", "2.0"), 0)
            with self.assertRaises(SystemExit):
                self.main("--version", "3.0")
            pick.assert_called_once_with("3.0")
        self.old_intact()

    def test_corrupt_version_cache_requires_explicit_version_download(self):
        path = self.dest / "bin/bash.exe"
        path.parent.mkdir()
        path.write_bytes(b"cached")
        (self.dest / ".neo-version").write_bytes(b"\xff\xfe")
        with patch.object(runtime, "pick_asset", side_effect=SystemExit("version unavailable")) as pick:
            with self.assertRaisesRegex(SystemExit, "version unavailable"):
                self.main("--version", "9")
            pick.assert_called_once_with("9")
        self.old_intact()

    def test_sha256_cli_rejects_invalid_or_unversioned_pins_before_io(self):
        with patch.object(runtime, "check_local_path") as check, patch.object(runtime, "pick_asset") as pick:
            for args in [("--sha256", "a" * 64),
                         ("--version", "", "--sha256", "a" * 64),
                         ("--version", "v", "--sha256", "a" * 64),
                         *(("--version", "9", "--sha256", value)
                           for value in ("", "a" * 63, "a" * 65, "g" * 64, "a" * 63 + "\n", "sha256:" + "a" * 64))]:
                with self.subTest(args=args), self.assertRaises(SystemExit) as error:
                    self.main(*args)
                self.assertEqual(error.exception.code, 2)
            check.assert_not_called()
            pick.assert_not_called()
        self.old_intact()

    def test_matching_sha256_installs_and_pinned_cache_hits(self):
        download = self.mock_release()
        with patch.object(runtime, "smoke_test", return_value="5.2"):
            self.assertEqual(self.main("--version", "9", "--sha256", self.archive_sha256.upper()), 0)
        self.assertEqual((self.dest / ".neo-version").read_text(), "v9")
        self.assertEqual((self.dest / ".neo-archive-sha256").read_text(), self.archive_sha256)
        self.assertEqual((self.dest / "usr/bin/bash.exe").read_bytes(), b"mock bash")
        with patch.object(runtime, "pick_asset", side_effect=AssertionError("cache missed")):
            self.assertEqual(self.main("--version", "v9", "--sha256", self.archive_sha256), 0)
        download.assert_called_once()

    def test_wrong_sha256_rejected_before_zip_open_and_keeps_old_cache(self):
        self.mock_release()
        (self.dest / ".neo-version").write_text("v8")
        (self.dest / ".neo-archive-sha256").write_text("b" * 64)
        before = {p.relative_to(self.dest): p.read_bytes() for p in self.dest.rglob("*") if p.is_file()}
        with patch.object(runtime.zipfile, "ZipFile", side_effect=AssertionError("ZIP opened before hash")), \
                patch.object(runtime, "smoke_test") as smoke:
            self.assertEqual(self.main("--version", "9", "--sha256", "0" * 64), 1)
            smoke.assert_not_called()
        self.assertEqual(before, {p.relative_to(self.dest): p.read_bytes() for p in self.dest.rglob("*") if p.is_file()})
        self.old_intact()

    def test_pinned_cache_requires_both_markers_and_force_redownloads(self):
        pick_asset = runtime.pick_asset
        download = self.mock_release()
        bash = self.dest / "bin/bash.exe"
        bash.parent.mkdir()
        bash.write_bytes(b"cached")
        cases = [("v9", None, False), ("v9", "0" * 64, False),
                 ("v8", self.archive_sha256, False), (None, self.archive_sha256, False),
                 ("v9", b"\xff", False), ("v9", self.archive_sha256, True)]
        for tag, digest, force in cases:
            with self.subTest(tag=tag, digest=digest, force=force):
                for name, value in ((".neo-version", tag), (".neo-archive-sha256", digest)):
                    marker = self.dest / name
                    marker.unlink(missing_ok=True)
                    if value is not None:
                        marker.write_bytes(value if isinstance(value, bytes) else value.encode())
                # Exercise the real version-specific API selection, never latest or live network.
                metadata = {"tag_name": "v9", "assets": [{"name": "MinGit-9-64-bit.zip", "browser_download_url": "mock://asset"}]}
                with patch.object(runtime, "pick_asset", wraps=pick_asset), \
                        patch.object(runtime, "fetch_bytes", return_value=json.dumps(metadata).encode()) as fetch, \
                        patch.object(runtime, "smoke_test", return_value="5.2"):
                    self.assertEqual(self.main("--version", "9", "--sha256", self.archive_sha256,
                                               *(["--force"] if force else [])), 0)
                    fetch.assert_called_once_with(
                        "https://api.github.com/repos/git-for-windows/git/releases/tags/v9", timeout=60)
                self.assertEqual((self.dest / ".neo-archive-sha256").read_text(), self.archive_sha256)
        self.assertEqual(download.call_count, len(cases))

    def test_pinned_version_api_failure_never_uses_latest(self):
        with patch.object(runtime, "fetch_bytes", side_effect=OSError("mock API failure")) as fetch:
            with self.assertRaisesRegex(SystemExit, "指定版本"):
                self.main("--version", "9.9", "--sha256", "a" * 64)
            fetch.assert_called_once_with(
                "https://api.github.com/repos/git-for-windows/git/releases/tags/v9.9", timeout=60)
        self.old_intact()

    def test_specified_version_api_failure_does_not_fallback(self):
        with patch.object(runtime, "fetch_bytes", side_effect=OSError("mock API failure")):
            with self.assertRaisesRegex(SystemExit, "指定版本"):
                self.main("--version", "9.9")
            self.assertEqual(runtime.pick_asset(runtime.FALLBACK_VERSION)[0], runtime.FALLBACK_VERSION)
        self.old_intact()

    def test_mismatched_api_release_rejected(self):
        with patch.object(runtime, "fetch_bytes", return_value=json.dumps({"tag_name": "v1", "assets": []}).encode()):
            with self.assertRaisesRegex(SystemExit, "版本不匹配"):
                self.main("--version", "2")
        self.old_intact()

    def test_download_failure_preserves_old(self):
        self.mock_release()
        with patch.object(runtime, "download", side_effect=SystemExit("all mirrors failed")):
            with self.assertRaises(SystemExit):
                self.main("--force")
        self.old_intact()

    def test_invalid_zip_preserves_old(self):
        self.mock_release()
        def bad_download(tag, asset, mirror, path):
            Path(path).write_bytes(b"not a zip")
            return "mock"
        with patch.object(runtime, "download", side_effect=bad_download):
            self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_missing_shell_and_traversal_preserve_old(self):
        for members in [{"README": b"no shell"}, {"../escaped": b"bad"}]:
            self.mock_release(members)
            self.assertEqual(self.main("--force"), 1)
            self.old_intact()

    def test_smoke_failure_preserves_old(self):
        self.mock_release()
        def smoke(path):
            self.assertNotIn(str(self.dest), path)
            self.assertTrue(Path(path).is_file())
            self.assertTrue((self.dest / "old.txt").exists())
            return None
        with patch.object(runtime, "smoke_test", side_effect=smoke):
            self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_success_smokes_stage_then_commits(self):
        self.mock_release()
        with patch.object(runtime, "smoke_test", return_value="5.2") as smoke:
            self.assertEqual(self.main("--force"), 0)
            self.assertIn(".gitbash-stage-", smoke.call_args.args[0])
        self.assertFalse((self.dest / "old.txt").exists())
        self.assertEqual((self.dest / "usr/bin/bash.exe").read_bytes(), b"mock bash")
        self.assertEqual((self.dest / ".neo-version").read_text(), "v9")
        self.assertEqual([p.name for p in self.parent.iterdir()], ["gitbash"])

    def test_locked_old_directory_is_untouched(self):
        self.mock_release()
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime.os, "replace", side_effect=PermissionError("locked")):
            self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_publish_failure_rolls_back(self):
        self.mock_release()
        replace = os.replace
        def failing_replace(src, dst):
            if Path(src).name == "payload":
                raise PermissionError("mock locked target")
            return replace(src, dst)
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime.os, "replace", side_effect=failing_replace):
            self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_interrupt_rolls_back(self):
        self.mock_release()
        replace = os.replace
        def interrupted_replace(src, dst):
            if Path(src).name == "payload":
                raise KeyboardInterrupt()
            return replace(src, dst)
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime.os, "replace", side_effect=interrupted_replace):
            with self.assertRaises(KeyboardInterrupt):
                self.main("--force")
        self.old_intact()

    def test_failed_rollback_keeps_backup(self):
        self.mock_release()
        replace = os.replace
        def failing_replace(src, dst):
            if Path(src) != self.dest:
                raise PermissionError("mock publish and rollback blocked")
            return replace(src, dst)
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime.os, "replace", side_effect=failing_replace):
            with self.assertRaisesRegex(RuntimeError, "回滚失败"):
                self.main("--force")
        backups = list(self.parent.glob(".gitbash-backup-*"))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / "old.txt").read_bytes(), b"old runtime")

    def test_download_size_and_time_budgets(self):
        class Response(io.BytesIO):
            headers = {}
        for headers, data, clock in [({}, b"x" * 17, [0, 0]), ({"Content-Length": "17"}, b"", [0]),
                                     ({"Content-Length": "invalid"}, b"", [0]), ({}, b"x", [0, 901])]:
            response = Response(data)
            response.headers = headers
            with self.subTest(headers=headers, data=data), patch.object(runtime, "MAX_DOWNLOAD_BYTES", 16), \
                    patch.object(runtime.urllib.request, "urlopen", return_value=response), \
                    patch.object(runtime.time, "monotonic", side_effect=clock):
                with self.assertRaises(SystemExit):
                    runtime.download("v9", "runtime.zip", "github", str(self.parent / "test.zip"))
        (self.parent / "test.zip").unlink(missing_ok=True)
        self.old_intact()

    def test_api_size_budget(self):
        with patch.object(runtime, "MAX_API_BYTES", 16), patch.object(runtime.urllib.request, "urlopen", return_value=io.BytesIO(b"x" * 17)):
            with self.assertRaisesRegex(ValueError, "预算"):
                runtime.fetch_bytes("https://mock.invalid")

    def test_zip_budget_rejected_before_extraction(self):
        for setting, limit in [("MAX_ZIP_MEMBERS", 1), ("MAX_EXTRACT_BYTES", 1)]:
            self.mock_release()
            with patch.object(runtime, setting, limit), patch.object(zipfile.ZipFile, "extractall", side_effect=AssertionError("extraction must not start")):
                self.assertEqual(self.main("--force"), 1)
            self.old_intact()

    def test_windows_zip_aliases_and_reparse_entries_rejected(self):
        names = ["/absolute", "bin/../outside", "bin/CON.exe", "bin/bash.exe.", "bin//bash.exe", "bin/./bash.exe", "bin/bash.exe:stream",
                 "bin/COM¹.exe", "bin/LPT².txt", "bin/name?.dll", "bin/name*.dll", 'bin/name".dll', "bin/name<.dll", "bin/name>.dll", "bin/name|.dll", "bin//"]
        for name in names:
            with self.subTest(name=name):
                self.mock_release({name: b"bad"})
                with patch.object(runtime, "smoke_test", side_effect=AssertionError("invalid archive executed")), \
                        patch.object(zipfile.ZipFile, "extractall", side_effect=AssertionError("invalid archive extracted")):
                    self.assertEqual(self.main("--force"), 1)
                self.old_intact()
        for attributes in [(0o120777 << 16), 0x400, (0o010777 << 16)]:
            info = zipfile.ZipInfo("usr/bin/bash.exe")
            info.external_attr = attributes
            self.mock_release({info: b"bad"})
            self.assertEqual(self.main("--force"), 1)
            self.old_intact()
        self.mock_release({"bin/bash.exe": b"one", "BIN/BASH.EXE": b"two"})
        self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_nul_truncated_zip_name_rejected(self):
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as archive:
            archive.writestr("bin/bash.exeXevil", b"bad")
        data = io.BytesIO(data.getvalue().replace(b"bash.exeXevil", b"bash.exe\0evil"))
        with zipfile.ZipFile(data) as archive:
            with self.assertRaises(ValueError):
                runtime.extract_runtime(archive, str(self.parent))
        self.old_intact()

    def test_reparse_parent_rejected_before_network_or_cache_hit(self):
        from types import SimpleNamespace
        original = runtime.os.lstat
        def attributes(path, *args, **kwargs):
            if os.path.normpath(path) == str(self.parent):
                return SimpleNamespace(st_mode=0o40755, st_file_attributes=0x400)
            return original(path, *args, **kwargs)
        with patch.object(runtime.os, "lstat", side_effect=attributes):
            self.assertEqual(self.main("--force"), 1)
        self.old_intact()

    def test_path_denied_before_cache_and_rechecked_before_commit(self):
        with patch.object(runtime.os, "lstat", side_effect=PermissionError("denied")):
            self.assertEqual(self.main(), 1)
        self.old_intact()
        self.mock_release()
        check = runtime.check_local_path
        calls = []
        def changed_path(path):
            calls.append(path)
            if path == str(self.dest):
                raise ValueError("destination replaced by junction during download")
            check(path)
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime, "check_local_path", side_effect=changed_path), \
                patch.object(runtime.os, "replace", side_effect=AssertionError("unsafe commit")):
            self.assertEqual(self.main("--force"), 1)
        self.assertTrue(any(Path(p).name == "payload" for p in calls))
        self.old_intact()

    def test_new_install_publishes_version_without_backup(self):
        shutil.rmtree(self.dest)
        self.mock_release()
        with patch.object(runtime, "smoke_test", return_value="5.2"):
            self.assertEqual(self.main(), 0)
        self.assertEqual((self.dest / ".neo-version").read_text(), "v9")
        self.assertEqual([p.name for p in self.parent.iterdir()], ["gitbash"])

    def test_failed_upgrade_preserves_version_and_nested_data(self):
        (self.dest / ".neo-version").write_text("v8")
        (self.dest / "custom").mkdir()
        (self.dest / "custom/user.bin").write_bytes(b"custom data")
        self.mock_release()
        replace = os.replace
        def fail_publish(src, dst):
            if Path(src).name == "payload":
                raise OSError("publish failure")
            return replace(src, dst)
        with patch.object(runtime, "smoke_test", return_value="5.2"), patch.object(runtime.os, "replace", side_effect=fail_publish):
            self.assertEqual(self.main("--force"), 1)
        self.assertEqual((self.dest / ".neo-version").read_text(), "v8")
        self.assertEqual((self.dest / "custom/user.bin").read_bytes(), b"custom data")
        self.old_intact()

    def test_smoke_uses_only_staged_path_and_clean_environment(self):
        bash = self.parent / "stage/usr/bin/bash.exe"
        poisoned = {"BASH_ENV": "evil", "ENV": "evil", "SHELLOPTS": "xtrace", "LD_PRELOAD": "evil.dll", "BASH_FUNC_command%%": "() { return 0; }"}
        with patch.dict(os.environ, poisoned), patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "5.2\n", "")) as run:
            self.assertEqual(runtime.smoke_test(str(bash)), "5.2")
            env = run.call_args.kwargs["env"]
            self.assertTrue(all(str(self.parent / "stage") in p for p in env["PATH"].split(os.pathsep)))
            for key in poisoned:
                self.assertNotIn(key, env)
            self.assertEqual(run.call_args.kwargs["cwd"], str(self.parent / "stage"))
            self.assertEqual(run.call_args.args[0][:4], [str(bash), "--noprofile", "--norc", "-c"])

    def test_real_smoke_failure_branches_keep_old_runtime(self):
        for result in [OSError("not an executable"), subprocess.TimeoutExpired("bash", 30), subprocess.CompletedProcess([], 9, "", "failed")]:
            self.mock_release()
            kwargs = {"side_effect": result} if isinstance(result, BaseException) else {"return_value": result}
            with patch.object(subprocess, "run", **kwargs) as run:
                self.assertEqual(self.main("--force"), 1)
                run.assert_called_once()
            self.old_intact()


@unittest.skipUnless(os.name == "nt", "PowerShell tests require Windows")
class RirTests(unittest.TestCase):
    def test_mock_curl_atomic_download_and_cache_validation(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            for name, sample in [("old.wav", b"\x01\x00"), ("new.wav", b"\x02\x00")]:
                with wave.open(str(root / name), "wb") as wav:
                    wav.setnchannels(1)
                    wav.setsampwidth(2)
                    wav.setframerate(16000)
                    wav.writeframes(sample * 1024)
            (root / "invalid.wav").write_bytes(b"<html>error</html>" * 100)
            valid = (root / "new.wav").read_bytes()
            (root / "truncated.wav").write_bytes(valid[:-1])
            malformed = bytearray(valid)
            malformed[20:22] = (3).to_bytes(2, "little")
            (root / "float16.wav").write_bytes(malformed)
            malformed = bytearray(valid)
            malformed[24:28] = (8000).to_bytes(4, "little")
            (root / "wrong-rate.wav").write_bytes(malformed)
            for name, chunks in [
                ("duplicate-data.wav", valid[12:36] + b"data\x01\x00\x00\x00\x01\x00" * 2),
                ("late-format.wav", valid[36:] + valid[12:36]),
                ("duplicate-format.wav", valid[12:36] * 2 + valid[36:]),
            ]:
                (root / name).write_bytes(b"RIFF" + (len(chunks) + 4).to_bytes(4, "little") + b"WAVE" + chunks)
            script_path = Path(__file__).resolve().parents[1] / "wake-training/dl_rir.ps1"
            ps = r'''
$ErrorActionPreference = 'Stop'
. '__SCRIPT__' -FunctionsOnly
$root = '__ROOT__'
$dst = Join-Path $root 'old.wav'
$old = [Convert]::ToBase64String([IO.File]::ReadAllBytes($dst))
function curl.exe {
    $output = $args[[array]::IndexOf($args, '-o') + 1]
    if ($script:mode -eq 'failed') {
        [IO.File]::WriteAllText($output, 'partial')
        $global:LASTEXITCODE = 22
    } else {
        $source = if ($script:mode -eq 'invalid') { 'invalid.wav' } else { 'new.wav' }
        [IO.File]::Copy((Join-Path $root $source), $output)
        $global:LASTEXITCODE = 0
    }
}
if (-not (Test-RirWav $dst)) { throw 'valid cache rejected' }
foreach ($bad in @('invalid.wav', 'truncated.wav', 'float16.wav', 'wrong-rate.wav', 'duplicate-data.wav', 'late-format.wav', 'duplicate-format.wav')) {
    $checked = Test-RirWav (Join-Path $root $bad)
    if ($checked) { throw "Invalid WAV accepted: $bad; result=$checked" }
}
foreach ($mode in @('failed', 'invalid')) {
    $script:mode = $mode
    if (Save-RirDownload 'mock://rir' $dst { param($p) Test-RirWav $p }) { throw 'failure accepted' }
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($dst)) -ne $old) { throw 'old cache changed' }
    if (@(Get-ChildItem $root -Filter '*.part').Count -ne 0) { throw 'partial leaked' }
}
$script:mode = 'success'
$lock = [IO.File]::Open($dst, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
try {
    if (Save-RirDownload 'mock://rir' $dst { param($p) Test-RirWav $p }) { throw 'locked destination accepted' }
} finally { $lock.Dispose() }
if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($dst)) -ne $old) { throw 'locked old cache changed' }
if (@(Get-ChildItem $root -Filter '*.part').Count -ne 0) { throw 'locked partial leaked' }
$originalCheck = ${function:Test-RirLocalPath}
$script:checkedDestination = 0
function Test-RirLocalPath([string]$Path) {
    if ($Path -eq $dst) {
        $script:checkedDestination++
        return $script:checkedDestination -eq 1
    }
    return & $originalCheck $Path
}
if (Save-RirDownload 'mock://rir' $dst { param($p) Test-RirWav $p }) { throw 'changed destination accepted' }
if ($script:checkedDestination -lt 2) { throw 'destination not rechecked' }
if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($dst)) -ne $old) { throw 'changed path overwrote cache' }
${function:Test-RirLocalPath} = $originalCheck
if (-not (Save-RirDownload 'mock://rir' $dst { param($p) Test-RirWav $p })) { throw 'commit failed' }
if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($dst)) -eq $old) { throw 'new cache missing' }
$missing = Join-Path $root 'missing.wav'
$script:mode = 'failed'
if (Save-RirDownload 'mock://rir' $missing { param($p) Test-RirWav $p }) { throw 'failed download accepted' }
if (Test-Path $missing) { throw 'failed cache created' }
if (@(Get-ChildItem $root -Filter '*.part').Count -ne 0) { throw 'partial leaked' }
$script:mode = 'success'
$script:partLock = $null
try {
    $saved = Save-RirDownload 'mock://rir' $dst {
        param($p)
        $script:partLock = [IO.File]::Open($p, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
        return $false
    }
    if ($saved) { throw 'invalid locked partial accepted' }
} finally { if ($script:partLock) { $script:partLock.Dispose() } }
Get-ChildItem $root -Filter '*.part' | Remove-Item
'''.replace('__SCRIPT__', str(script_path).replace("'", "''")).replace('__ROOT__', td.replace("'", "''"))
            encoded = base64.b64encode(ps.encode("utf-16le")).decode("ascii")
            result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded], capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
