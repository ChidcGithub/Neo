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
from unittest.mock import patch

from tools.check_release import CRT_NAMES, CRT_MIN_VERSION, CRT_DOWNLOAD_URL, REQUIRED_FILES


SOURCE = Path(__file__).with_name("installer.nsi").read_text(encoding="utf-8")


class NsModel:
    """Small interpreter of the actual script, with Windows APIs replaced by memory state."""

    def __init__(self, faults=()):
        self.variables = {"$INSTDIR": r"C:\Programs\Neo", "$APPDATA": r"C:\Users\test\AppData\Roaming",
                          "$TEMP": r"C:\Temp", "$StageDir": r"C:\Programs\stage",
                          "$BackupDir": r"C:\Programs\backup", "$PLUGINSDIR": r"C:\Temp\plugins",
                          "$DESKTOP": r"C:\Users\test\Desktop", "$SMPROGRAMS": r"C:\Users\test\Start"}
        self.files = {}
        self.directories = set()
        self.directory_ids = {}
        self.next_directory_id = 1
        self.residues = {}
        self.payload = {"neo.exe": b"new executable"}
        self.identity_path = None
        self.registry = None
        self.exported = None
        self.faults = set(faults)
        self.error = False
        self.stack = []
        self.calls = []
        self.messages = []
        self.aborted = False
        self.attributes = {}
        self.native_machine = 0x8664
        self.os_version = (10, 0, 19041)
        self.process_result = "0"
        self.reg_view = "32"
        self.last_reg_view = "32"
        self.crt_registry = {"Installed": 1, "Version": "v14.51.36247.0"}
        self.crt_reads = []

    def value(self, token):
        token = token.strip('"')
        return re.sub(r'\$(?:[A-Za-z][A-Za-z0-9]*|[0-9])', lambda m: str(self.variables.get(m[0], "")), token)

    def lines(self, name):
        marker = f'Section "{name}"\n'
        if marker in SOURCE:
            body = SOURCE.split(marker, 1)[1].split("SectionEnd", 1)[0]
        else:
            marker = f"Function {name}\n"
            if marker not in SOURCE:
                marker = f"Function ${{PREFIX}}{name.removeprefix('un.')}\n"
            body = SOURCE.split(marker, 1)[1].split("FunctionEnd", 1)[0]
        def expand(match):
            macro, path, flag, filename = shlex.split(match[0], posix=False)[1:]
            text = SOURCE.split(f"!macro {macro} PATH FLAG NAME\n", 1)[1].split("!macroend", 1)[0]
            return text.replace("${PATH}", path.strip('"')).replace("${FLAG}", flag).replace("${NAME}", filename.strip('"'))
        body = re.sub(r'!insertmacro (?:Save|Restore)Shortcut[^\n]+', expand, body)
        body = body.replace('${PREFIX}', 'un.' if name.startswith('un.') else '')
        # NSIS relative jumps count instructions, not labels or comments.
        instructions, labels = [], {}
        enabled = True
        for line in body.splitlines():
            line = line.strip()
            # Model the no-art build; the isolated compiler also checks this variant.
            if line == '!ifdef HAVE_ART':
                enabled = False
                continue
            if line in ('!else', '!endif'):
                enabled = True
                continue
            if not enabled:
                continue
            if not line or line.startswith(";"):
                continue
            if line.endswith(":"):
                labels[line[:-1]] = len(instructions)
            else:
                instructions.append(shlex.split(line, posix=False))
        return instructions, labels

    def is_directory(self, path):
        return path in self.directories or any(p.startswith(path + "\\") for p in (*self.files, *self.directories))

    def exists(self, path):
        if path.endswith(r"\*.*"):
            return self.is_directory(path[:-4])
        return path in self.files or self.is_directory(path)

    def execute(self, name):
        # Fixtures may seed files directly. Their parents survive deletion of the last file.
        for path in (*self.files, *self.directories):
            parent = ntpath.dirname(path)
            while parent and parent != path:
                self.directories.add(parent)
                path, parent = parent, ntpath.dirname(parent)
        lines, labels = self.lines(name)
        pc = 0
        for _ in range(1000):
            if pc == len(lines):
                return
            op, *args = lines[pc]
            jump = None
            v = self.value
            if op == "StrCpy":
                text = v(args[1])
                if len(args) > 3:
                    text = text[int(v(args[3])):]
                self.variables[args[0]] = text[:int(v(args[2]))] if len(args) > 2 else text
            elif op == "StrLen":
                self.variables[args[0]] = str(len(v(args[1])))
            elif op == "StrCmp":
                jump = args[2] if v(args[0]).casefold() == v(args[1]).casefold() else (args[3] if len(args) > 3 else None)
            elif op == "IntCmp":
                a, b = int(v(args[0]), 0), int(v(args[1]), 0)
                index = 2 if a == b else (3 if a < b else 4)
                jump = args[index] if len(args) > index else None
            elif op == "IntOp":
                a, b = int(v(args[1]), 0), int(v(args[3]), 0)
                assert args[2] in ("&", "+")
                self.variables[args[0]] = str(a & b if args[2] == "&" else a + b)
            elif op == "Goto":
                jump = args[0]
            elif op == "IfErrors":
                jump = args[0] if self.error else (args[1] if len(args) > 1 else None)
            elif op == "IfFileExists":
                jump = args[1] if self.exists(v(args[0])) else (args[2] if len(args) > 2 else None)
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
                if self.aborted:
                    return
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
                if "IsWow64Process2" in api:
                    self.variables["$0"] = "0" if "arch-api" in self.faults else "1"
                    self.variables["$2"] = str(self.native_machine)
                elif api.startswith("'*(i 276"):
                    self.variables["$0"] = "0" if "allocate" in self.faults else "100"
                elif "RtlGetVersion" in api:
                    self.variables["$4"] = "-1" if "version-api" in self.faults else "0"
                elif api.startswith("'*$0(i,"):
                    for register, value in zip(("$1", "$2", "$3"), self.os_version):
                        self.variables[register] = str(value)
                elif "GetFileAttributesW" in api:
                    match = re.search(r'GetFileAttributesW\(w (r0|"[^"]+")\) i \.r([01])', api)
                    assert match is not None, api
                    operand, register = match.groups()
                    path = self.variables["$0"] if operand == "r0" else v(operand)
                    dest = "$" + register
                    default = 0x10 if self.is_directory(path) else (0 if path in self.files else -1)
                    attrs, err = self.attributes.get(path, (default, 2))
                    self.variables[dest] = str(attrs)
                    self.stack.append(str(err))
                elif "CreateFileW" in api:
                    self.identity_path = v("$INSTDIR")
                    self.variables["$0"] = "-1" if "identity-open" in self.faults or not self.exists(self.identity_path) else "200"
                elif "GetFileInformationByHandle" in api:
                    self.variables["$2"] = "0" if "identity-query" in self.faults else "1"
                elif api.startswith("'*$1(i .r2"):
                    path = self.identity_path
                    attrs = self.attributes.get(path, (0x10 if self.is_directory(path) else 0, 0))[0]
                    if path not in self.directory_ids:
                        self.directory_ids[path] = (101, 0, self.next_directory_id, self.next_directory_id, 456)
                        self.next_directory_id += 1
                    identity = self.directory_ids[path]
                    self.variables["$2"] = str(attrs)
                    for register, value in zip(("$3", "$4", "$5", "$6", "$7"), identity):
                        self.variables[register] = str(value)
                elif "CloseHandle" in api:
                    self.calls.append("close-handle")
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
            elif op == "System::Alloc":
                self.stack.append("0" if "identity-allocate" in self.faults else "300")
            elif op == "System::Free":
                self.calls.append("free")
            elif op == "SetErrorLevel":
                self.variables["exit_code"] = v(args[0])
            elif op == "nsExec::ExecToStack" and "powershell.exe" in " ".join(args):
                self.stack.extend(["output", self.process_result])
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
                if ("delete", path) in self.faults or self.is_directory(path):
                    self.error = True
                else:
                    self.files.pop(path, None)
            elif op in ("WriteRegStr", "WriteRegDWORD"):
                key, field, data = v(args[1]), v(args[2]), v(args[3])
                if key == v("$ResidueKey") and key:
                    if "residue-write" in self.faults:
                        self.error = True
                    else:
                        self.residues[key] = data
                else:
                    if self.registry is None:
                        self.registry = {}
                    self.registry[field] = data
            elif op == "SetRegView":
                view = self.last_reg_view if args[0] == "lastused" else args[0]
                self.last_reg_view, self.reg_view = self.reg_view, view
            elif op in ("ReadRegStr", "ReadRegDWORD") and args[1] == "HKLM":
                key, field = v(args[2]), v(args[3])
                assert key == r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64"
                self.crt_reads.append((self.reg_view, field))
                data = self.crt_registry.get(field) if self.reg_view == "64" else None
                expected_type = int if op == "ReadRegDWORD" else str
                self.error = type(data) is not expected_type or "crt-reg-denied" in self.faults
                self.variables[args[0]] = "" if self.error else str(data)
            elif op == "${VersionCompare}":
                a, b = (tuple(map(int, v(arg).split("."))) for arg in args[:2])
                self.variables[args[2]] = "0" if a == b else ("1" if a > b else "2")
            elif op == "ReadRegStr":
                key = v(args[2])
                self.error = key not in self.residues or "residue-read" in self.faults
                self.variables[args[0]] = "" if self.error else self.residues[key]
            elif op == "DeleteRegKey" and v(args[1]) == v("$ResidueKey") and v("$ResidueKey"):
                self.residues.pop(v(args[1]), None)
            elif op == "DeleteRegKey":
                self.calls.append("reg-delete")
                if "reg-delete" in self.faults:
                    self.error = True
                else:
                    self.registry = None
            elif op == "Rename":
                src, dst = v(args[0]), v(args[1])
                assert self.variables.get("$OUTDIR") != src
                if ("rename", src) in self.faults or not self.exists(src) or self.exists(dst):
                    self.error = True
                else:
                    moved_ids = {p: identity for p, identity in self.directory_ids.items()
                                 if p == src or p.startswith(src + "\\")}
                    for path, identity in moved_ids.items():
                        self.directory_ids[dst + path[len(src):]] = identity
                        del self.directory_ids[path]
                    moved_dirs = {p for p in self.directories if p == src or p.startswith(src + "\\")}
                    self.directories.difference_update(moved_dirs)
                    self.directories.update(dst + p[len(src):] for p in moved_dirs)
                    moved = {p: b for p, b in self.files.items() if p == src or p.startswith(src + "\\")}
                    for path, data in moved.items():
                        self.files[dst + path[len(src):]] = data
                        del self.files[path]
            elif op == "CreateDirectory":
                path = v(args[0])
                if path in self.files:
                    self.error = True
                else:
                    self.directories.add(path)
            elif op == "GetTempFileName":
                base = v(args[1]) + r"\stage"
                path, suffix = base, 0
                while self.exists(path):
                    suffix += 1
                    path = f"{base}-{suffix}"
                self.variables[args[0]] = path
                self.files[path] = b""
            elif op == "File":
                for path, data in self.payload.items():
                    self.files[v("$OUTDIR") + "\\" + path] = data
            elif op == "WriteUninstaller":
                self.files[v(args[0])] = b"new uninstaller"
            elif op == "CreateShortcut":
                self.files[v(args[0])] = v(args[1]).encode()
            elif op == "${GetSize}":
                for register in args[2:]:
                    self.variables[register] = "1"
            elif op in ("SetOverwrite", "DetailPrint"):
                pass
            elif op == "SetOutPath":
                self.variables["$OUTDIR"] = v(args[0])
            elif op == "RMDir":
                path = v(args[-1])
                if ("rmdir", path) in self.faults or not self.is_directory(path):
                    self.error = True
                elif "/r" in args:
                    self.files = {p: b for p, b in self.files.items() if not p.startswith(path + "\\")}
                    self.directories = {p for p in self.directories if p != path and not p.startswith(path + "\\")}
                    self.directory_ids = {p: identity for p, identity in self.directory_ids.items()
                                          if p != path and not p.startswith(path + "\\")}
                elif any(p.startswith(path + "\\") for p in (*self.files, *self.directories)):
                    self.error = True
                else:
                    self.directories.discard(path)
                    self.directory_ids.pop(path, None)
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

    def test_native_architecture_not_emulated_x64(self):
        for machine, allowed in [(0x8664, True), (0xAA64, False), (0x14C, False), (0, False)]:
            with self.subTest(machine=machine):
                m = NsModel()
                m.native_machine = machine
                m.execute("CheckPlatform")
                self.assertEqual(not m.aborted, allowed)
                if not allowed:
                    self.assertIn("原生 x64", m.messages[0])
                    self.assertEqual(m.variables["exit_code"], "2")

    def test_real_version_boundary_and_detection_failures(self):
        for version, allowed in [((6, 3, 9600), False), ((6, 3, 99999), False),
                                 ((11, 0, 1), True), ((10, 0, 18363), False),
                                 ((10, 0, 19040), False), ((10, 0, 19041), True),
                                 ((10, 0, 19045), True), ((10, 0, 22000), True)]:
            with self.subTest(version=version):
                m = NsModel()
                m.os_version = version
                m.execute("CheckPlatform")
                self.assertEqual(not m.aborted, allowed)
                self.assertIn("free", m.calls)
        for fault in ("arch-api", "version-api", "allocate"):
            m = NsModel([fault])
            m.execute("CheckPlatform")
            self.assertTrue(m.aborted)
            self.assertIn("无法可靠确认", m.messages[0])

    def test_platform_api_layout_and_calling_convention(self):
        body = SOURCE.split("Function CheckPlatform\n", 1)[1].split("FunctionEnd", 1)[0]
        self.assertIn("IsWow64Process2(p -1, *i r1 r1, *i r2 r2) i .r0", body)
        self.assertIn("IntOp $2 $2 & 0xffff", body)
        self.assertIn('*(i 276, i 0, i 0, i 0, i 0, &w128 "") p .r0', body)
        self.assertIn("RtlGetVersion(p r0) i .r4", body)
        self.assertIn("*$0(i, i .r1, i .r2, i .r3)", body)
        self.assertNotRegex(body, r"\?\s*c")
        self.assertLess(body.index("System::Free $0"), body.index("StrCmp $4 0"))

    def test_platform_gate_is_install_only_and_before_payload(self):
        install_init = SOURCE.split("Function .onInit\n", 1)[1].split("FunctionEnd", 1)[0]
        uninstall_init = SOURCE.split("Function un.onInit\n", 1)[1].split("FunctionEnd", 1)[0]
        self.assertIn("Call CheckPlatform", install_init)
        self.assertIn("Call AcquireLock", install_init)
        self.assertNotIn("CheckPlatform", uninstall_init)
        self.assertIn("Call un.AcquireLock", uninstall_init)
        self.assertLess(SOURCE.index("Call CheckPlatform"), SOURCE.index('File /r "${PACKAGE_DIR}'))

    def test_crt_registry_version_boundaries_and_invalid_values(self):
        cases = [("v14.44.35211.0", False), ("v14.51.36231.0", False),
                 ("v14.51.36246.0", False), ("v14.51.36247.0", True),
                 ("14.51.36247.0", True), ("v14.51.36247.1", True),
                 ("v14.52.1.0", True), ("v15.0.0.0", True),
                 ("", False), ("v", False), ("v14.51.36247", False),
                 ("v14.51..0", False), ("v14.51.36247.0.1", False),
                 ("v14.51.36247.0x", False), ("v14.51.-36247.0", False),
                 ("v999999999999.0.0.0", False), (123, False)]
        for version, allowed in cases:
            with self.subTest(version=version):
                m = NsModel()
                m.crt_registry["Version"] = version
                m.execute("CheckVCRuntime")
                self.assertEqual(not m.aborted, allowed)
                self.assertEqual(m.reg_view, "32")
                self.assertEqual(m.crt_reads, [("64", "Installed"), ("64", "Version")])
                self.assertFalse(m.files)
                self.assertFalse(m.stack)
                if not allowed:
                    self.assertEqual(m.variables["exit_code"], "2")
                    self.assertIn(CRT_DOWNLOAD_URL, m.messages[0])

    def test_crt_failure_aborts_before_any_install_mutation(self):
        for registry, fault in [({}, None), ({"Installed": 0}, None),
                                ({"Installed": "1"}, None), ({"Installed": 1}, None),
                                ({"Installed": 1, "Version": "v14.44.1.0"}, None),
                                ({"Installed": 1, "Version": "v14.51.36247.0"}, "crt-reg-denied")]:
            with self.subTest(registry=registry, fault=fault):
                m = NsModel([fault])
                m.crt_registry = registry
                m.files = {r"C:\Programs\Neo\neo.exe": b"old",
                           r"C:\Programs\backup\neo.exe": b"backup",
                           r"C:\Users\test\AppData\Roaming\Neo\credentials": b"untouched"}
                m.registry = {"DisplayName": "old Neo"}
                before = dict(m.files)
                m.execute("Install")
                self.assertTrue(m.aborted)
                self.assertEqual(m.files, before)
                self.assertEqual(m.registry, {"DisplayName": "old Neo"})
                self.assertFalse(m.calls)
                self.assertEqual(m.reg_view, "32")

    def test_crt_preflight_scope_order_and_shared_requirement(self):
        body = SOURCE.split("Function CheckVCRuntime\n", 1)[1].split("FunctionEnd", 1)[0]
        self.assertIn(f'"{CRT_MIN_VERSION}"', body)
        self.assertIn(CRT_DOWNLOAD_URL, body)
        self.assertIn("未验证 DLL 完整性或运行兼容性", body)
        self.assertNotIn("System::Call", body)
        self.assertNotIn("Exec", body)
        self.assertIn("RequestExecutionLevel user", SOURCE)
        install = SOURCE.split('Section "Install"\n', 1)[1].split("SectionEnd", 1)[0]
        self.assertLess(install.index("Call CheckVCRuntime"), install.index("Call CheckRunning"))
        self.assertLess(install.index("Call CheckVCRuntime"), install.index("Rename"))
        self.assertLess(install.index("Call CheckVCRuntime"), install.index("File /r"))
        self.assertEqual(SOURCE.count("Call CheckVCRuntime"), 1)

    def test_process_gate_distinguishes_running_failure_timeout_and_start_error(self):
        for result, message in [("0", None), ("10", "正在运行"), ("20", "无法枚举进程"),
                                ("timeout", "超时"), ("error", "无法启动进程检查"),
                                ("1", "返回异常")]:
            with self.subTest(result=result):
                m = NsModel()
                m.process_result = result
                m.execute("CheckRunning")
                self.assertEqual(m.error, result != "0")
                self.assertEqual(m.stack, [])
                if message:
                    self.assertIn(message, m.messages[0])
                else:
                    self.assertFalse(m.messages)

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
        self.assertLess(body.index("Call un.CheckInstallTarget"), body.index("Delete "))
        self.assertNotIn("RMDir /r", body)
        self.assertLess(body.index("DeleteRegKey"), body.index('Delete "$INSTDIR\\uninstall.exe"'))

    def test_uninstall_cleans_only_empty_new_and_legacy_resource_directories(self):
        paths = [r"assets", r"assets-stt\sense-voice", r"assets-stt\vad", r"assets-stt",
                 r"resources\models\wake", r"resources\models\stt\sense-voice",
                 r"resources\models\stt\vad", r"resources\models\stt", r"resources\models",
                 r"resources\lang", r"resources", r"docs\licenses\assets", r"docs\licenses\runtime",
                 r"docs\licenses", r"docs", r"runtime\onnx", r"runtime\gitbash", r"runtime"]
        for populated in (False, True):
            with self.subTest(populated=populated):
                m = NsModel()
                root = m.value("$INSTDIR")
                m.files.update({root + r"\neo.exe": b"app", root + r"\uninstall.exe": b"uninstaller",
                                root + r"\README.md": b"legacy readme", root + r"\LICENSE": b"license",
                                root + r"\NOTICE": b"notice"})
                m.directories.update(root + "\\" + path for path in paths)
                retained = {root + "\\" + path + r"\custom.txt": b"custom" for path in paths} if populated else {}
                m.files.update(retained)
                if populated:
                    m.files[root + r"\docs\README.md"] = b"distribution guide"
                    retained[root + r"\docs\README.md"] = b"distribution guide"
                    for name in ("README.md", "cargo-inventory.json", "cargo-inventory.md", "cargo-notices.txt",
                                 "assets-audit.md", "runtime-audit.md", "assets/font/LICENSE", "runtime/onnx/NOTICE"):
                        path = root + "\\docs\\licenses\\" + name.replace("/", "\\")
                        m.files[path] = retained[path] = b"legal evidence"
                    for name in ("zh-CN.lang", "en-US.lang", "custom.lang"):
                        path = root + "\\resources\\lang\\" + name
                        m.files[path] = retained[path] = b'{"key": "user translation"}'
                m.execute("Uninstall")
                self.assertFalse(m.aborted, m.messages)
                self.assertEqual(m.files, retained)
                for path in paths:
                    self.assertEqual(m.is_directory(root + "\\" + path), populated, path)
                self.assertEqual(bool(m.residues), populated)

    def test_legacy_upgrade_preserves_complete_old_layout_in_backup(self):
        m = NsModel()
        root = m.value("$INSTDIR")
        old = {name: name.encode() for name in [r"neo.exe", r"uninstall.exe", r"README.md",
               r"assets\hi_neo.onnx", r"assets\onnxruntime.dll", r"assets\custom.dll",
               r"assets-stt\sense-voice\model.int8.onnx", r"assets-stt\custom.txt",
               r"runtime\gitbash\usr\bin\bash.exe", "vcruntime140.dll"]}
        m.files.update({root + "\\" + name: data for name, data in old.items()})
        m.payload.update({r"resources\models\wake\hi_neo.onnx": b"new model",
                          r"resources\models\stt\sense-voice\model.int8.onnx": b"new stt",
                          r"runtime\onnx\onnxruntime.dll": b"new dll", r"docs\README.md": b"new guide",
                          r"resources\lang\zh-CN.lang": b'{"key": "zh"}',
                          r"resources\lang\en-US.lang": b'{"key": "en"}'})
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)
        for name, data in old.items():
            self.assertEqual(m.files[m.value("$BackupDir") + "\\" + name], data)
        for name, data in m.payload.items():
            self.assertEqual(m.files[root + "\\" + name], data)
        self.assertNotIn(root + r"\assets\onnxruntime.dll", m.files)
        self.assertNotIn(root + r"\README.md", m.files)

    def test_crt_whitelist_is_installed_and_uninstalled_without_unknown_files(self):
        self.assertIn('File /r "${PACKAGE_DIR}\\*.*"', SOURCE)
        self.assertIn('!define PACKAGE_DIR "dist\\neo"', SOURCE)
        body = SOURCE.split('Section "Uninstall"', 1)[1]
        deleted_dlls = re.findall(r'Delete "\$INSTDIR\\([^"\\]+\.dll)"', body)
        self.assertEqual(set(deleted_dlls), set(CRT_NAMES))
        m = NsModel()
        known = ["neo.exe", "uninstall.exe", *CRT_NAMES]
        unknown = ["custom.dll", "msvcp140_custom.dll", "assets\\custom.dll", "runtime\\custom.txt"]
        for name in known + unknown:
            m.files[m.value("$INSTDIR") + "\\" + name] = name.encode()
        m.registry = {"DisplayVersion": "old"}
        m.execute("Uninstall")
        self.assertFalse(m.aborted)
        self.assertIsNone(m.registry)
        self.assertEqual(m.files, {m.value("$INSTDIR") + "\\" + name: name.encode() for name in unknown})

    def test_crt_delete_failure_preserves_uninstaller_and_registration(self):
        for name in CRT_NAMES:
            with self.subTest(name=name):
                m = NsModel()
                path = m.value("$INSTDIR") + "\\" + name
                uninstaller = m.value(r"$INSTDIR\uninstall.exe")
                m.files.update({path: b"locked CRT", uninstaller: b"uninstaller",
                                m.value(r"$INSTDIR\neo.exe"): b"old executable"})
                m.registry = {"DisplayVersion": "old"}
                m.faults.add(("delete", path))
                m.execute("Uninstall")
                self.assertTrue(m.aborted)
                self.assertEqual(m.files[path], b"locked CRT")
                self.assertIn(uninstaller, m.files)
                self.assertEqual(m.registry, {"DisplayVersion": "old"})

    def installed_model(self):
        m = NsModel()
        for name in ["neo.exe", "uninstall.exe", "assets\\hi_neo.onnx",
                     "assets-stt\\model.int8.onnx", "runtime\\gitbash\\bin\\bash.exe",
                     "assets\\custom.txt", "assets-stt\\custom.txt", "runtime\\custom.txt", "notes.txt",
                     "resources\\models\\wake\\hi_neo.onnx", "resources\\models\\wake\\custom.onnx",
                     "resources\\models\\stt\\sense-voice\\model.int8.onnx", "resources\\custom.txt",
                     "resources\\lang\\zh-CN.lang", "resources\\lang\\en-US.lang", "resources\\lang\\custom.lang",
                     "runtime\\onnx\\onnxruntime.dll", "runtime\\onnx\\custom.dll",
                     "docs\\README.md", "docs\\custom.md", "NOTICE", "docs\\licenses\\README.md",
                     "docs\\licenses\\assets\\custom.txt", "docs\\licenses\\runtime\\custom.txt"]:
            m.files[m.value("$INSTDIR") + "\\" + name] = name.encode()
        m.registry = {"DisplayVersion": "old"}
        return m

    def test_uninstall_then_reinstall_keeps_residue_and_user_data(self):
        m = self.installed_model()
        appdata = m.value(r"$APPDATA\Neo\memory.db")
        m.files[appdata] = b"private data"
        m.execute("Uninstall")
        self.assertFalse(m.aborted)
        self.assertIsNone(m.registry)
        self.assertNotIn(m.value(r"$INSTDIR\neo.exe"), m.files)
        self.assertNotIn(m.value(r"$INSTDIR\uninstall.exe"), m.files)
        self.assertTrue(m.residues)
        remaining = dict(m.files)
        m.execute("CheckInstallTarget")
        self.assertFalse(m.error)
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)
        self.assertEqual(m.files[m.value(r"$INSTDIR\neo.exe")], b"new executable")
        self.assertIn(m.value(r"$INSTDIR\uninstall.exe"), m.files)
        for path, data in remaining.items():
            preserved = path.replace(m.value("$INSTDIR"), m.value("$BackupDir"), 1) if path != appdata else path
            self.assertEqual(m.files[preserved], data)
        self.assertEqual(m.files[appdata], b"private data")
        # A second uninstall records the new directory, not the moved backup.
        m.execute("Uninstall")
        self.assertFalse(m.aborted, m.messages)
        m.execute("CheckInstallTarget")
        self.assertFalse(m.error)

    def test_reinstall_publish_failure_restores_residue_for_retry(self):
        m = self.installed_model()
        m.execute("Uninstall")
        before = dict(m.files)
        m.faults.add(("rename", m.value("$StageDir")))
        m.execute("Install")
        self.assertTrue(m.aborted)
        self.assertEqual(m.files, before)
        m.execute("CheckInstallTarget")
        self.assertFalse(m.error)
        m.aborted = False
        m.faults.clear()
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)

    def test_unknown_legacy_and_forged_residue_remain_rejected(self):
        for names in [["notes.txt"], ["assets\\hi_neo.onnx", "assets-stt\\model.int8.onnx",
                                    "runtime\\gitbash\\bin\\bash.exe"],
                      [".neo-uninstalled", "assets\\custom.txt"], ["neo.exe"], ["uninstall.exe"]]:
            with self.subTest(names=names):
                m = NsModel()
                m.files = {m.value("$INSTDIR") + "\\" + name: b"keep" for name in names}
                before = dict(m.files)
                m.execute("Install")
                self.assertTrue(m.aborted)
                self.assertEqual(m.files, before)
                self.assertTrue(any("旧版卸载" in message for message in m.messages))
                m.aborted = False
                m.execute("Uninstall")
                self.assertTrue(m.aborted)
                self.assertFalse(m.residues, "a copied uninstaller must not bless an unknown directory")
                self.assertEqual(m.files, before)

    def test_entry_files_reject_directories_reparse_points_and_attribute_errors(self):
        for name in ("neo.exe", "uninstall.exe"):
            for kind in ("directory", "reparse", "denied", "io-error"):
                for recorded in (False, True):
                    with self.subTest(name=name, kind=kind, recorded=recorded):
                        m = self.installed_model()
                        if recorded:
                            m.execute("Uninstall")
                            m.files[m.value(r"$INSTDIR\neo.exe")] = b"app"
                            m.files[m.value(r"$INSTDIR\uninstall.exe")] = b"uninstaller"
                        path = m.value("$INSTDIR") + "\\" + name
                        if kind == "directory":
                            del m.files[path]
                            m.directories.add(path)
                        else:
                            m.attributes[path] = {"reparse": (0x400, 0), "denied": (-1, 5),
                                                  "io-error": (-1, 1117)}[kind]
                        before, residues = dict(m.files), dict(m.residues)
                        m.execute("CheckInstallTarget")
                        self.assertTrue(m.error)
                        m.execute("Uninstall")
                        self.assertTrue(m.aborted)
                        self.assertEqual(m.files, before)
                        self.assertEqual(m.residues, residues)
                        self.assertEqual(m.stack, [])

    def model_instructions(self, model, text):
        instructions = [shlex.split(line, posix=False) for line in text.splitlines() if line.strip()]
        with patch.object(model, "lines", return_value=(instructions, {})):
            model.execute("test instructions")

    def test_model_rename_requires_source_and_absent_destination(self):
        for source_exists, destination_kind in ((False, None), (True, "file"), (True, "directory")):
            with self.subTest(source_exists=source_exists, destination_kind=destination_kind):
                m = NsModel()
                if source_exists:
                    m.files[m.value(r"$INSTDIR\notes.txt")] = b"source"
                if destination_kind == "file":
                    m.files[m.value("$BackupDir")] = b"destination"
                elif destination_kind == "directory":
                    m.directories.add(m.value("$BackupDir"))
                before = dict(m.files)
                self.model_instructions(m, 'Rename "$INSTDIR" "$BackupDir"')
                self.assertTrue(m.error)
                self.assertEqual(m.files, before)

    def test_consecutive_reinstalls_preserve_distinct_backups(self):
        m = self.installed_model()
        m.execute("Uninstall")
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)
        first_backup = m.value("$BackupDir")
        retained = {p: data for p, data in m.files.items() if p.startswith(first_backup + "\\")}
        self.assertTrue(retained)
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)
        self.assertNotEqual(m.value("$BackupDir"), first_backup)
        for path, data in retained.items():
            self.assertEqual(m.files[path], data)
        self.assertNotIn(m.value(r"$INSTDIR\notes.txt"), m.files)
        self.assertEqual(m.files[m.value(r"$INSTDIR\neo.exe")], b"new executable")

    def test_temp_name_skips_existing_file_and_empty_directory(self):
        m = NsModel()
        m.files[r"C:\Programs\stage"] = b"keep"
        m.directories.add(r"C:\Programs\stage-1")
        self.model_instructions(m, 'GetTempFileName $StageDir "C:\\Programs"')
        self.assertEqual(m.value("$StageDir"), r"C:\Programs\stage-2")
        self.assertEqual(m.files[r"C:\Programs\stage"], b"keep")
        self.assertEqual(m.files[m.value("$StageDir")], b"")

    def test_rollback_destination_collision_keeps_backup_and_can_retry(self):
        m = self.installed_model()
        m.execute("Uninstall")
        identity = m.directory_ids[m.value("$INSTDIR")]
        self.model_instructions(m, 'Rename "$INSTDIR" "$BackupDir"\nCreateDirectory "$INSTDIR"')
        m.variables["$OldMoved"] = "1"
        before = dict(m.files)
        m.execute("RollbackInstall")
        self.assertEqual(m.files, before)
        self.assertEqual(m.variables["$OldMoved"], "1")
        self.assertTrue(m.messages)
        self.model_instructions(m, 'RMDir "$INSTDIR"')
        m.execute("RollbackInstall")
        self.assertEqual(m.variables["$OldMoved"], "0")
        self.assertEqual(m.directory_ids[m.value("$INSTDIR")], identity)
        m.execute("CheckInstallTarget")
        self.assertFalse(m.error)

    def test_deleted_and_recreated_directory_cannot_reuse_residue_identity(self):
        for recursive in (False, True):
            with self.subTest(recursive=recursive):
                m = self.installed_model()
                m.execute("Uninstall")
                root = m.value("$INSTDIR")
                original = m.directory_ids[root]
                m.variables["$INSTDIR"] = root + r"\assets"
                m.execute("GetResidueKey")
                child_identity = m.directory_ids[m.value("$INSTDIR")]
                m.variables["$INSTDIR"] = root
                if recursive:
                    self.model_instructions(m, 'RMDir /r "$INSTDIR"')
                else:
                    self.model_instructions(m, 'RMDir /r "$INSTDIR\\assets"\n'
                                           'RMDir /r "$INSTDIR\\assets-stt"\n'
                                           'RMDir /r "$INSTDIR\\runtime"\n'
                                           'RMDir /r "$INSTDIR\\resources"\n'
                                           'RMDir /r "$INSTDIR\\docs"\n'
                                           'Delete "$INSTDIR\\notes.txt"\nRMDir "$INSTDIR"')
                self.assertFalse(m.error)
                self.assertNotIn(root, m.directory_ids)
                self.assertNotIn(root + r"\assets", m.directory_ids)
                self.assertTrue(m.residues, "external deletion leaves the old registry record")
                self.model_instructions(m, 'CreateDirectory "$INSTDIR"\nCreateDirectory "$INSTDIR\\assets"')
                m.files[root + r"\unrelated.txt"] = b"unknown directory"
                m.execute("CheckInstallTarget")
                self.assertTrue(m.error)
                self.assertNotEqual(m.directory_ids[root], original)
                m.variables["$INSTDIR"] = root + r"\assets"
                m.execute("GetResidueKey")
                self.assertNotEqual(m.directory_ids[m.value("$INSTDIR")], child_identity)

    def test_residue_requires_same_path_volume_file_id_and_creation_time(self):
        for mismatch in ("path", "volume", "file-id", "creation-time", "missing-record", "read-denied"):
            with self.subTest(mismatch=mismatch):
                m = self.installed_model()
                m.execute("Uninstall")
                path = m.value("$INSTDIR")
                if mismatch == "path":
                    m.residues = {key: r"C:\Other\Neo" for key in m.residues}
                elif mismatch == "missing-record":
                    m.residues.clear()
                elif mismatch == "read-denied":
                    m.faults.add("residue-read")
                else:
                    identity = list(m.directory_ids[path])
                    identity[{"volume": 0, "file-id": 2, "creation-time": 3}[mismatch]] += 1
                    m.directory_ids[path] = tuple(identity)
                before = dict(m.files)
                m.execute("CheckInstallTarget")
                self.assertTrue(m.error)
                self.assertEqual(m.files, before)

    def test_identity_or_record_failure_precedes_uninstall_deletion(self):
        for fault in ("identity-open", "identity-query", "identity-allocate", "residue-write", "residue-read"):
            with self.subTest(fault=fault):
                m = self.installed_model()
                m.faults.add(fault)
                before = dict(m.files)
                m.execute("Uninstall")
                self.assertTrue(m.aborted)
                self.assertEqual(m.files, before)
                self.assertEqual(m.registry, {"DisplayVersion": "old"})

    def test_identity_rejects_zero_file_id_and_nondirectory_handle(self):
        for attrs, identity in [(0x10, (101, 0, 0, 123, 456)),
                                (0, (101, 0, 7, 123, 456)),
                                (0x410, (101, 0, 7, 123, 456))]:
            m = self.installed_model()
            path = m.value("$INSTDIR")
            m.attributes[path] = (attrs, 0)
            m.directory_ids[path] = identity
            m.execute("GetResidueKey")
            self.assertTrue(m.error)
            self.assertEqual(m.variables["$ResidueKey"], "")
            self.assertEqual(m.stack, [])
            self.assertIn("close-handle", m.calls)

    def test_residue_does_not_bypass_reparse_or_access_checks(self):
        for attrs in ((0x410, 0), (-1, 5)):
            m = self.installed_model()
            m.execute("Uninstall")
            m.attributes[m.value("$INSTDIR")] = attrs
            m.execute("CheckInstallTarget")
            self.assertTrue(m.error)

    def test_registry_delete_failure_keeps_reinstall_and_uninstall_retry(self):
        m = self.installed_model()
        m.faults.add("reg-delete")
        m.execute("Uninstall")
        self.assertTrue(m.aborted)
        self.assertIn(m.value(r"$INSTDIR\uninstall.exe"), m.files)
        self.assertEqual(m.registry, {"DisplayVersion": "old"})
        m.execute("CheckInstallTarget")
        self.assertFalse(m.error)
        m.aborted = False
        m.faults.clear()
        m.execute("Uninstall")
        self.assertFalse(m.aborted, m.messages)
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)

    def test_partial_uninstall_can_retry_and_reinstall(self):
        for path in (r"$INSTDIR\neo.exe", r"$INSTDIR\msvcp140.dll", r"$INSTDIR\uninstall.exe"):
            m = self.installed_model()
            m.files[m.value(r"$INSTDIR\msvcp140.dll")] = b"CRT"
            m.faults.add(("delete", m.value(path)))
            m.execute("Uninstall")
            self.assertTrue(m.aborted)
            m.execute("CheckInstallTarget")
            self.assertFalse(m.error)
            m.aborted = False
            m.faults.clear()
            m.execute("Uninstall")
            self.assertFalse(m.aborted, m.messages)
            m.execute("Install")
            self.assertFalse(m.aborted, m.messages)

    def test_empty_uninstall_removes_identity_record(self):
        m = NsModel()
        m.files = {m.value(r"$INSTDIR\neo.exe"): b"app",
                   m.value(r"$INSTDIR\uninstall.exe"): b"uninstaller"}
        m.execute("Uninstall")
        self.assertFalse(m.aborted)
        self.assertFalse(m.files)
        self.assertFalse(m.residues)
        m.execute("Install")
        self.assertFalse(m.aborted, m.messages)

    def test_residue_api_layout_and_validation_order(self):
        body = SOURCE.split('Function ${PREFIX}GetResidueKey\n', 1)[1].split('FunctionEnd', 1)[0]
        self.assertIn('System::Alloc 52', body)
        self.assertIn('i 0x02200000', body)  # BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        self.assertIn('i .r2, i .r6, i .r7, i, i, i, i, i .r3, i, i, i, i .r4, i .r5', body)
        install = SOURCE.split('Section "Install"\n', 1)[1].split('SectionEnd', 1)[0]
        self.assertEqual(install.count('Call CheckInstallTarget'), 2)
        uninstall = SOURCE.split('Section "Uninstall"\n', 1)[1]
        self.assertLess(uninstall.index('WriteRegStr'), uninstall.index('Delete '))
        self.assertLess(uninstall.index('ReadRegStr'), uninstall.index('Delete '))

    def test_crt_transaction_rollback_restores_all_old_dlls_and_unknown_files(self):
        for previous in (False, True):
            for blocked in (None, r"C:\Programs\Neo", r"C:\Programs\backup"):
                with self.subTest(previous=previous, blocked=blocked):
                    m = NsModel()
                    m.variables.update({"$Published": "1", "$OldMoved": str(int(previous))})
                    old = {}
                    for name in [*CRT_NAMES, "custom.dll", "assets\\custom.txt"]:
                        m.files[m.value("$INSTDIR") + "\\" + name] = b"new"
                        if previous:
                            path = m.value("$BackupDir") + "\\" + name
                            old[path] = b"old: " + name.encode()
                    m.files.update(old)
                    if blocked:
                        m.faults.add(("rename", blocked))
                    m.execute("InstallAbort")
                    if previous:
                        for path, content in old.items():
                            restored = path if blocked else path.replace(m.value("$BackupDir"), m.value("$INSTDIR"), 1)
                            self.assertEqual(m.files[restored], content)
                    elif blocked != m.value("$INSTDIR"):
                        self.assertFalse(m.files)
                    before = dict(m.files)
                    m.execute("InstallAbort")
                    self.assertEqual(m.files, before)

    def test_release_uses_explicit_x64_output_and_formal_assets(self):
        root = Path(__file__).resolve().parent.parent
        workflow = (root / ".github/workflows/build.yml").read_text(encoding="utf-8")
        self.assertIn("cargo build --locked --release -p neo-app --target x86_64-pc-windows-msvc", workflow)
        self.assertIn(r"Copy-Item target\ci-rust\x86_64-pc-windows-msvc\release\neo.exe", workflow)
        self.assertNotIn(r"Copy-Item target\release\neo.exe", workflow)
        self.assertIn(r'Copy-Item "crates\neo-wake\assets\$model" "$pkg\resources\models\wake\"', workflow)
        self.assertIn(r'Copy-Item crates\neo-wake\assets\*.dll "$pkg\runtime\onnx\"', workflow)
        self.assertNotIn(r'Copy-Item -Recurse crates\neo-wake\assets', workflow)
        self.assertNotIn("wake-training", workflow)
        self.assertLess(workflow.index("python tools/check_release.py"), workflow.index("- name: Zip portable package"))
        wake = (root / "crates/neo-wake/src/lib.rs").read_text(encoding="utf-8")
        self.assertIn("安装资源缺失：hi_neo.onnx", wake)
        self.assertNotIn("请先跑 wake-training", wake)

    def test_apache_license_page_preserves_existing_page_order(self):
        pages = re.findall(r'^!insertmacro (MUI_PAGE_\w+)(.*)$', SOURCE, re.M)
        self.assertEqual(pages, [("MUI_PAGE_WELCOME", ""), ("MUI_PAGE_LICENSE", ' "LICENSE"'),
                                 ("MUI_PAGE_DIRECTORY", ""), ("MUI_PAGE_INSTFILES", ""),
                                 ("MUI_PAGE_FINISH", "")])
        self.assertIn('VIAddVersionKey /LANG=2052 "LegalCopyright" "Apache-2.0"', SOURCE)

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
            for name in (*REQUIRED_FILES, "docs/licenses/assets/font/LICENSE",
                         "docs/licenses/runtime/onnx/NOTICE"):
                path = payload / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"inert test payload; never execute")
            # MUI reads the repository LICENSE at compile time, not from the payload.
            license_path = script.parent.parent / "LICENSE"
            shutil.copy2(license_path, Path(td) / "LICENSE")
            shutil.copy2(license_path, payload / "LICENSE")
            for language in ("zh-CN", "en-US"):
                (payload / f"resources/lang/{language}.lang").write_bytes(b'{"key": "translation"}')
            result = subprocess.run(
                [compiler, "/NOCD", "/INPUTCHARSET", "UTF8", "/DVERSION=1.2.3", "/DVI_VERSION=1.2.3.0", str(script)],
                cwd=td, capture_output=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, (result.stdout + result.stderr).decode("utf-8", errors="replace"))
            self.assertTrue((Path(td) / "dist/neo-1.2.3-installer-x64.exe").is_file())
            for variant in ("int8", "fp32"):
                variant_payload = payload.with_name("neo-" + variant)
                shutil.copytree(payload, variant_payload)
                output = Path(td) / f"dist/neo-1.2.3-{variant}-installer-x64.exe"
                result = subprocess.run(
                    [compiler, "/NOCD", "/INPUTCHARSET", "UTF8", "/DVERSION=1.2.3",
                     "/DVI_VERSION=1.2.3.0", f"/DPACKAGE_DIR={variant_payload}",
                     f"/DOUTPUT_FILE={output}", str(script)],
                    cwd=td, capture_output=True, timeout=60,
                )
                self.assertEqual(result.returncode, 0, (result.stdout + result.stderr).decode("utf-8", errors="replace"))
                self.assertTrue(output.is_file())


if __name__ == "__main__":
    unittest.main()
