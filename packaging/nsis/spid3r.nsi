; ============================================================================
;  Spid3r — Windows installer (NSIS 3.x)
;
;  Per-user install (no administrator prompt):
;    * installs the toolchain to %LOCALAPPDATA%\Programs\Spid3r
;    * adds that directory to the CURRENT USER's PATH (new cmd/PowerShell
;      windows can run `spid3r` directly)
;    * creates Start Menu shortcuts in Programs\Spid3r
;    * optional: desktop shortcuts, examples, benchmarks, docs, and .matrix
;      file association
;    * registers uninstaller and complete metadata in "Apps & features"
;
;  Build:  `cargo build --release` on Windows runs this script from build.rs,
;          passing APPVERSION (Cargo.toml), WITH_GUI, SPID3R_EXE, and optionally
;          CALCULATOR_EXE.
;          Manual: makensis spid3r.nsi (from this directory, after a release build)
;  Test:   Spid3r-Setup-<ver>.exe /S /D=C:\some\dir
; ============================================================================

Unicode true
!include "MUI2.nsh"
!include "WordFunc.nsh"

!ifndef WITH_GUI
!define WITH_GUI 1
!endif

!ifndef WITH_NATIVE
!define WITH_NATIVE 1
!endif

!ifndef LLVM_DLL
!define LLVM_DLL "C:\Program Files\LLVM\bin\LLVM-C.dll"
!endif
!ifndef LLVM_LIB
!define LLVM_LIB "C:\Program Files\LLVM\lib\LLVM-C.lib"
!endif
!ifndef RUNTIME_LIB
!define RUNTIME_LIB "..\..\target\release\spid3r_lang.lib"
!endif
!ifndef APPVERSION
!define APPVERSION     "1.0.0"
!endif
!ifndef SPID3R_EXE
!define SPID3R_EXE     "..\..\target\release\spid3r.exe"
!endif
!ifndef CALCULATOR_EXE
!define CALCULATOR_EXE "..\..\target\release\calculator.exe"
!endif

!define APPNAME        "Spid3r"
!define PUBLISHER      "ANSNEW TECH."
!define UNINSTKEY      "Software\Microsoft\Windows\CurrentVersion\Uninstall\Spid3r"
!define DOCSURL        "https://needyamin.github.io/spid3r/docs/html/doc.html"
!define REPOURL        "https://github.com/needyamin/spid3r"

!ifndef HWND_BROADCAST
!define HWND_BROADCAST 0xFFFF
!endif
!ifndef WM_SETTINGCHANGE
!define WM_SETTINGCHANGE 0x001A
!endif

Name "${APPNAME} ${APPVERSION}"
OutFile "..\..\Spid3r-Setup-${APPVERSION}.exe"
InstallDir "$LOCALAPPDATA\Programs\Spid3r"
InstallDirRegKey HKCU "Software\Spid3r" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!if ${WITH_NATIVE} == 1
BrandingText "Spid3r Programming Language — Dual-Engine VM & Native AOT Compiler"
!else if ${WITH_GUI} == 1
BrandingText "Spid3r Programming Language — Dual-Engine VM with Webview GUI"
!else
BrandingText "Spid3r Programming Language — Zero-Dependency Core Toolchain"
!endif

; ----------------------------------------------------------------- UI ------
!define MUI_ABORTWARNING
!define MUI_ICON   "assets\spid3r.ico"
!define MUI_UNICON "assets\spid3r.ico"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "assets\header.bmp"
!define MUI_WELCOMEFINISHPAGE_BITMAP "assets\welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "assets\welcome.bmp"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE.txt"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_TITLE "Spid3r is ready"
!if ${WITH_GUI} == 1
!define FINISH_GUI      "$\r$\n    spid3r gui examples\demo_apps\web_calculator\calculator.matrix"
!define FINISH_GUI_NOTE " GUI programs need the WebView2 runtime (included with Windows 11 and Microsoft Edge)."
!else
!define FINISH_GUI      ""
!define FINISH_GUI_NOTE ""
!endif
!if ${WITH_NATIVE} == 1
!define FINISH_NATIVE      "$\r$\n    spid3r build examples\hello.matrix"
!define FINISH_NATIVE_NOTE " spid3r build also needs Visual Studio Build Tools (C++ workload with the Windows SDK) to link executables; rerun setup and select the C++ Build Tools component if you skipped it."
!else
!define FINISH_NATIVE      ""
!define FINISH_NATIVE_NOTE ""
!endif
!define MUI_FINISHPAGE_TEXT "Spid3r has been installed to $INSTDIR and added to your user PATH.$\r$\n$\r$\nStart Menu shortcuts have been created in 'Spid3r'.$\r$\n$\r$\nRun in a NEW terminal window:$\r$\n    spid3r version$\r$\n    spid3r run examples\hello.matrix${FINISH_GUI}${FINISH_NATIVE}$\r$\n$\r$\n${FINISH_GUI_NOTE}${FINISH_NATIVE_NOTE}"
!define MUI_FINISHPAGE_SHOWREADME "${DOCSURL}"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Open online Spid3r documentation"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ------------------------------------------------------------ sections -----
Section "Spid3r compiler & runtime (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "${SPID3R_EXE}"
!if ${WITH_NATIVE} == 1
  File "${LLVM_DLL}"
  File "${LLVM_LIB}"
  File "${RUNTIME_LIB}"
!endif
!if ${WITH_GUI} == 1
!if /FileExists "${CALCULATOR_EXE}"
  File "${CALCULATOR_EXE}"
!endif
!endif
  File "assets\spid3r.ico"
  SetOutPath "$INSTDIR\stdlib"
  File /r "..\..\stdlib\*.*"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; --- add to the current user's PATH (deduplicated) ---
  ReadRegStr $R0 HKCU "Environment" "Path"
  ${WordAdd} "$R0" ";" "E+$INSTDIR" $R1
  StrCmp "$R0" "$R1" +2
    WriteRegExpandStr HKCU "Environment" "Path" "$R1"
  StrCmp "$R0" "$R1" +2
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  WriteRegStr HKCU "Software\Spid3r" "InstallDir" "$INSTDIR"

  ; --- Start Menu shortcuts ---
  CreateDirectory "$SMPROGRAMS\Spid3r"
  CreateShortCut "$SMPROGRAMS\Spid3r\Spid3r Command Shell.lnk" "$SYSDIR\cmd.exe" '/K "spid3r version & echo Welcome to Spid3r! Type spid3r --help for commands."' "$INSTDIR\spid3r.ico" 0
!if ${WITH_GUI} == 1
!if /FileExists "${CALCULATOR_EXE}"
  CreateShortCut "$SMPROGRAMS\Spid3r\Spid3r Calculator.lnk" "$INSTDIR\calculator.exe" "" "$INSTDIR\spid3r.ico" 0
!endif
!endif
  CreateShortCut "$SMPROGRAMS\Spid3r\Uninstall Spid3r.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\uninstall.exe" 0

  ; --- Apps & features entry ---
  WriteRegStr   HKCU "${UNINSTKEY}" "DisplayName"          "${APPNAME} ${APPVERSION}"
  WriteRegStr   HKCU "${UNINSTKEY}" "DisplayVersion"       "${APPVERSION}"
  WriteRegStr   HKCU "${UNINSTKEY}" "Publisher"            "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINSTKEY}" "DisplayIcon"          "$INSTDIR\spid3r.ico"
  WriteRegStr   HKCU "${UNINSTKEY}" "InstallLocation"      "$INSTDIR"
  WriteRegStr   HKCU "${UNINSTKEY}" "HelpLink"             "${DOCSURL}"
  WriteRegStr   HKCU "${UNINSTKEY}" "URLInfoAbout"         "${DOCSURL}"
  WriteRegStr   HKCU "${UNINSTKEY}" "URLUpdateInfo"        "${REPOURL}"
  WriteRegStr   HKCU "${UNINSTKEY}" "Comments"             "Spid3r programming language runtime, compiler, and tools"
  WriteRegDWORD HKCU "${UNINSTKEY}" "EstimatedSize"        35000
  WriteRegStr   HKCU "${UNINSTKEY}" "UninstallString"      '"$INSTDIR\uninstall.exe"'
  WriteRegStr   HKCU "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify"             1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair"             1
SectionEnd

; ------------------------------------------------------- prerequisites -----
!ifdef WEBVIEW2_SETUP
; Registry keys from Microsoft's WebView2 distribution guide; the 32-bit
; registry view maps HKLM\SOFTWARE to WOW6432Node on 64-bit Windows.
!define WEBVIEW2_KEY "Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

Section "WebView2 runtime (for spid3r gui; only if missing)" SecWebView2
  ReadRegStr $0 HKLM "SOFTWARE\${WEBVIEW2_KEY}" "pv"
  StrCmp $0 "" 0 +2
    ReadRegStr $0 HKCU "Software\${WEBVIEW2_KEY}" "pv"
  StrCmp $0 "" webview2_install
  StrCmp $0 "0.0.0.0" webview2_install
  DetailPrint "WebView2 runtime $0 is already installed."
  Goto webview2_done
webview2_install:
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "/oname=MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2_SETUP}"
  DetailPrint "Installing the Microsoft Edge WebView2 runtime..."
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $1
  StrCmp $1 0 +2
    DetailPrint "WebView2 setup exited with code $1; spid3r gui needs it (https://developer.microsoft.com/microsoft-edge/webview2/)."
  SetOutPath "$INSTDIR"
webview2_done:
SectionEnd
!endif

!if ${WITH_NATIVE} == 1
!define VSWHERE      "$PROGRAMFILES32\Microsoft Visual Studio\Installer\vswhere.exe"
!define BUILDTOOLS   "https://aka.ms/vs/17/release/vs_BuildTools.exe"
!define BUILDTOOLS_PAGE "https://visualstudio.microsoft.com/visual-cpp-build-tools/"

Section /o "C++ Build Tools for spid3r build (downloads ~3 GB, needs admin)" SecBuildTools
  ; spid3r build links with the Build Tools edition's C++ toolset; skip if present.
  IfFileExists "${VSWHERE}" 0 buildtools_install
  nsExec::ExecToStack '"${VSWHERE}" -products Microsoft.VisualStudio.Product.BuildTools -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath'
  Pop $0
  Pop $1
  StrCmp $1 "" buildtools_install
  DetailPrint "Visual Studio Build Tools with the C++ toolset are already installed."
  Goto buildtools_done
buildtools_install:
  IfFileExists "$SYSDIR\curl.exe" 0 buildtools_failed
  DetailPrint "Downloading the Visual Studio Build Tools installer from Microsoft..."
  nsExec::ExecToLog '"$SYSDIR\curl.exe" -fL --retry 3 -o "$TEMP\spid3r_vs_BuildTools.exe" "${BUILDTOOLS}"'
  Pop $0
  StrCmp $0 0 0 buildtools_failed
  DetailPrint "Installing the C++ build tools and Windows SDK (this can take a while)..."
  ExecWait '"$TEMP\spid3r_vs_BuildTools.exe" --passive --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended' $0
  Delete "$TEMP\spid3r_vs_BuildTools.exe"
  StrCmp $0 0 buildtools_done
  StrCmp $0 3010 0 +3
    DetailPrint "Build Tools installed; restart Windows before using spid3r build."
    Goto buildtools_done
  DetailPrint "Build Tools setup exited with code $0."
buildtools_failed:
  MessageBox MB_OK|MB_ICONEXCLAMATION "The C++ Build Tools could not be installed automatically.$\r$\n$\r$\nspid3r build needs them to link executables; everything else works without them. Install them later from:$\r$\n${BUILDTOOLS_PAGE}$\r$\n(select the $\"Desktop development with C++$\" workload)." /SD IDOK
buildtools_done:
SectionEnd
!endif

Section "Examples, documentation & benchmarks" SecDocs
  SetOutPath "$INSTDIR"
  File "..\..\README.md"
  SetOutPath "$INSTDIR\docs"
  File /r "..\..\docs\*.*"
  SetOutPath "$INSTDIR\examples"
  File /r "..\..\examples\*.*"
  SetOutPath "$INSTDIR\benchmarks"
  File /r "..\..\benchmarks\*.*"

  ; --- Documentation shortcuts in Start Menu ---
  CreateDirectory "$SMPROGRAMS\Spid3r"
  CreateShortCut "$SMPROGRAMS\Spid3r\Developer Guide.lnk" "$INSTDIR\docs\html\doc.html" "" "$INSTDIR\spid3r.ico" 0
  CreateShortCut "$SMPROGRAMS\Spid3r\WebAssembly Playground.lnk" "$INSTDIR\docs\html\playground.html" "" "$INSTDIR\spid3r.ico" 0
  CreateShortCut "$SMPROGRAMS\Spid3r\Benchmarks Dashboard.lnk" "$INSTDIR\docs\html\benchmarks.html" "" "$INSTDIR\spid3r.ico" 0
SectionEnd

Section /o "Desktop shortcuts" SecDesktop
  CreateShortCut "$DESKTOP\Spid3r Shell.lnk" "$SYSDIR\cmd.exe" '/K "spid3r version"' "$INSTDIR\spid3r.ico" 0
!if ${WITH_GUI} == 1
!if /FileExists "${CALCULATOR_EXE}"
  CreateShortCut "$DESKTOP\Spid3r Calculator.lnk" "$INSTDIR\calculator.exe" "" "$INSTDIR\spid3r.ico" 0
!endif
!endif
SectionEnd

Section "Associate .matrix files with Spid3r" SecAssoc
  WriteRegStr HKCU "Software\Classes\.matrix" "" "Spid3r.Matrix"
  WriteRegStr HKCU "Software\Classes\.matrix" "Content Type" "text/plain"
  WriteRegStr HKCU "Software\Classes\.matrix" "PerceivedType" "text"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix" "" "Spid3r Program"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\DefaultIcon" "" "$INSTDIR\spid3r.ico"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\open\command" "" '"$INSTDIR\spid3r.exe" run "%1"'
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\run" "" "Run with Spid3r"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\run\command" "" '"$INSTDIR\spid3r.exe" run "%1"'
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\check" "" "Check Syntax (spid3r check)"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\check\command" "" '"$INSTDIR\spid3r.exe" check "%1"'
!if ${WITH_GUI} == 1
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\gui" "" "Run with GUI (spid3r gui)"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\gui\command" "" '"$INSTDIR\spid3r.exe" gui "%1"'
!endif
  System::Call 'shell32::SHChangeNotify(l 0x08000000, l 0, p 0, p 0)'
SectionEnd

; --------------------------------------------------------- descriptions ----
!if ${WITH_NATIVE} == 1
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: run / compile / exec / check / test / fmt / debug / repl / gui / build (native executables via LLVM), standalone calculator app, and PATH registration."
!else if ${WITH_GUI} == 1
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: run / compile / exec / check / test / fmt / debug / repl / gui, standalone calculator app, and PATH registration."
!else
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: run / compile / exec / check / test / fmt / debug / repl, plus automatic PATH registration."
!endif
LangString DESC_SecDocs  ${LANG_ENGLISH} "The 25 example programs, full benchmark suite, and local documentation (Developer Guide, Playground, Benchmarks dashboard)."
LangString DESC_SecDesktop ${LANG_ENGLISH} "Creates convenient shortcuts on your desktop for quick terminal access and the standalone calculator."
LangString DESC_SecAssoc ${LANG_ENGLISH} "Associates .matrix source files with Spid3r and registers Explorer context menu actions (Run, Check syntax, GUI)."
!ifdef WEBVIEW2_SETUP
LangString DESC_SecWebView2 ${LANG_ENGLISH} "Installs the Microsoft Edge WebView2 runtime that spid3r gui windows use, only if it is missing (Windows 11 and Microsoft Edge already include it)."
!endif
!if ${WITH_NATIVE} == 1
LangString DESC_SecBuildTools ${LANG_ENGLISH} "Downloads and installs Microsoft Visual Studio Build Tools (C++ workload with the Windows SDK), which spid3r build needs to link executables. About 3 GB; asks for administrator rights; skipped if already installed. Installing means accepting Microsoft's Build Tools license."
!endif

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}     $(DESC_SecCore)
!ifdef WEBVIEW2_SETUP
  !insertmacro MUI_DESCRIPTION_TEXT ${SecWebView2} $(DESC_SecWebView2)
!endif
!if ${WITH_NATIVE} == 1
  !insertmacro MUI_DESCRIPTION_TEXT ${SecBuildTools} $(DESC_SecBuildTools)
!endif
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDocs}     $(DESC_SecDocs)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop}  $(DESC_SecDesktop)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAssoc}    $(DESC_SecAssoc)
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ------------------------------------------------------------ uninstall ----
Section "Uninstall"
  ; --- remove the PATH entry ---
  ReadRegStr $R0 HKCU "Environment" "Path"
  ${WordAdd} "$R0" ";" "E-$INSTDIR" $R1
  StrCmp "$R0" "$R1" +2
    WriteRegExpandStr HKCU "Environment" "Path" "$R1"
  StrCmp "$R0" "$R1" +2
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  ; --- file association ---
  DeleteRegKey HKCU "Software\Classes\.matrix"
  DeleteRegKey HKCU "Software\Classes\Spid3r.Matrix"
  System::Call 'shell32::SHChangeNotify(l 0x08000000, l 0, p 0, p 0)'

  ; --- Start Menu & Desktop shortcuts ---
  RMDir /r "$SMPROGRAMS\Spid3r"
  Delete "$DESKTOP\Spid3r Shell.lnk"
  Delete "$DESKTOP\Spid3r Calculator.lnk"

  ; --- uninstall entry + install record ---
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "Software\Spid3r"

  ; --- files ---
  Delete "$INSTDIR\spid3r.exe"
  Delete "$INSTDIR\calculator.exe"
  Delete "$INSTDIR\LLVM-C.dll"
  Delete "$INSTDIR\LLVM-C.lib"
  Delete "$INSTDIR\spid3r_lang.lib"
  Delete "$INSTDIR\spid3r.lib"
  Delete "$INSTDIR\spid3r.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\docs"
  RMDir /r "$INSTDIR\stdlib"
  RMDir /r "$INSTDIR\examples"
  RMDir /r "$INSTDIR\benchmarks"
  RMDir "$INSTDIR"
SectionEnd
