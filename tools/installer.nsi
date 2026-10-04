; Neo Windows x64 安装包（per-user 安装，免 UAC 提权）。
; 由 .github/workflows/release.yml 调用（本地手动编译同）：
;   makensis /NOCD /INPUTCHARSET UTF8 /DVERSION=1.2.3 /DVI_VERSION=1.2.3.0 tools\installer.nsi
; ⚠️ 两个开关都不能省：/NOCD 保持工作目录在仓库根（默认会切到 tools\，
; dist\ 与 target\ 的相对路径会全找不到）；/INPUTCHARSET UTF8 读本脚本的
; 中文注释（默认 ACP 直接报 Bad text encoding）。
; 输入：dist\neo\（已装配好的发行目录）+ target\package\installer-art\（美术资源，
; 由 tools/make_installer_art.py 生成；缺失时自动退化为无图标版式，
; 本地不生成美术也能编译通过）
; 输出：dist\neo-<VERSION>-installer-x64.exe

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef VI_VERSION
  ; VIProductVersion 必须是四段纯数字；版本带预发布后缀时请显式传 /DVI_VERSION
  !define VI_VERSION "${VERSION}.0"
!endif

!define ART "target\package\installer-art"
!if /FileExists "${ART}\neo.ico"
  !define HAVE_ART
!endif

Unicode true
SetCompressor /SOLID lzma
AllowSkipFiles off
ManifestDPIAware true

Name "Neo"
Caption "Neo 安装"
BrandingText "Neo · 教室大屏 AI 助手"
OutFile "dist\neo-${VERSION}-installer-x64.exe"
InstallDir "$LOCALAPPDATA\Programs\Neo"
RequestExecutionLevel user

; 安装包 exe 的版本信息（资源管理器属性页 / SmartScreen 展示用）
VIProductVersion "${VI_VERSION}"
VIAddVersionKey /LANG=2052 "ProductName" "Neo"
VIAddVersionKey /LANG=2052 "FileDescription" "Neo 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=2052 "CompanyName" "Neo"
VIAddVersionKey /LANG=2052 "LegalCopyright" "Apache-2.0"

; ---- 版式（MUI_* define 必须先于 MUI2.nsh 引入）----
!ifdef HAVE_ART
  !define MUI_ICON "${ART}\neo.ico"
  !define MUI_UNICON "${ART}\neo.ico"
  !define MUI_HEADERIMAGE
  !define MUI_HEADERIMAGE_RIGHT
  !define MUI_HEADERIMAGE_BITMAP "${ART}\header.bmp"
  !define MUI_HEADERIMAGE_UNBITMAP "${ART}\header.bmp"
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${ART}\welcome.bmp"
  !define MUI_UNWELCOMEFINISHPAGE_BITMAP "${ART}\welcome.bmp"
!endif

Var StageDir
Var BackupDir
Var OldMoved
Var Published
Var InstallMutex
Var ShellSaved
Var ShellDirty
Var RegistrySaved
Var DesktopSaved
Var StartSaved
Var UninstallSaved
Var ResidueKey

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo"
!define RESIDUE_KEY "Software\Neo\InstallerResidue"

!define MUI_CUSTOMFUNCTION_ABORT InstallAbort
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "欢迎使用 Neo 安装向导"
!define MUI_WELCOMEPAGE_TEXT "Neo 是教室大屏 AI 助手 —— 语音唤醒、课堂监测、截图答疑。$\r$\n$\r$\n本向导将把 Neo 安装到当前用户目录，无需管理员权限。$\r$\n$\r$\n单击「下一步」继续。"
!define MUI_DIRECTORYPAGE_TEXT_TOP "选择 Neo 的安装位置（仅当前用户，免管理员）。$\r$\n$\r$\n升级安装会自动覆盖旧版本程序文件；用户数据（%APPDATA%\Neo）不受影响。"
!define MUI_FINISHPAGE_TITLE "Neo 安装完成"
!define MUI_FINISHPAGE_RUN "$INSTDIR\neo.exe"
!define MUI_FINISHPAGE_RUN_TEXT "启动 Neo"
!define MUI_FINISHPAGE_LINK "访问项目主页（GitHub）"
!define MUI_FINISHPAGE_LINK_LOCATION "https://github.com/ChidcGithub/Neo"
!define MUI_FINISHPAGE_NOREBOOTSUPPORT

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "WordFunc.nsh"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"

!macro InitTransaction PREFIX
Function ${PREFIX}AcquireLock
  ; Global 覆盖同一用户的不同登录会话；拒绝访问也必须停止。
  System::Call 'kernel32::CreateMutexW(p 0, i 0, w "Global\NeoInstallerTransaction") p .r0 ?e'
  Pop $1
  StrCpy $InstallMutex $0
  StrCmp $0 0 init_failed
  StrCmp $1 183 init_failed
  Return
init_failed:
  MessageBox MB_OK|MB_ICONSTOP "另一个 Neo 安装程序正在运行，或无法获取安装锁。"
  Abort
FunctionEnd

Function ${PREFIX}CheckRunning
  ; 不依赖窗口标题；进程枚举失败也不能继续升级。
  nsExec::ExecToStack /TIMEOUT=15000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "try { if (@(Get-Process -ErrorAction Stop | Where-Object { $$_.ProcessName -eq $\'neo$\' }).Count -gt 0) { exit 10 }; exit 0 } catch { exit 20 }"'
  Pop $0
  Pop $1
  StrCmp $0 "0" running_ok
  StrCmp $0 "10" running_found
  StrCmp $0 "20" running_enumeration_failed
  StrCmp $0 "timeout" running_timeout
  MessageBox MB_OK|MB_ICONSTOP "无法启动进程检查（PowerShell 不可用或返回异常）。为保护现有安装，本次操作已停止。"
  Goto running_failed
running_found:
  MessageBox MB_OK|MB_ICONSTOP "Neo 正在运行，请完全退出 Neo 后重试。"
  Goto running_failed
running_enumeration_failed:
  MessageBox MB_OK|MB_ICONSTOP "无法枚举进程，不能确认 Neo 已退出。请检查系统权限或安全软件后重试；本次操作已停止。"
  Goto running_failed
running_timeout:
  MessageBox MB_OK|MB_ICONSTOP "进程检查超时（15 秒），不能确认 Neo 已退出。本次操作已停止，请稍后重试。"
running_failed:
  SetErrors
  Return
running_ok:
  ClearErrors
FunctionEnd

Function ${PREFIX}CheckShellPath
  Pop $0
  Push $4
  StrCpy $4 0x410
shell_path_loop:
  System::Call 'kernel32::GetFileAttributesW(w r0) i .r1 ?e'
  Pop $3
  IntCmp $1 -1 shell_path_missing
  IntOp $1 $1 & $4
  IntCmp $1 0 shell_path_parent shell_path_failed shell_path_failed
shell_path_missing:
  StrCmp $3 2 shell_path_parent
  StrCmp $3 3 shell_path_parent shell_path_failed
shell_path_parent:
  StrCpy $4 0x400
  ${GetParent} "$0" $1
  StrCmp $1 "" shell_path_ok
  StrCmp $1 $0 shell_path_ok
  StrCpy $0 $1
  Goto shell_path_loop
shell_path_failed:
  Pop $4
  SetErrors
  Return
shell_path_ok:
  Pop $4
  ClearErrors
FunctionEnd

Function ${PREFIX}CheckInstallPath
  ClearErrors
  GetFullPathName $INSTDIR "$INSTDIR"
  IfErrors path_failed
  ${GetRoot} "$INSTDIR" $0
  StrCmp $INSTDIR $0 path_failed
  StrCpy $1 "$0\"
  StrCmp $INSTDIR $1 path_failed
  ; 安装目录不能是数据目录、其子目录或祖先目录。
  StrCpy $0 "$APPDATA\Neo"
path_data_parents:
  StrCmp $INSTDIR $0 path_failed
  ${GetParent} "$0" $1
  StrCmp $1 "" path_children
  StrCmp $1 $0 path_failed
  StrCpy $0 $1
  Goto path_data_parents
path_children:
  StrCpy $0 "$APPDATA\Neo\"
  StrLen $1 $0
  StrCpy $3 $INSTDIR $1
  StrCmp $3 $0 path_failed
  StrCpy $0 $INSTDIR
path_ancestors:
  System::Call 'kernel32::GetFileAttributesW(w r0) i .r1 ?e'
  Pop $3
  IntCmp $1 -1 path_missing
  IntOp $1 $1 & 0x400
  IntCmp $1 0 path_parent path_failed path_failed
path_missing:
  ; 仅路径不存在可接受；访问拒绝等状态必须失败关闭。
  StrCmp $3 2 path_parent
  StrCmp $3 3 path_parent path_failed
path_parent:
  ${GetParent} "$0" $1
  StrCmp $1 "" path_ok
  StrCmp $1 $0 path_ok
  StrCpy $0 $1
  Goto path_ancestors
path_failed:
  SetErrors
  Return
path_ok:
  ClearErrors
FunctionEnd

Function ${PREFIX}GetResidueKey
  ; 路径本身不够：旧目录被删除/替换后，不能把同名的未知目录当作安装残留。
  ; 只查询目录身份，不跟随重解析点；调用者仍须先执行 CheckInstallPath。
  StrCpy $ResidueKey ""
  System::Call 'kernel32::CreateFileW(w "$INSTDIR", i 0, i 7, p 0, i 3, i 0x02200000, p 0) p .r0'
  StrCmp $0 -1 residue_identity_failed
  System::Alloc 52
  Pop $1
  StrCmp $1 0 residue_close_failed
  System::Call 'kernel32::GetFileInformationByHandle(p r0, p r1) i .r2'
  StrCmp $2 0 residue_free_failed
  ; BY_HANDLE_FILE_INFORMATION: attributes, 3 FILETIMEs, volume, size, links, file ID.
  System::Call '*$1(i .r2, i .r6, i .r7, i, i, i, i, i .r3, i, i, i, i .r4, i .r5)'
  IntOp $2 $2 & 0x410
  IntCmp $2 0x10 0 residue_free_failed residue_free_failed
  ; 不支持稳定文件 ID 的文件系统失败关闭；创建时间同时防止删除后的 ID 复用。
  StrCmp $4 0 0 residue_identity_ok
  StrCmp $5 0 residue_free_failed
residue_identity_ok:
  StrCpy $ResidueKey "${RESIDUE_KEY}\$3-$4-$5-$6-$7"
  System::Free $1
  System::Call 'kernel32::CloseHandle(p r0)'
  ClearErrors
  Return
residue_free_failed:
  System::Free $1
residue_close_failed:
  System::Call 'kernel32::CloseHandle(p r0)'
residue_identity_failed:
  SetErrors
FunctionEnd

Function ${PREFIX}CheckInstallTarget
  Call ${PREFIX}CheckInstallPath
  IfErrors target_failed
  IfFileExists "$INSTDIR\*.*" 0 target_ok
  ; IfFileExists 也接受同名目录；两个入口必须是普通文件，缺失才回退到残留认证。
  StrCpy $2 "0"
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\neo.exe") i .r0 ?e'
  Pop $1
  IntCmp $0 -1 target_app_missing
  IntOp $0 $0 & 0x410
  IntCmp $0 0 target_uninstaller target_failed target_failed
target_app_missing:
  StrCmp $1 2 target_app_absent
  StrCmp $1 3 0 target_failed
target_app_absent:
  StrCpy $2 "1"
target_uninstaller:
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\uninstall.exe") i .r0 ?e'
  Pop $1
  IntCmp $0 -1 target_uninstaller_missing
  IntOp $0 $0 & 0x410
  IntCmp $0 0 target_files_checked target_failed target_failed
target_uninstaller_missing:
  StrCmp $1 2 target_residue
  StrCmp $1 3 target_residue target_failed
target_files_checked:
  StrCmp $2 "0" target_ok
target_residue:
  Call ${PREFIX}GetResidueKey
  IfErrors target_failed
  ReadRegStr $0 HKCU "$ResidueKey" "Path"
  IfErrors target_failed
  StrCmp $0 $INSTDIR target_ok
target_failed:
  SetErrors
  Return
target_ok:
  ClearErrors
FunctionEnd
!macroend
!insertmacro InitTransaction ""
!insertmacro InitTransaction "un."

Function CheckPlatform
  ; IsWow64Process2 的 nativeMachine 不受 x86/x64 仿真影响；RunningX64 会误放 ARM64。
  StrCpy $0 0
  StrCpy $1 0
  StrCpy $2 0
  System::Call 'kernel32::IsWow64Process2(p -1, *i r1 r1, *i r2 r2) i .r0'
  StrCmp $0 1 0 platform_unknown
  IntOp $2 $2 & 0xffff
  IntCmp $2 0x8664 platform_version platform_arch platform_arch
platform_version:
  ; RtlGetVersion 不受兼容性 manifest / GetVersionEx 版本虚拟化影响。
  System::Call '*(i 276, i 0, i 0, i 0, i 0, &w128 "") p .r0'
  StrCmp $0 0 platform_unknown
  StrCpy $4 -1
  System::Call 'ntdll::RtlGetVersion(p r0) i .r4'
  System::Call '*$0(i, i .r1, i .r2, i .r3)'
  System::Free $0
  StrCmp $4 0 0 platform_unknown
  IntCmp $1 10 platform_build platform_old platform_supported
platform_build:
  IntCmp $3 19041 platform_supported platform_old platform_supported
platform_arch:
  MessageBox MB_OK|MB_ICONSTOP "Neo 仅支持原生 x64（AMD64）Windows，不支持 32 位 Windows 或 ARM64 仿真。安装尚未解包。"
  Goto platform_abort
platform_old:
  MessageBox MB_OK|MB_ICONSTOP "Neo 需要 Windows 10 2004（系统内部版本 19041）或更新的 Windows。请先升级系统；安装尚未解包。"
  Goto platform_abort
platform_unknown:
  MessageBox MB_OK|MB_ICONSTOP "无法可靠确认系统版本或原生架构。Neo 需要 Windows 10 2004（19041）及以上的原生 x64 系统；为安全起见已停止安装。"
platform_abort:
  SetErrorLevel 2
  Abort
platform_supported:
FunctionEnd

; Registry preflight only, not proof of DLL integrity/exports or runtime compatibility.
; 14.51.36247.0 is a conservative release floor for the observed 14.51 toolset.
; Re-evaluate for newer build tools; do not change the Windows platform floor here.
Function CheckVCRuntime
  SetRegView 64
  ClearErrors
  ReadRegDWORD $0 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Installed"
  IfErrors crt_missing
  StrCmp $0 "1" 0 crt_missing
  ReadRegStr $1 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64" "Version"
  IfErrors crt_missing
  ; Microsoft normally writes v14.xx.xxxxx.x. Reject malformed/empty values
  ; before VersionCompare (which is a comparator, not a version validator).
  StrCpy $2 $1 1
  StrCmp $2 "v" 0 crt_validate
  StrCpy $1 $1 1024 1
crt_validate:
  StrCpy $2 0
  StrCpy $4 0
  StrCpy $5 0
crt_char:
  StrCpy $3 $1 1 $2
  StrCmp $3 "" crt_end
  StrCmp $3 "." crt_dot
  StrCmp $3 "0" crt_digit
  StrCmp $3 "1" crt_digit
  StrCmp $3 "2" crt_digit
  StrCmp $3 "3" crt_digit
  StrCmp $3 "4" crt_digit
  StrCmp $3 "5" crt_digit
  StrCmp $3 "6" crt_digit
  StrCmp $3 "7" crt_digit
  StrCmp $3 "8" crt_digit
  StrCmp $3 "9" crt_digit crt_missing
crt_digit:
  IntOp $4 $4 + 1
  IntCmp $4 5 crt_next crt_next crt_missing
crt_dot:
  StrCmp $4 0 crt_missing
  IntOp $5 $5 + 1
  IntCmp $5 3 0 0 crt_missing
  StrCpy $4 0
crt_next:
  IntOp $2 $2 + 1
  Goto crt_char
crt_end:
  StrCmp $4 0 crt_missing
  StrCmp $5 3 0 crt_missing
  ${VersionCompare} "$1" "14.51.36247.0" $0
  StrCmp $0 2 crt_missing
  SetRegView lastused
  DetailPrint "VC++ v14 x64 注册版本：$1（仅前置检查；未验证 DLL 完整性或运行兼容性）。"
  ClearErrors
  Return
crt_missing:
  SetRegView lastused
  MessageBox MB_OK|MB_ICONSTOP "需要 Microsoft Visual C++ v14 x64 运行库 14.51.36247.0 或更新版本；未安装、版本过旧或无法可靠读取注册信息。$\r$\n$\r$\n请自行从微软官方下载并安装最新受支持版本，再重新运行 Neo 安装器：$\r$\nhttps://aka.ms/vc14/vc_redist.x64.exe$\r$\n$\r$\nNeo 不自动联网、下载或安装运行库，也不请求提权；微软运行库安装可能需要管理员协助。现有 Neo 文件未改动。"
  SetErrorLevel 2
  Abort
FunctionEnd

Function .onInit
  Call AcquireLock
  Call CheckPlatform
FunctionEnd

Function un.onInit
  Call un.AcquireLock
FunctionEnd

!macro SaveShortcut PATH FLAG NAME
  Push "${PATH}"
  Call CheckShellPath
  IfErrors shell_save_failed
  StrCpy ${FLAG} "0"
  IfFileExists "${PATH}" 0 +4
  CopyFiles /SILENT "${PATH}" "$PLUGINSDIR\${NAME}"
  IfErrors shell_save_failed
  StrCpy ${FLAG} "1"
!macroend

!macro RestoreShortcut PATH FLAG NAME
  Push "${PATH}"
  Call CheckShellPath
  IfErrors restore_failed_${NAME}
  StrCmp ${FLAG} "1" 0 +3
  CopyFiles /SILENT "$PLUGINSDIR\${NAME}" "${PATH}"
  Goto +2
  Delete "${PATH}"
  IfErrors 0 restore_end_${NAME}
restore_failed_${NAME}:
  StrCpy $4 "1"
restore_end_${NAME}:
!macroend

Function SaveShellState
  InitPluginsDir
  ClearErrors
  !insertmacro SaveShortcut "$DESKTOP\Neo.lnk" $DesktopSaved "desktop.lnk"
  !insertmacro SaveShortcut "$SMPROGRAMS\Neo\Neo.lnk" $StartSaved "start.lnk"
  !insertmacro SaveShortcut "$SMPROGRAMS\Neo\卸载 Neo.lnk" $UninstallSaved "uninstall.lnk"
  StrCpy $RegistrySaved "0"
  System::Call 'advapi32::RegOpenKeyExW(p 0x80000001, w "${UNINSTALL_KEY}", i 0, i 0x20019, *p .r0) i .r1'
  StrCmp $1 2 shell_saved
  StrCmp $1 0 0 shell_save_failed
  System::Call 'advapi32::RegCloseKey(p r0)'
  nsExec::ExecToStack /TIMEOUT=15000 '"$SYSDIR\reg.exe" export "HKCU\${UNINSTALL_KEY}" "$PLUGINSDIR\uninstall.reg" /y'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 shell_save_failed
  StrCpy $RegistrySaved "1"
shell_saved:
  StrCpy $ShellSaved "1"
  ClearErrors
  Return
shell_save_failed:
  SetErrors
FunctionEnd

Function RestoreShellState
  StrCmp $ShellSaved "1" 0 shell_restore_end
  StrCmp $ShellDirty "1" 0 shell_restore_end
  StrCpy $4 "0"
  ClearErrors
  !insertmacro RestoreShortcut "$DESKTOP\Neo.lnk" $DesktopSaved "desktop.lnk"
  !insertmacro RestoreShortcut "$SMPROGRAMS\Neo\Neo.lnk" $StartSaved "start.lnk"
  !insertmacro RestoreShortcut "$SMPROGRAMS\Neo\卸载 Neo.lnk" $UninstallSaved "uninstall.lnk"
  ClearErrors
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; 删除后明确区分不存在与权限失败，不能把部分注册状态当作回滚成功。
  System::Call 'advapi32::RegOpenKeyExW(p 0x80000001, w "${UNINSTALL_KEY}", i 0, i 0x20019, *p .r0) i .r1'
  StrCmp $1 2 registry_removed
  StrCmp $1 0 0 registry_remove_failed
  System::Call 'advapi32::RegCloseKey(p r0)'
registry_remove_failed:
  StrCpy $4 "1"
registry_removed:
  StrCmp $RegistrySaved "1" 0 shell_restore_done
  nsExec::ExecToStack /TIMEOUT=15000 '"$SYSDIR\reg.exe" import "$PLUGINSDIR\uninstall.reg"'
  Pop $0
  Pop $1
  StrCmp $0 "0" 0 shell_restore_failed
shell_restore_done:
  StrCmp $4 "1" shell_restore_failed
  StrCpy $ShellSaved "0"
  StrCpy $ShellDirty "0"
  Goto shell_restore_end
shell_restore_failed:
  MessageBox MB_OK|MB_ICONSTOP "快捷方式或卸载注册信息恢复失败，请检查权限并重新安装旧版本。"
shell_restore_end:
FunctionEnd

Function RollbackInstall
  SetOutPath "$TEMP"
  StrCmp $Published "1" 0 restore_old
  ClearErrors
  Rename "$INSTDIR" "$StageDir"
  IfErrors rollback_failed
  StrCpy $Published "0"
restore_old:
  StrCmp $OldMoved "1" 0 rollback_end
  ClearErrors
  Rename "$BackupDir" "$INSTDIR"
  IfErrors rollback_failed
  StrCpy $OldMoved "0"
  Goto rollback_end
rollback_failed:
  MessageBox MB_OK|MB_ICONSTOP "自动恢复失败。请关闭占用程序后检查 $INSTDIR；若存在旧备份 $BackupDir，请保留并恢复它，不要删除备份。"
rollback_end:
FunctionEnd

Function InstallAbort
  Call RollbackInstall
  Call RestoreShellState
  StrCmp $StageDir "" abort_end
  RMDir /r "$StageDir"
abort_end:
FunctionEnd

Function .onInstFailed
  Call InstallAbort
FunctionEnd

Section "Install"
  ; After welcome/license, before any payload, backup rename or shell mutation.
  Call CheckVCRuntime
  StrCpy $OldMoved "0"
  StrCpy $Published "0"
  StrCpy $ShellDirty "0"
  Call CheckRunning
  IfErrors install_failed
  ; 只允许专用目录；绝不把 APPDATA、磁盘根或任意已有目录当作载荷。
  Call CheckInstallPath
  IfErrors install_failed
  ${GetParent} "$INSTDIR" $2
  StrCmp $2 "" install_failed
  StrCpy $BackupDir "$INSTDIR.neo-backup"
  System::Call 'kernel32::GetFileAttributesW(w "$BackupDir") i .r0 ?e'
  Pop $1
  IntCmp $0 -1 backup_missing
  IntOp $0 $0 & 0x400
  IntCmp $0 0 backup_checked install_failed install_failed
backup_missing:
  StrCmp $1 2 backup_checked
  StrCmp $1 3 backup_checked install_failed
backup_checked:
  IfFileExists "$BackupDir\*.*" 0 check_target
  IfFileExists "$INSTDIR\*.*" backup_pending
  ClearErrors
  Rename "$BackupDir" "$INSTDIR"
  IfErrors backup_pending
check_target:
  Call CheckInstallTarget
  IfErrors target_rejected
create_stage:
  ${GetParent} "$INSTDIR" $2
  ; 同卷唯一暂存目录，解包/取消时旧安装完全不动。
  ClearErrors
  CreateDirectory "$2"
  IfErrors install_failed
  GetTempFileName $StageDir "$2"
  IfErrors install_failed
  Delete "$StageDir"
  IfErrors install_failed
  CreateDirectory "$StageDir"
  IfErrors install_failed
  SetOutPath "$StageDir"
  IfErrors install_failed
  SetOverwrite on
  ClearErrors
  File /r "dist\neo\*.*"
!ifdef HAVE_ART
  File /oname=neo.ico "${ART}\neo.ico"
!endif
  IfErrors install_failed
  WriteUninstaller "$StageDir\uninstall.exe"
  IfErrors install_failed
  IfFileExists "$StageDir\neo.exe" 0 install_failed
  Call CheckRunning
  IfErrors install_failed
  Call CheckInstallTarget
  IfErrors target_rejected
  Call SaveShellState
  IfErrors install_failed
  SetOutPath "$TEMP"
  ; 下面只有目录重命名，不逐个删除或覆盖旧文件。占用导致失败则原样退出。
  IfFileExists "$INSTDIR\*.*" 0 publish
  ClearErrors
  Rename "$INSTDIR" "$BackupDir"
  IfErrors install_failed
  StrCpy $OldMoved "1"
  Call CheckRunning
  IfErrors install_failed
publish:
  ClearErrors
  Rename "$StageDir" "$INSTDIR"
  IfErrors install_failed
  StrCpy $Published "1"
  SetOutPath "$INSTDIR"

  StrCpy $ShellDirty "1"
  CreateDirectory "$SMPROGRAMS\Neo"
!ifdef HAVE_ART
  CreateShortcut "$SMPROGRAMS\Neo\Neo.lnk" "$INSTDIR\neo.exe" "" "$INSTDIR\neo.ico" 0
  CreateShortcut "$DESKTOP\Neo.lnk" "$INSTDIR\neo.exe" "" "$INSTDIR\neo.ico" 0
!else
  CreateShortcut "$SMPROGRAMS\Neo\Neo.lnk" "$INSTDIR\neo.exe"
  CreateShortcut "$DESKTOP\Neo.lnk" "$INSTDIR\neo.exe"
!endif
  CreateShortcut "$SMPROGRAMS\Neo\卸载 Neo.lnk" "$INSTDIR\uninstall.exe"
  IfErrors install_failed

  ; 「应用和功能」卸载项（per-user 安装写 HKCU）
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "DisplayName" "Neo"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "Publisher" "Neo"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
!ifdef HAVE_ART
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "DisplayIcon" "$\"$INSTDIR\neo.ico$\""
!endif
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "EstimatedSize" $0
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo" "NoRepair" 1
  IfErrors install_failed
  ; 提交后保留完整旧目录（包括载荷子目录内的未知文件），移到本次唯一暂存路径。
  ; 无清单时不能将 resources/docs/runtime 或旧 assets/assets-stt 整棵目录删除。
  StrCpy $Published "0"
  StrCmp $OldMoved "1" 0 committed
  StrCpy $OldMoved "0"
  ClearErrors
  Rename "$BackupDir" "$StageDir"
  IfErrors keep_backup
  StrCpy $BackupDir "$StageDir"
keep_backup:
  DetailPrint "旧安装与自定义文件保留在 $BackupDir"
committed:
  StrCpy $ShellSaved "0"
  StrCpy $ShellDirty "0"
  StrCpy $StageDir ""
  Goto install_end
target_rejected:
  MessageBox MB_OK|MB_ICONSTOP "该非空目录不是可确认的 Neo 安装或卸载残留。旧版卸载没有可信残留记录时，请先将原目录改名保留，再安装到原路径；不要删除其中的自定义文件。"
  Goto install_failed
backup_pending:
  MessageBox MB_OK|MB_ICONSTOP "检测到上次保留的备份 $BackupDir。请先确认或恢复该备份；本次不会覆盖它。"
install_failed:
  Call InstallAbort
  SetErrorLevel 1
  MessageBox MB_OK|MB_ICONSTOP "安装未完成。旧安装未删除；请检查运行实例、文件占用、磁盘空间和目录权限后重试。"
  Abort
install_end:
SectionEnd

Section "Uninstall"
  Call un.CheckRunning
  IfErrors uninstall_failed
  ; 不能仅凭一份复制来的卸载器，为任意非空目录创建可信残留记录。
  Call un.CheckInstallTarget
  IfErrors uninstall_failed
  Push "$DESKTOP\Neo.lnk"
  Call un.CheckShellPath
  IfErrors uninstall_failed
  Push "$SMPROGRAMS\Neo\Neo.lnk"
  Call un.CheckShellPath
  IfErrors uninstall_failed
  Push "$SMPROGRAMS\Neo\卸载 Neo.lnk"
  Call un.CheckShellPath
  IfErrors uninstall_failed
  ; 在任何删除之前持久化并读回目录身份，失败则保留完整安装。
  ; 单独的 HKCU 记录不随卸载注册项删除，也不信任目录内的标识文件。
  ; 这是同一用户安装历史的证据，不是抵御该用户主动篡改注册表的安全边界。
  Call un.GetResidueKey
  IfErrors uninstall_failed
  ClearErrors
  WriteRegStr HKCU "$ResidueKey" "Path" "$INSTDIR"
  IfErrors uninstall_failed
  ReadRegStr $0 HKCU "$ResidueKey" "Path"
  IfErrors uninstall_failed
  StrCmp $0 $INSTDIR 0 uninstall_failed
  ClearErrors
  ; 主程序删除失败时不先破坏快捷方式、资源和注册项。
  Delete "$INSTDIR\neo.exe"
  IfErrors uninstall_failed
  Delete "$DESKTOP\Neo.lnk"
  Delete "$SMPROGRAMS\Neo\Neo.lnk"
  Delete "$SMPROGRAMS\Neo\卸载 Neo.lnk"
  IfErrors uninstall_failed
  RMDir "$SMPROGRAMS\Neo"

  ; 没有文件清单时仅删除已知顶层文件，资源目录中的自定义数据保留。
  ClearErrors
  Delete "$INSTDIR\neo.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\NOTICE"
  ; 与 check_release.py 的 CRT_NAMES 对齐；不使用 *.dll，保留未知用户文件。
  Delete "$INSTDIR\vcruntime140.dll"
  Delete "$INSTDIR\vcruntime140_1.dll"
  Delete "$INSTDIR\vcruntime140_threads.dll"
  Delete "$INSTDIR\msvcp140.dll"
  Delete "$INSTDIR\msvcp140_1.dll"
  Delete "$INSTDIR\msvcp140_2.dll"
  Delete "$INSTDIR\msvcp140_atomic_wait.dll"
  Delete "$INSTDIR\msvcp140_codecvt_ids.dll"
  Delete "$INSTDIR\concrt140.dll"
  IfErrors uninstall_failed
  ; 注册项删除失败时保留卸载器，以便用户修复权限后重试。
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo"
  System::Call 'advapi32::RegOpenKeyExW(p 0x80000001, w "${UNINSTALL_KEY}", i 0, i 0x20019, *p .r0) i .r1'
  StrCmp $1 2 uninstall_registry_removed
  StrCmp $1 0 0 uninstall_failed
  System::Call 'advapi32::RegCloseKey(p r0)'
  Goto uninstall_failed
uninstall_registry_removed:
  ClearErrors
  Delete "$INSTDIR\uninstall.exe"
  IfErrors uninstall_failed
  ; 新旧布局均仅自底向上清理空目录；模型、文档及自定义文件不递归删除。
  RMDir "$INSTDIR\assets"
  RMDir "$INSTDIR\assets-stt\sense-voice"
  RMDir "$INSTDIR\assets-stt\vad"
  RMDir "$INSTDIR\assets-stt"
  RMDir "$INSTDIR\resources\models\wake"
  RMDir "$INSTDIR\resources\models\stt\sense-voice"
  RMDir "$INSTDIR\resources\models\stt\vad"
  RMDir "$INSTDIR\resources\models\stt"
  RMDir "$INSTDIR\resources\models"
  RMDir "$INSTDIR\resources\lang"
  RMDir "$INSTDIR\resources"
  RMDir "$INSTDIR\docs\licenses\assets"
  RMDir "$INSTDIR\docs\licenses\runtime"
  RMDir "$INSTDIR\docs\licenses"
  RMDir "$INSTDIR\docs"
  RMDir "$INSTDIR\runtime\onnx"
  RMDir "$INSTDIR\runtime\gitbash"
  RMDir "$INSTDIR\runtime"
  ClearErrors
  RMDir "$INSTDIR"
  ; 非空目录和绑定它的记录一起保留；只有确实删除空目录才移除记录。
  IfErrors uninstall_residue_kept
  DeleteRegKey HKCU "$ResidueKey"
uninstall_residue_kept:

  ; 用户数据 %APPDATA%\Neo（数据库/记忆）保留，不再需要可手动删除
  Goto uninstall_end
uninstall_failed:
  SetErrorLevel 1
  MessageBox MB_OK|MB_ICONSTOP "无法安全卸载。请退出 Neo，并检查安装目录和权限后重试。"
  Abort
uninstall_end:
SectionEnd
