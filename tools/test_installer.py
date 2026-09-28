"""Execute NSIS control flow with isolated effects; compile but never run installers."""
import base64
import ntpath
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest


SOURCE = Path(__file__).with_name("installer.nsi").read_text(encoding="utf-8")


class NsModel:
    """Small interpreter of the actual script, with Windows APIs replaced by memory state."""

    def __init__(self, faults=()):
        self.variables = {"$INSTDIR": r"C:\Programs\Neo", "$APPDATA": r"C:\Users\test\AppData\Roaming",
                          "$TEMP": r"C:\Temp", "$StageDir": r"C:\Programs\stage",
                          "$BackupDir": r"C:\Programs\backup", "$PLUGINSDIR": r"C:\Temp\plugins",
                          "$DESKTOP": r"C:\Users\test\Desktop", "$SMPROGRAMS": r"C:\Users\test\Start"}
        self.files = {}
        self.registry = None
        self.exported = None
        self.faults = set(faults)
        self.error = False
        self.stack = []
        self.calls = []
        self.messages = []
        self.aborted = False
        self.attributes = {}

    def value(self, token):
        token = token.strip('"')
        return re.sub(r'\$(?:[A-Za-z][A-Za-z0-9]*|[0-9])', lambda m: str(self.variables.get(m[0], "")), token)

    def lines(self, name):
        marker = f"Function {name}\n"
        if marker not in SOURCE:
            marker = f"Function ${{PREFIX}}{name}\n"
        body = SOURCE.split(marker, 1)[1].split("FunctionEnd", 1)[0]
        def expand(match):
            macro, path, flag, filename = shlex.split(match[0], posix=False)[1:]
            text = SOURCE.split(f"!macro {macro} PATH FLAG NAME\n", 1)[1].split("!macroend", 1)[0]
            return text.replace("${PATH}", path.strip('"')).replace("${FLAG}", flag).replace("${NAME}", filename.strip('"'))
        body = re.sub(r'!insertmacro (?:Save|Restore)Shortcut[^\n]+', expand, body)
        # NSIS relative jumps count instructions, not labels or comments.
        instructions, labels = [], {}
        for line in body.splitlines():
            line = line.strip()
            if not line or line.startswith(";"):
                continue
            if line.endswith(":"):
                labels[line[:-1]] = len(instructions)
            else:
                instructions.append(shlex.split(line, posix=False))
        return instructions, labels

    def execute(self, name):
        lines, labels = self.lines(name)
        pc = 0
        for _ in range(1000):
            if pc == len(lines):
                return
            op, *args = lines[pc]
            jump = None
            v = self.value
            if op == "StrCpy":
                self.variables[args[0]] = v(args[1])[:int(v(args[2]))] if len(args) > 2 else v(args[1])
            elif op == "StrLen":
                self.variables[args[0]] = str(len(v(args[1])))
            elif op == "StrCmp":
                jump = args[2] if v(args[0]).casefold() == v(args[1]).casefold() else (args[3] if len(args) > 3 else None)
            elif op == "IntCmp":
                a, b = int(v(args[0]), 0), int(v(args[1]), 0)
                index = 2 if a == b else (3 if a < b else 4)
                jump = args[index] if len(args) > index else None
            elif op == "IntOp":
                assert args[2] == "&"
                self.variables[args[0]] = str(int(v(args[1]), 0) & int(v(args[3]), 0))
            elif op == "Goto":
                jump = args[0]
            elif op == "IfErrors":
                jump = args[0] if self.error else (args[1] if len(args) > 1 else None)
            elif op == "IfFileExists":
                jump = args[1] if v(args[0]) in self.files else args[2]
            elif op == "ClearErrors":
                self.error = False
            elif op == "SetErrors":
                self.error = True
            elif op == "Push":
                self.stack.append(v(args[0]))
            elif op == "Pop":
                self.variables[args[0]] = self.stack.pop()
            elif op == "Return":
                return
            elif op == "Abort":
                self.aborted = True
                return
            elif op == "Call":
                self.execute(args[0])
            elif op == "GetFullPathName":
                self.variables[args[0]] = ntpath.normpath(v(args[1]))
            elif op == "${GetRoot}":
                self.variables[args[1]] = ntpath.splitdrive(v(args[0]))[0]
            elif op == "${GetParent}":
                path = v(args[0])
                parent = ntpath.dirname(path.rstrip("\\"))
                self.variables[args[1]] = "" if parent == path else parent
            elif op == "System::Call":
                api = " ".join(args)
                if "GetFileAttributesW" in api:
                    path = self.variables["$0"]
                    attrs, err = self.attributes.get(path, (0 if path in self.files else -1, 2))
                    self.variables["$1"] = str(attrs)
                    self.stack.append(str(err))
                elif "CreateMutexW" in api:
                    self.calls.append("mutex")
                    self.variables["$0"] = "0" if "mutex-denied" in self.faults else "100"
                    self.stack.append("183" if "mutex-exists" in self.faults else "0")
                elif "RegOpenKeyExW" in api:
                    self.calls.append("reg-open")
                    self.variables["$1"] = "5" if "reg-denied" in self.faults else ("2" if self.registry is None else "0")
                    self.variables["$0"] = "10"
                elif "RegCloseKey" not in api:
                    raise AssertionError(api)
            elif op == "nsExec::ExecToStack":
                command = " ".join(args)
                action = "export" if ' export ' in command else "import"
                assert '"$PLUGINSDIR\\uninstall.reg"' in command
                self.calls.append(action)
                failed = action in self.faults
                if not failed:
                    if action == "export":
                        self.exported = dict(self.registry)
                    else:
                        self.registry = dict(self.exported)
                self.stack.extend(["output", "1" if failed else "0"])
            elif op == "CopyFiles":
                src, dst = v(args[1]), v(args[2])
                if ("copy", dst) in self.faults:
                    self.error = True
                else:
                    self.files[dst] = self.files[src]
            elif op == "Delete":
                path = v(args[0])
                if ("delete", path) in self.faults:
                    self.error = True
                else:
                    self.files.pop(path, None)
            elif op == "DeleteRegKey":
                self.calls.append("reg-delete")
                if "reg-delete" in self.faults:
                    self.error = True
                else:
                    self.registry = None
            elif op == "Rename":
                src, dst = v(args[0]), v(args[1])
                assert self.variables.get("$OUTDIR") != src
                if ("rename", src) in self.faults:
                    self.error = True
                else:
                    moved = {p: b for p, b in self.files.items() if p.startswith(src + "\\")}
                    for path, data in moved.items():
                        self.files[dst + path[len(src):]] = data
                        del self.files[path]
            elif op == "SetOutPath":
                self.variables["$OUTDIR"] = v(args[0])
            elif op == "RMDir":
                path = v(args[-1])
                if "/r" in args:
                    self.files = {p: b for p, b in self.files.items() if not p.startswith(path + "\\")}
            elif op == "MessageBox":
                self.messages.append(" ".join(args))
            elif op != "InitPluginsDir":
                raise AssertionError(f"Unhandled NSIS instruction: {op}")
            if jump and jump != "0":
                pc = pc + int(jump) if jump.startswith(("+", "-")) else labels[jump]
            else:
                pc += 1
        raise AssertionError(f"{name} did not terminate")


class InstallerTests(unittest.TestCase):
    def shell_paths(self, model):
        return [model.value(p) for p in [r"$DESKTOP\Neo.lnk", r"$SMPROGRAMS\Neo\Neo.lnk", r"$SMPROGRAMS\Neo\卸载 Neo.lnk"]]

    def test_publish_then_shell_failure_restores_previous_state(self):
        for previous in (False, True):
            with self.subTest(previous=previous):
                m = NsModel()
                paths = self.shell_paths(m)
                if previous:
                    m.files.update({p: b"custom old shortcut" for p in paths})
                    m.registry = {"DisplayVersion": "old", "custom": "preserve"}
                old_registry = None if m.registry is None else dict(m.registry)
                m.execute("SaveShellState")
                self.assertFalse(m.error)
                self.assertEqual(m.variables["$ShellSaved"], "1")
                m.variables.update({"$Published": "1", "$OldMoved": str(int(previous)), "$ShellDirty": "1"})
                m.files[r"C:\Programs\Neo\neo.exe"] = b"new"
                if previous:
                    m.files[r"C:\Programs\backup\runtime\custom.txt"] = b"old user data"
                m.files.update({p: b"new shortcut" for p in paths})
                m.registry = {"DisplayVersion": "partial new write"}
                m.execute("InstallAbort")
                self.assertEqual(m.registry, old_registry)
                for p in paths:
                    self.assertEqual(m.files.get(p), b"custom old shortcut" if previous else None)
                self.assertNotIn(r"C:\Programs\Neo\neo.exe", m.files)
                if previous:
                    self.assertEqual(m.files[r"C:\Programs\Neo\runtime\custom.txt"], b"old user data")
                before = dict(m.files)
                m.execute("InstallAbort")
                self.assertEqual(m.files, before, "repeated failure callback must be idempotent")

    def test_no_shell_mutation_when_publish_fails(self):
        m = NsModel()
        m.registry = {"old": "untouched"}
        m.execute("SaveShellState")
        m.variables["$ShellDirty"] = "0"
        m.calls.clear()
        m.execute("RestoreShellState")
        self.assertEqual(m.calls, [])
        self.assertEqual(m.registry, {"old": "untouched"})

    def test_snapshot_denied_or_reparse_aborts_before_copy(self):
        for attrs, err in [(-1, 5), (0x400, 0), (0x10, 0)]:
            m = NsModel()
            path = self.shell_paths(m)[0]
            m.files[path] = b"old"
            m.attributes[path] = (attrs, err)
            m.execute("SaveShellState")
            self.assertTrue(m.error)
            self.assertNotIn("$ShellSaved", m.variables)
            self.assertEqual(m.files, {path: b"old"})
        for fault in ("reg-denied", "export"):
            m = NsModel([fault])
            m.registry = {"old": "state"}
            m.execute("SaveShellState")
            self.assertTrue(m.error)
            self.assertNotIn("$ShellSaved", m.variables)

    def test_shell_restore_failures_are_reported_not_disarmed(self):
        for fault in ("reg-delete", "import", "reg-denied", "shortcut", "reparse"):
            with self.subTest(fault=fault):
                m = NsModel()
                paths = self.shell_paths(m)
                m.files.update({p: b"old" for p in paths})
                m.registry = {"old": "state"}
                m.execute("SaveShellState")
                m.variables["$ShellDirty"] = "1"
                m.registry = {"new": "state"}
                m.files.update({p: b"new" for p in paths})
                if fault == "reparse":
                    m.attributes[paths[0]] = (0x400, 0)
                else:
                    m.faults.add(("copy", paths[0]) if fault == "shortcut" else fault)
                m.execute("RestoreShellState")
                self.assertTrue(m.messages)
                self.assertEqual(m.variables["$ShellSaved"], "1")
                self.assertEqual(m.variables["$ShellDirty"], "1")
                if fault in ("shortcut", "reparse"):
                    self.assertEqual(m.files[paths[0]], b"new")
                    self.assertEqual([m.files[p] for p in paths[1:]], [b"old", b"old"])
                    self.assertEqual(m.registry, {"old": "state"})
                    m.faults.clear()
                    m.attributes.clear()
                    m.execute("RestoreShellState")
                    self.assertEqual(m.files[paths[0]], b"old")
                    self.assertEqual(m.variables["$ShellSaved"], "0")

    def test_rollback_rename_failure_keeps_old_backup(self):
        for blocked in (r"C:\Programs\Neo", r"C:\Programs\backup"):
            m = NsModel([("rename", blocked)])
            m.variables.update({"$Published": "1", "$OldMoved": "1"})
            m.files[r"C:\Programs\Neo\neo.exe"] = b"new"
            m.files[r"C:\Programs\backup\runtime\custom.txt"] = b"old"
            m.execute("InstallAbort")
            self.assertEqual(m.files[r"C:\Programs\backup\runtime\custom.txt"], b"old")
            self.assertTrue(m.messages)

    def test_mutex_fail_closed_and_shared_by_installer_uninstaller(self):
        self.assertIn('w "Global\\NeoInstallerTransaction"', SOURCE)
        self.assertIn('!insertmacro InitTransaction "un."', SOURCE)
        self.assertIn("Call un.AcquireLock", SOURCE)
        for fault, rejected in [(None, False), ("mutex-exists", True), ("mutex-denied", True)]:
            m = NsModel([fault])
            m.execute("AcquireLock")
            self.assertEqual(m.aborted, rejected)

    def test_path_gate_windows_attributes(self):
        appdata = r"C:\Users\test\AppData\Roaming"
        for target, fault, allowed in [("C:\\", None, False), (r"C:\Users", None, False), (appdata, None, False),
                                       (appdata + r"\Neo\inside", None, False), (r"C:\Programs\Neo", None, True),
                                       (r"C:\Programs\Neo", (0x400, 0), False), (r"C:\Programs\Neo", (-1, 5), False),
                                       (r"C:\Programs\Neo", (-1, 3), True), ("\\\\server\\share\\", None, False)]:
            with self.subTest(target=target, fault=fault):
                m = NsModel()
                m.variables["$INSTDIR"] = target
                if fault:
                    m.attributes[r"C:\Programs"] = fault
                m.execute("CheckInstallPath")
                self.assertEqual(not m.error, allowed)

    @unittest.skipUnless(os.name == "nt", "PowerShell requires Windows")
    def test_actual_process_gate_with_mock_processes(self):
        command = re.search(r'-Command "(try .*?)"\'', SOURCE).group(1)
        command = command.replace("$$", "$" ).replace("$\\'", "'")
        for mock, expected in [
            ("function Get-Process { [pscustomobject]@{ProcessName='not-neo'} }", 0),
            ("function Get-Process { [pscustomobject]@{ProcessName='neo'} }", 10),
            ("function Get-Process { throw 'enumeration failed' }", 20),
        ]:
            encoded = base64.b64encode((mock + "; " + command).encode("utf-16le")).decode("ascii")
            result = subprocess.run(["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded], capture_output=True, timeout=30)
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)

    def test_commit_and_uninstall_keep_unknown_data(self):
        commit = SOURCE.split('; 提交后', 1)[1].split('Goto install_end', 1)[0]
        self.assertNotRegex(commit, r'(?m)^\s*(Delete|RMDir)\b')
        self.assertIn('StrCpy $StageDir ""', commit)
        body = SOURCE.split('Section "Uninstall"', 1)[1]
        self.assertLess(body.index("Call un.CheckRunning"), body.index("Delete "))
        self.assertLess(body.index("Call un.CheckInstallPath"), body.index("Delete "))
        self.assertNotIn("RMDir /r", body)
        self.assertLess(body.index("DeleteRegKey"), body.index('Delete "$INSTDIR\\uninstall.exe"'))

    def test_isolated_nsis_compile(self):
        compiler = shutil.which("makensis")
        if not compiler:
            candidate = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "NSIS/makensis.exe"
            if candidate.is_file():
                compiler = str(candidate)
        if not compiler:
            self.skipTest("NSIS compiler unavailable")
        script = Path(__file__).with_name("installer.nsi").resolve()
        with tempfile.TemporaryDirectory() as td:
            payload = Path(td) / "dist/neo"
            payload.mkdir(parents=True)
            (payload / "neo.exe").write_bytes(b"inert test payload; never execute")
            result = subprocess.run(
                [compiler, "/NOCD", "/INPUTCHARSET", "UTF8", "/DVERSION=1.2.3", "/DVI_VERSION=1.2.3.0", str(script)],
                cwd=td, capture_output=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, (result.stdout + result.stderr).decode("utf-8", errors="replace"))
            self.assertTrue((Path(td) / "dist/neo-1.2.3-installer-x64.exe").is_file())


if __name__ == "__main__":
    unittest.main()
