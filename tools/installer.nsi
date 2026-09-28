; Neo Windows x64 安装包（per-user 安装，免 UAC 提权）。
; 由 .github/workflows/release.yml 调用（本地手动编译同）：
;   makensis /NOCD /INPUTCHARSET UTF8 /DVERSION=1.2.3 /DVI_VERSION=1.2.3.0 tools\installer.nsi
; ⚠️ 两个开关都不能省：/NOCD 保持工作目录在仓库根（默认会切到 tools\，
; dist\ 与 build\ 的相对路径会全找不到）；/INPUTCHARSET UTF8 读本脚本的
; 中文注释（默认 ACP 直接报 Bad text encoding）。
; 输入：dist\neo\（已装配好的发行目录）+ build\installer-art\（美术资源，
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

!define ART "build\installer-art"
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
VIAddVersionKey /LANG=2052 "LegalCopyright" "MIT"

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

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo"

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

!insertmacro MUI_PAGE_WELCOME
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
  MessageBox MB_OK|MB_ICONSTOP "请完全退出 Neo 后重试。无法确认进程已退出时，安装不会继续。"
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
!macroend
!insertmacro InitTransaction ""
!insertmacro InitTransaction "un."

Function .onInit
  Call AcquireLock
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
  IfFileExists "$INSTDIR\*.*" 0 create_stage
  IfFileExists "$INSTDIR\neo.exe" 0 install_failed
  IfFileExists "$INSTDIR\uninstall.exe" 0 install_failed
create_stage:
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
  Call CheckInstallPath
  IfErrors install_failed
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
  ; 无清单时不能将 assets/runtime 整棵目录当成可删除的已知文件。
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
  Call un.CheckInstallPath
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
  RMDir "$INSTDIR\assets"
  RMDir "$INSTDIR\assets-stt"
  RMDir "$INSTDIR\runtime"
  RMDir "$INSTDIR"

  ; 用户数据 %APPDATA%\Neo（数据库/记忆）保留，不再需要可手动删除
  Goto uninstall_end
uninstall_failed:
  SetErrorLevel 1
  MessageBox MB_OK|MB_ICONSTOP "无法安全卸载。请退出 Neo，并检查安装目录和权限后重试。"
  Abort
uninstall_end:
SectionEnd
