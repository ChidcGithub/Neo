; Neo Windows x64 安装包（per-user 安装，免 UAC 提权）。
; 由 .github/workflows/release.yml 调用：
;   makensis /DVERSION=1.2.3 /DVI_VERSION=1.2.3.0 tools\installer.nsi
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

Section "Install"
  SetOutPath $INSTDIR

  ; 覆盖式升级：只清我们安装过的载荷，不用 RMDir /r $INSTDIR
  ; （防止用户把安装目录改到别处时被连锅端）。
  ; 用户数据在 %APPDATA%\Neo，与安装目录无关，不受影响。
  Delete "$INSTDIR\neo.exe"
  Delete "$INSTDIR\neo.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\assets"
  RMDir /r "$INSTDIR\assets-stt"
  RMDir /r "$INSTDIR\runtime"

  File /r "dist\neo\*.*"
!ifdef HAVE_ART
  File /oname=neo.ico "${ART}\neo.ico"
!endif

  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Neo"
!ifdef HAVE_ART
  CreateShortcut "$SMPROGRAMS\Neo\Neo.lnk" "$INSTDIR\neo.exe" "" "$INSTDIR\neo.ico" 0
  CreateShortcut "$DESKTOP\Neo.lnk" "$INSTDIR\neo.exe" "" "$INSTDIR\neo.ico" 0
!else
  CreateShortcut "$SMPROGRAMS\Neo\Neo.lnk" "$INSTDIR\neo.exe"
  CreateShortcut "$DESKTOP\Neo.lnk" "$INSTDIR\neo.exe"
!endif
  CreateShortcut "$SMPROGRAMS\Neo\卸载 Neo.lnk" "$INSTDIR\uninstall.exe"

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
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\Neo.lnk"
  RMDir /r "$SMPROGRAMS\Neo"

  ; 与安装侧同款的定点清理；RMDir（非 /r）只在目录已空时移除
  Delete "$INSTDIR\neo.exe"
  Delete "$INSTDIR\neo.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\assets"
  RMDir /r "$INSTDIR\assets-stt"
  RMDir /r "$INSTDIR\runtime"
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Neo"
  ; 用户数据 %APPDATA%\Neo（数据库/记忆）保留，不再需要可手动删除
SectionEnd
