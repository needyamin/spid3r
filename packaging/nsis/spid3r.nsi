; ============================================================================
;  Spid3r — Windows installer (NSIS 3.x)
;
;  Per-user install (no administrator prompt):
;    * installs the toolchain to %LOCALAPPDATA%\Programs\Spid3r
;    * optionally adds that directory to the CURRENT USER's PATH (checked by
;      default on the Welcome & Components pages; opt out via checkbox, /NOPATH,
;      or SPID3R_NO_PATH=1)
;    * creates Start Menu shortcuts in Programs\Spid3r
;    * optional: desktop shortcuts, .matrix/.mt file association, IDE extension
;    * terminates running Spid3r processes cleanly before upgrade / uninstall
;      so no locked executables block re-install or leave orphan files behind
;    * registers uninstaller and complete metadata in "Apps & features"
;
;  The payload is the toolchain only — no docs, examples, benchmarks or
;  stdlib sources. Those live in the repository and at DOCSURL, and the stdlib
;  is already compiled into spid3r.exe.
;
;  Build:  `cargo build --release` on Windows runs this script from build.rs,
;          passing APPVERSION (Cargo.toml), WITH_GUI, SPID3R_EXE, and optionally
;          CALCULATOR_EXE.
;          Manual: makensis spid3r.nsi (from this directory, after a release build)
;  Test:   Spid3r-Setup-<ver>.exe /S [/NOPATH] [/NOEDITOR] /D=C:\some\dir
; ============================================================================

Unicode true
!include "MUI2.nsh"
!include "WordFunc.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"
!include "Sections.nsh"

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
!define APPVERSION     "1.1.1"
!endif
!ifndef SPID3R_EXE
!define SPID3R_EXE     "..\..\target\release\spid3r.exe"
!endif
!ifndef CALCULATOR_EXE
!define CALCULATOR_EXE "..\..\target\release\calculator.exe"
!endif
!ifndef FORMATTER_VSIX
!if /FileExists "..\vscode\spid3r-formatter.vsix"
!define FORMATTER_VSIX "..\vscode\spid3r-formatter.vsix"
!endif
!endif
!ifdef FORMATTER_VSIX
  !if "${FORMATTER_VSIX}" != ""
    !if /FileExists "${FORMATTER_VSIX}"
      !define HAVE_FORMATTER_VSIX 1
    !endif
  !endif
!endif
; The desktop control panel (spid3r-control.exe). Passed by build.rs when the
; gui payload built it; absent in core-only installers.
;!ifdef CONTROL_EXE — defined on the makensis command line
; The Stage-2 self-hosted compiler (spid3r-self.exe) and its runtime pin
; metadata (Phase 10.1, SH-2 / SH-6), passed by tools/release/release_selfhost.sh:
;!ifdef SPID3R_SELF_EXE
;!ifdef RUNTIME_SHA256
;!ifdef RUNTIME_MANIFEST

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

Var WelcomePathCheckbox
Var PathAdded
Var FinishPathSummary
Var FinishRunHint

Name "${APPNAME} ${APPVERSION}"
!ifndef OUTFILE
!define OUTFILE "..\..\dist\Spid3r-Setup-${APPVERSION}.exe"
!endif
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Spid3r"
InstallDirRegKey HKCU "Software\Spid3r" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!if ${WITH_NATIVE} == 1
BrandingText "Spid3r Programming Language — Spid3r Engine (spvm) & YamLLVM Native AOT"
!else if ${WITH_GUI} == 1
BrandingText "Spid3r Programming Language — Spid3r Engine (spvm) with Webview GUI"
!else
BrandingText "Spid3r Programming Language — Spid3r Engine Zero-Dependency Core Toolchain"
!endif

; ----------------------------------------------------------------- UI ------
!define MUI_ABORTWARNING
!define MUI_ICON   "assets\spid3r.ico"
!define MUI_UNICON "assets\spid3r.ico"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "assets\header.bmp"
!define MUI_WELCOMEFINISHPAGE_BITMAP "assets\welcome.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "assets\welcome.bmp"

!define MUI_WELCOMEPAGE_TEXT "Setup will guide you through installing Spid3r ${APPVERSION} per-user in your Local AppData folder (no administrator prompt needed).$\r$\n$\r$\nClick Next to continue, or uncheck the PATH option below if you prefer not to modify your shell PATH."
!define MUI_PAGE_CUSTOMFUNCTION_SHOW WelcomeShow
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE WelcomeLeave
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE.txt"
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_TITLE "Spid3r is ready"
!define MUI_FINISHPAGE_TEXT_LARGE
!if ${WITH_GUI} == 1
!define FINISH_GUI_NOTE "$\r$\nGUI windows require WebView2 (included in Windows 11 & Edge)."
!else
!define FINISH_GUI_NOTE ""
!endif
!if ${WITH_NATIVE} == 1
!define FINISH_NATIVE_NOTE "$\r$\nNative AOT compiler included: yamc / spid3r build."
!else
!define FINISH_NATIVE_NOTE ""
!endif
!define MUI_FINISHPAGE_TEXT "Spid3r has been installed to $INSTDIR.$FinishPathSummary$\r$\n$\r$\n$FinishRunHint${FINISH_GUI_NOTE}${FINISH_NATIVE_NOTE}"
!define MUI_FINISHPAGE_SHOWREADME "${DOCSURL}"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Open online Spid3r documentation"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ------------------------------------------------ process / path helpers ---
; Shared by both installer (`UN = ""`) and uninstaller (`UN = "un."`) so
; upgrades and uninstalls terminate running Spid3r binaries cleanly, clean up
; locked files safely, and never truncate long (>1024-char) user PATHs.
!macro DEFINE_FILE_HELPERS UN
Function ${UN}StopSpid3rProcesses
  Push $1
  Push $2
  ${If} ${FileExists} "$WINDIR\Sysnative\taskkill.exe"
    StrCpy $2 "$WINDIR\Sysnative\taskkill.exe"
  ${Else}
    StrCpy $2 "$SYSDIR\taskkill.exe"
  ${EndIf}
  ${If} ${FileExists} "$2"
    nsExec::ExecToLog '"$2" /F /T /IM "spid3r-control.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "spid3r-self.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "spid3r.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "sp.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "yamc.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "yamcvm.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "spvm.exe"'
    Pop $1
    nsExec::ExecToLog '"$2" /F /T /IM "spkg.exe"'
    Pop $1
    Sleep 400
  ${EndIf}
  Pop $2
  Pop $1
FunctionEnd

; Delete relative filename $0 inside $INSTDIR even if locked by a running
; process. Order: direct delete -> stop Spid3r processes + retry -> rename to
; *.old and delete without prompting for a reboot.
Function ${UN}ForceDeletePath
  Push $1
  Push $2
  ${If} ${FileExists} "$INSTDIR\$0"
    ClearErrors
    Delete "$INSTDIR\$0"
    ${If} ${FileExists} "$INSTDIR\$0"
      Call ${UN}StopSpid3rProcesses
      ; Only kill calculator.exe if this installation owns a locked copy.
      ${If} $0 == "calculator.exe"
        ${If} ${FileExists} "$WINDIR\Sysnative\taskkill.exe"
          StrCpy $2 "$WINDIR\Sysnative\taskkill.exe"
        ${Else}
          StrCpy $2 "$SYSDIR\taskkill.exe"
        ${EndIf}
        ${If} ${FileExists} "$2"
          nsExec::ExecToLog '"$2" /F /T /IM "calculator.exe"'
          Pop $1
          Sleep 300
        ${EndIf}
      ${EndIf}
      Delete "$INSTDIR\$0"
      ${If} ${FileExists} "$INSTDIR\$0"
        Delete "$INSTDIR\$0.old"
        ClearErrors
        Rename "$INSTDIR\$0" "$INSTDIR\$0.old"
        ${IfNot} ${Errors}
          Delete "$INSTDIR\$0.old"
        ${EndIf}
      ${EndIf}
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $1
FunctionEnd

Function ${UN}SafeAddDirToPath
  Exch $R0 ; directory to add
  Push $0
  Push $1
  Push $2
  ClearErrors
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} ${Errors}
    ; Value is missing OR >1023 chars (ERROR_MORE_DATA): handle safely in PS.
    Goto ${UN}add_path_ps
  ${EndIf}
  ${If} $0 == ""
    WriteRegExpandStr HKCU "Environment" "Path" "$R0"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
    Goto ${UN}add_path_done
  ${EndIf}
  StrLen $2 "$0"
  ${If} $2 >= 1000
    Goto ${UN}add_path_ps
  ${EndIf}
  ${WordAdd} "$0" ";" "E+$R0" $1
  ${If} $0 != $1
    WriteRegExpandStr HKCU "Environment" "Path" "$1"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
  Goto ${UN}add_path_done

${UN}add_path_ps:
  System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_PATH_DIR", t "$R0")'
  nsExec::ExecToLog /TIMEOUT=15000 `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$d = $$env:SPID3R_PATH_DIR.TrimEnd([char]92); $$cur = [Environment]::GetEnvironmentVariable('Path', 'User'); $$parts = @(); if ($$cur) { $$parts = $$cur -split ';' | Where-Object { $$_ -ne '' -and $$_.TrimEnd([char]92) -ine $$d } }; $$parts += $$d; [Environment]::SetEnvironmentVariable('Path', ($$parts -join ';'), 'User')"`
  Pop $1
  System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_PATH_DIR", p 0)'

${UN}add_path_done:
  Pop $2
  Pop $1
  Pop $0
  Pop $R0
FunctionEnd

Function ${UN}SafeRemoveDirFromPath
  Exch $R0 ; directory to remove
  Push $0
  Push $1
  Push $2
  ClearErrors
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} ${Errors}
    Goto ${UN}rem_path_ps
  ${EndIf}
  ${If} $0 == ""
    Goto ${UN}rem_path_done
  ${EndIf}
  StrLen $2 "$0"
  ${If} $2 >= 1000
    Goto ${UN}rem_path_ps
  ${EndIf}
  ${WordAdd} "$0" ";" "E-$R0" $1
  ${WordAdd} "$1" ";" "E-$R0\" $2
  ${If} $0 != $2
    WriteRegExpandStr HKCU "Environment" "Path" "$2"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000
  ${EndIf}
  Goto ${UN}rem_path_done

${UN}rem_path_ps:
  System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_PATH_DIR", t "$R0")'
  nsExec::ExecToLog /TIMEOUT=15000 `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$d = $$env:SPID3R_PATH_DIR.TrimEnd([char]92); $$cur = [Environment]::GetEnvironmentVariable('Path', 'User'); if ($$cur) { $$parts = $$cur -split ';' | Where-Object { $$_ -ne '' -and $$_.TrimEnd([char]92) -ine $$d }; [Environment]::SetEnvironmentVariable('Path', ($$parts -join ';'), 'User') }"`
  Pop $1
  System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_PATH_DIR", p 0)'

${UN}rem_path_done:
  Pop $2
  Pop $1
  Pop $0
  Pop $R0
FunctionEnd

Function ${UN}SafeAddToPath
  Push "$INSTDIR"
  Call ${UN}SafeAddDirToPath
  Push "$LOCALAPPDATA\Spid3r\bin"
  Call ${UN}SafeAddDirToPath
FunctionEnd

Function ${UN}SafeRemoveFromPath
  Push "$INSTDIR"
  Call ${UN}SafeRemoveDirFromPath
  Push "$LOCALAPPDATA\Spid3r\bin"
  Call ${UN}SafeRemoveDirFromPath
FunctionEnd
!macroend

!insertmacro DEFINE_FILE_HELPERS ""
!insertmacro DEFINE_FILE_HELPERS "un."

Function PrepareInstDir
  ${If} ${FileExists} "$INSTDIR\*.*"
    DetailPrint "Preparing destination directory $INSTDIR..."
    Delete "$INSTDIR\*.old"
    Delete "$INSTDIR\*.tmp"
    RMDir /r "$INSTDIR\stdlib"
    RMDir /r "$INSTDIR\docs"
    RMDir /r "$INSTDIR\examples"
    RMDir /r "$INSTDIR\benchmarks"
    StrCpy $0 "spid3r.exe"
    Call ForceDeletePath
    StrCpy $0 "sp.exe"
    Call ForceDeletePath
    StrCpy $0 "yamc.exe"
    Call ForceDeletePath
    StrCpy $0 "yamcvm.exe"
    Call ForceDeletePath
    StrCpy $0 "spvm.exe"
    Call ForceDeletePath
    StrCpy $0 "spkg.exe"
    Call ForceDeletePath
    StrCpy $0 "spid3r-self.exe"
    Call ForceDeletePath
    StrCpy $0 "spid3r-control.exe"
    Call ForceDeletePath
    StrCpy $0 "calculator.exe"
    Call ForceDeletePath
    StrCpy $0 "LLVM-C.dll"
    Call ForceDeletePath
    StrCpy $0 "LLVM-C.lib"
    Call ForceDeletePath
    StrCpy $0 "spid3r_lang.lib"
    Call ForceDeletePath
    StrCpy $0 "spid3r_lang.lib.sha256"
    Call ForceDeletePath
    StrCpy $0 "runtime-manifest.json"
    Call ForceDeletePath
    StrCpy $0 "uninstall.exe"
    Call ForceDeletePath
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------ sections -----
Section "Spid3r compiler & runtime (required)" SecCore
  SectionIn RO
  StrCpy $PathAdded "0"
  StrCpy $FinishPathSummary " (user PATH was not modified)."
  StrCpy $FinishRunHint 'Open "Spid3r Command Shell" from Start Menu, or run: "$INSTDIR\spid3r.exe" version'

  ; Terminate any running old binaries and clear legacy artifacts before writing.
  Call PrepareInstDir

  SetOutPath "$INSTDIR"
  File "${SPID3R_EXE}"
  File "/oname=sp.exe" "${SPID3R_EXE}"
  File "/oname=yamc.exe" "${SPID3R_EXE}"
  File "/oname=yamcvm.exe" "${SPID3R_EXE}"
  File "/oname=spvm.exe" "${SPID3R_EXE}"
  File "/oname=spkg.exe" "${SPID3R_EXE}"
!ifdef SPID3R_SELF_EXE
  File "/oname=spid3r-self.exe" "${SPID3R_SELF_EXE}"
!endif
!ifdef CONTROL_EXE
  File "${CONTROL_EXE}"
!endif
!if ${WITH_NATIVE} == 1
  File "${LLVM_DLL}"
  File "${LLVM_LIB}"
  File "${RUNTIME_LIB}"
!ifdef RUNTIME_SHA256
  File "/oname=spid3r_lang.lib.sha256" "${RUNTIME_SHA256}"
!endif
!ifdef RUNTIME_MANIFEST
  File "/oname=runtime-manifest.json" "${RUNTIME_MANIFEST}"
!endif
!endif
!if ${WITH_GUI} == 1
!if /FileExists "${CALCULATOR_EXE}"
  File "${CALCULATOR_EXE}"
!endif
!endif
  File "assets\spid3r.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Write the default spid3r.ini into %APPDATA%\Spid3r so the machine starts
  ; with a visible, editable configuration file (Runtime Configuration).
  ; Idempotent: `control ini --init` refuses to overwrite an existing ini.
  nsExec::ExecToLog '"$INSTDIR\spid3r.exe" control ini --init'
  Pop $1

  WriteRegStr   HKCU "Software\Spid3r" "InstallDir" "$INSTDIR"
  WriteRegDWORD HKCU "Software\Spid3r" "AddedToPath" 0

  ; --- Start Menu shortcuts ---
  CreateDirectory "$SMPROGRAMS\Spid3r"
  CreateShortCut "$SMPROGRAMS\Spid3r\Spid3r Command Shell.lnk" "$SYSDIR\cmd.exe" '/K "set $\"PATH=$INSTDIR;$LOCALAPPDATA\Spid3r\bin;%PATH%$\" & spid3r version & echo Welcome to Spid3r! Type spid3r --help for commands."' "$INSTDIR\spid3r.ico" 0
!ifdef CONTROL_EXE
  CreateShortCut "$SMPROGRAMS\Spid3r\Spid3r Control Panel.lnk" "$INSTDIR\spid3r-control.exe" "" "$INSTDIR\spid3r.ico" 0
!endif
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
  SetRebootFlag false
SectionEnd

Section "Add Spid3r to PATH (recommended for cmd / PowerShell)" SecPath
  Call SafeAddToPath
  WriteRegDWORD HKCU "Software\Spid3r" "AddedToPath" 1
  StrCpy $PathAdded "1"
  StrCpy $FinishPathSummary " and added to your user PATH."
  StrCpy $FinishRunHint "Run in a NEW terminal window: spid3r version"
SectionEnd

; ------------------------------------------------------- prerequisites -----
!if ${WITH_GUI} == 1
; Registry keys from Microsoft's WebView2 distribution guide; the 32-bit
; registry view maps HKLM\SOFTWARE to WOW6432Node on 64-bit Windows.
!define WEBVIEW2_KEY "Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WEBVIEW2_URL "https://go.microsoft.com/fwlink/p/?LinkId=2124703"

Section "WebView2 runtime (for spid3r gui; only if missing)" SecWebView2
  ; First check 32-bit / WOW6432Node registry view across HKLM and HKCU
  SetRegView 32
  ReadRegStr $0 HKLM "SOFTWARE\${WEBVIEW2_KEY}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Goto webview2_found
  ${EndIf}
  ReadRegStr $0 HKCU "Software\${WEBVIEW2_KEY}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Goto webview2_found
  ${EndIf}

  ; Check native 64-bit registry view across HKLM and HKCU
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\${WEBVIEW2_KEY}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Goto webview2_found
  ${EndIf}
  ReadRegStr $0 HKCU "Software\${WEBVIEW2_KEY}" "pv"
  ${If} $0 != ""
  ${AndIf} $0 != "0.0.0.0"
    Goto webview2_found
  ${EndIf}

  ; Missing WebView2 runtime: install bundled or download bootstrapper
  Goto webview2_install

webview2_found:
  SetRegView default
  DetailPrint "WebView2 runtime $0 is already installed."
  Goto webview2_done

webview2_install:
  SetRegView default
!ifdef WEBVIEW2_SETUP
  DetailPrint "Installing bundled Microsoft Edge WebView2 runtime..."
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "/oname=MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2_SETUP}"
  ExecWait '"$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe" /silent /install' $1
  StrCmp $1 0 webview2_installed
  DetailPrint "Bundled WebView2 setup exited with code $1; attempting online download..."
!endif

  ; If not bundled or bundled setup failed: download bootstrapper and install
  DetailPrint "Downloading Microsoft Edge WebView2 bootstrapper from Microsoft..."
  Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"

  ; 1. Try curl via 64-bit Sysnative, 32-bit Sysdir, or PATH
  IfFileExists "$WINDIR\Sysnative\curl.exe" 0 +3
    nsExec::ExecToLog '"$WINDIR\Sysnative\curl.exe" -fsSL --retry 3 -o "$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2_URL}"'
    Goto webview2_check_curl
  IfFileExists "$SYSDIR\curl.exe" 0 +3
    nsExec::ExecToLog '"$SYSDIR\curl.exe" -fsSL --retry 3 -o "$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2_URL}"'
    Goto webview2_check_curl
  nsExec::ExecToLog 'curl.exe -fsSL --retry 3 -o "$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2_URL}"'

webview2_check_curl:
  Pop $1
  ${If} $1 == 0
  ${AndIf} ${FileExists} "$TEMP\MicrosoftEdgeWebview2Setup.exe"
    Goto webview2_exec_dl
  ${EndIf}

  ; 2. Fallback: PowerShell download with TLS 1.2
  DetailPrint "Downloading WebView2 bootstrapper via PowerShell..."
  Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
  nsExec::ExecToLog `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; (New-Object System.Net.WebClient).DownloadFile('${WEBVIEW2_URL}', '$TEMP\MicrosoftEdgeWebview2Setup.exe')"`
  Pop $1
  ${If} $1 == 0
  ${AndIf} ${FileExists} "$TEMP\MicrosoftEdgeWebview2Setup.exe"
    Goto webview2_exec_dl
  ${EndIf}
  Goto webview2_failed

webview2_exec_dl:
  DetailPrint "Installing downloaded Microsoft Edge WebView2 runtime..."
  ExecWait '"$TEMP\MicrosoftEdgeWebview2Setup.exe" /silent /install' $1
  Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
  StrCmp $1 0 webview2_installed
  DetailPrint "WebView2 installer exited with code $1."
  Goto webview2_failed

webview2_installed:
  DetailPrint "WebView2 runtime installation complete."
  Goto webview2_done

webview2_failed:
  Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
  DetailPrint "WebView2 setup could not be completed automatically; spid3r gui requires it (https://developer.microsoft.com/microsoft-edge/webview2/)."
  MessageBox MB_OK|MB_ICONEXCLAMATION "The Microsoft Edge WebView2 runtime could not be installed automatically.$\r$\n$\r$\nspid3r gui programs need it. You can install it manually from:$\r$\nhttps://developer.microsoft.com/microsoft-edge/webview2/" /SD IDOK

webview2_done:
  SetOutPath "$INSTDIR"
SectionEnd
!endif

; No documentation, examples or benchmark component: the installer carries the
; toolchain and nothing else. The developer guide, playground and examples live
; in the repository and at DOCSURL, so shipping a second copy in Program Files
; only meant stale docs on disk and 100+ extra files to uninstall.

Section /o "Desktop shortcuts" SecDesktop
  CreateShortCut "$DESKTOP\Spid3r Shell.lnk" "$SYSDIR\cmd.exe" '/K "set $\"PATH=$INSTDIR;$LOCALAPPDATA\Spid3r\bin;%PATH%$\" & spid3r version"' "$INSTDIR\spid3r.ico" 0
!if ${WITH_GUI} == 1
!if /FileExists "${CALCULATOR_EXE}"
  CreateShortCut "$DESKTOP\Spid3r Calculator.lnk" "$INSTDIR\calculator.exe" "" "$INSTDIR\spid3r.ico" 0
!endif
!endif
SectionEnd

Section "Associate .matrix and .mt files with Spid3r" SecAssoc
  WriteRegStr HKCU "Software\Classes\.matrix" "" "Spid3r.Matrix"
  WriteRegStr HKCU "Software\Classes\.matrix" "Content Type" "text/plain"
  WriteRegStr HKCU "Software\Classes\.matrix" "PerceivedType" "text"
  WriteRegStr HKCU "Software\Classes\.mt" "" "Spid3r.Matrix"
  WriteRegStr HKCU "Software\Classes\.mt" "Content Type" "text/plain"
  WriteRegStr HKCU "Software\Classes\.mt" "PerceivedType" "text"
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

!ifdef HAVE_FORMATTER_VSIX
; Trim $0 at the first CR or LF so multi-line `where.exe` output yields only the first hit.
Function FirstLine
  Push $1
  Push $2
  Push $3
  StrLen $1 $0
  StrCpy $2 0
  ${DoWhile} $2 < $1
    StrCpy $3 $0 1 $2
    ${If} $3 == "$\r"
    ${OrIf} $3 == "$\n"
      StrCpy $0 $0 $2
      ${ExitDo}
    ${EndIf}
    IntOp $2 $2 + 1
  ${Loop}
  Pop $3
  Pop $2
  Pop $1
FunctionEnd

; Install the bundled offline VSIX into one detected editor CLI ($R0 = code|cursor|windsurf|codium).
; Expects $R7 = native where.exe path, $R8 = native cmd.exe path; increments $R9 if CLI was found.
Function InstallEditorVsix
  Push $0
  Push $1

  ; Search ONLY PATH (`$$PATH:` prevents `where.exe` from searching CWD).
  nsExec::ExecToStack '"$R7" "$$PATH:$R0.cmd"'
  Pop $1
  Pop $0
  ${If} $1 == 0
    Call FirstLine
    ${If} $0 != ""
    ${AndIf} ${FileExists} "$0"
      IntOp $R9 $R9 + 1
      DetailPrint "Installing Spid3r IDE extension via $0..."
      ; Pass paths via env vars + cmd /d /v:on so spaces/&/AutoRun cannot inject into cmd.
      System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_EDITOR_CLI", t r0)'
      System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_VSIX_PATH", t "$INSTDIR\editor\spid3r-formatter.vsix")'
      nsExec::ExecToLog /TIMEOUT=60000 '"$R8" /d /v:on /c "call "!SPID3R_EDITOR_CLI!" --install-extension "!SPID3R_VSIX_PATH!" --force"'
      Pop $1
      System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_EDITOR_CLI", p 0)'
      System::Call 'Kernel32::SetEnvironmentVariable(t "SPID3R_VSIX_PATH", p 0)'
      ${If} $1 == 0
        DetailPrint "Installed Spid3r IDE extension for $R0."
      ${Else}
        DetailPrint "Note: $R0 CLI exited with '$1'; offline VSIX remains at $INSTDIR\editor\spid3r-formatter.vsix."
      ${EndIf}
      Goto editor_cli_done
    ${EndIf}
  ${EndIf}

  nsExec::ExecToStack '"$R7" "$$PATH:$R0.exe"'
  Pop $1
  Pop $0
  ${If} $1 == 0
    Call FirstLine
    ${If} $0 != ""
    ${AndIf} ${FileExists} "$0"
      IntOp $R9 $R9 + 1
      DetailPrint "Installing Spid3r IDE extension via $0..."
      nsExec::ExecToLog /TIMEOUT=60000 '"$0" --install-extension "$INSTDIR\editor\spid3r-formatter.vsix" --force'
      Pop $1
      ${If} $1 == 0
        DetailPrint "Installed Spid3r IDE extension for $R0."
      ${Else}
        DetailPrint "Note: $R0 CLI exited with '$1'; offline VSIX remains at $INSTDIR\editor\spid3r-formatter.vsix."
      ${EndIf}
    ${EndIf}
  ${EndIf}

editor_cli_done:
  Pop $1
  Pop $0
FunctionEnd

Section "IDE formatter extension (VS Code, Cursor, Windsurf, VSCodium)" SecEditorExt
  SetOutPath "$INSTDIR\editor"
  File "/oname=spid3r-formatter.vsix" "${FORMATTER_VSIX}"
  ; Restore working directory to the trusted $INSTDIR before running external tools.
  SetOutPath "$INSTDIR"

  ClearErrors
  ${GetParameters} $R1
  ${GetOptions} "$R1" "/NOEDITOR" $R2
  ${IfNot} ${Errors}
    DetailPrint "Skipping automatic IDE extension install (/NOEDITOR); offline VSIX saved at $INSTDIR\editor\spid3r-formatter.vsix."
    Goto editor_ext_done
  ${EndIf}

  ReadEnvStr $R2 "SPID3R_NO_EDITOR_EXT"
  ${If} $R2 == "1"
    DetailPrint "Skipping automatic IDE extension install (SPID3R_NO_EDITOR_EXT=1); offline VSIX saved at $INSTDIR\editor\spid3r-formatter.vsix."
    Goto editor_ext_done
  ${EndIf}

  ${If} ${FileExists} "$WINDIR\Sysnative\where.exe"
    StrCpy $R7 "$WINDIR\Sysnative\where.exe"
  ${Else}
    StrCpy $R7 "$SYSDIR\where.exe"
  ${EndIf}
  ${If} ${FileExists} "$WINDIR\Sysnative\cmd.exe"
    StrCpy $R8 "$WINDIR\Sysnative\cmd.exe"
  ${Else}
    StrCpy $R8 "$SYSDIR\cmd.exe"
  ${EndIf}

  ${IfNot} ${FileExists} "$R7"
    DetailPrint "where.exe not found; offline VSIX saved at $INSTDIR\editor\spid3r-formatter.vsix."
    Goto editor_ext_done
  ${EndIf}

  StrCpy $R9 0
  StrCpy $R0 "code"
  Call InstallEditorVsix
  StrCpy $R0 "cursor"
  Call InstallEditorVsix
  StrCpy $R0 "windsurf"
  Call InstallEditorVsix
  StrCpy $R0 "codium"
  Call InstallEditorVsix

  ${If} $R9 == 0
    DetailPrint "No supported editor CLI (code, cursor, windsurf, codium) found on PATH. Offline VSIX saved at: $INSTDIR\editor\spid3r-formatter.vsix"
  ${EndIf}

editor_ext_done:
SectionEnd
!endif

; --------------------------------------------------------- descriptions ----
!if ${WITH_NATIVE} == 1
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: spid3r, sp, yamc, yamcvm, spvm, spkg, YamLLVM native AOT, and standalone calculator app."
!else if ${WITH_GUI} == 1
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: spid3r, sp, yamc, yamcvm, spvm, spkg, the Spid3r Control Panel, and standalone calculator app."
!else
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: spid3r, sp, yamc, yamcvm, spvm, and spkg zero-dependency core binaries."
!endif
LangString DESC_SecPath ${LANG_ENGLISH} "Adds $INSTDIR and $LOCALAPPDATA\Spid3r\bin to your current user PATH so spid3r, spkg, and installed CLI tools work directly in new cmd and PowerShell windows."
LangString DESC_SecDesktop ${LANG_ENGLISH} "Creates convenient shortcuts on your desktop for quick terminal access and the standalone calculator."
LangString DESC_SecAssoc ${LANG_ENGLISH} "Associates .matrix and .mt source files with Spid3r and registers Explorer context menu actions (Run, Check syntax, GUI)."
!ifdef HAVE_FORMATTER_VSIX
LangString DESC_SecEditorExt ${LANG_ENGLISH} "Bundles the offline spid3r-formatter.vsix extension and installs it automatically for detected VS Code, Cursor, Windsurf, or VSCodium editors on PATH."
!endif
!if ${WITH_GUI} == 1
LangString DESC_SecWebView2 ${LANG_ENGLISH} "Installs the Microsoft Edge WebView2 runtime that spid3r gui windows use, only if it is missing (Windows 11 and Microsoft Edge already include it)."
!endif

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}     $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecPath}     $(DESC_SecPath)
!if ${WITH_GUI} == 1
  !insertmacro MUI_DESCRIPTION_TEXT ${SecWebView2} $(DESC_SecWebView2)
!endif
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop}  $(DESC_SecDesktop)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAssoc}    $(DESC_SecAssoc)
!ifdef HAVE_FORMATTER_VSIX
  !insertmacro MUI_DESCRIPTION_TEXT ${SecEditorExt} $(DESC_SecEditorExt)
!endif
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ------------------------------------------------- page callbacks & init ---
; Placed after Sections so `${SecPath}` is defined for Select/UnselectSection.
Function .onInit
  ClearErrors
  ${GetParameters} $0
  ${GetOptions} "$0" "/NOPATH" $1
  ${IfNot} ${Errors}
    !insertmacro UnselectSection ${SecPath}
    Return
  ${EndIf}
  ReadEnvStr $1 "SPID3R_NO_PATH"
  ${If} $1 == "1"
    !insertmacro UnselectSection ${SecPath}
  ${EndIf}
FunctionEnd

Function WelcomeShow
  ${NSD_CreateCheckbox} 120u 154u 195u 16u "Add Spid3r to PATH (cmd / PowerShell / terminal)"
  Pop $WelcomePathCheckbox
  SetCtlColors $WelcomePathCheckbox "" "${MUI_BGCOLOR}"
  ; Ensure checkbox renders above the Welcome text label at all DPI scales.
  System::Call 'user32::SetWindowPos(p $WelcomePathCheckbox, p 0, i 0, i 0, i 0, i 0, i 3)'
  ${If} ${SectionIsSelected} ${SecPath}
    ${NSD_Check} $WelcomePathCheckbox
  ${Else}
    ${NSD_Uncheck} $WelcomePathCheckbox
  ${EndIf}
FunctionEnd

Function WelcomeLeave
  ${NSD_GetState} $WelcomePathCheckbox $0
  ${If} $0 == ${BST_CHECKED}
    !insertmacro SelectSection ${SecPath}
  ${Else}
    !insertmacro UnselectSection ${SecPath}
  ${EndIf}
FunctionEnd

; ------------------------------------------------------------ uninstall ----
Section "Uninstall"
  ; Move current working directory out of $INSTDIR BEFORE deleting files or
  ; removing $INSTDIR, otherwise Windows holds a directory lock on $INSTDIR.
  SetOutPath "$TEMP"

  ; --- remove the PATH entry (even if PATH > 1024 chars or trailing '\') ---
  Call un.SafeRemoveFromPath

  ; --- file association ---
  DeleteRegKey HKCU "Software\Classes\.matrix"
  DeleteRegKey HKCU "Software\Classes\.mt"
  DeleteRegKey HKCU "Software\Classes\Spid3r.Matrix"
  System::Call 'shell32::SHChangeNotify(l 0x08000000, l 0, p 0, p 0)'

  ; --- Start Menu & Desktop shortcuts ---
  RMDir /r "$SMPROGRAMS\Spid3r"
  Delete "$DESKTOP\Spid3r Shell.lnk"
  Delete "$DESKTOP\Spid3r Calculator.lnk"

  ; --- uninstall entry + install record ---
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "Software\Spid3r"

  ; --- stop running binaries first, then delete all files & legacy artifacts ---
  Call un.StopSpid3rProcesses

  StrCpy $0 "spid3r.exe"
  Call un.ForceDeletePath
  StrCpy $0 "sp.exe"
  Call un.ForceDeletePath
  StrCpy $0 "yamc.exe"
  Call un.ForceDeletePath
  StrCpy $0 "yamcvm.exe"
  Call un.ForceDeletePath
  StrCpy $0 "spvm.exe"
  Call un.ForceDeletePath
  StrCpy $0 "spkg.exe"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r-self.exe"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r-control.exe"
  Call un.ForceDeletePath
  StrCpy $0 "calculator.exe"
  Call un.ForceDeletePath
  StrCpy $0 "LLVM-C.dll"
  Call un.ForceDeletePath
  StrCpy $0 "LLVM-C.lib"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r_lang.lib"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r_lang.lib.sha256"
  Call un.ForceDeletePath
  StrCpy $0 "runtime-manifest.json"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r.lib"
  Call un.ForceDeletePath
  StrCpy $0 "spid3r.ico"
  Call un.ForceDeletePath
  StrCpy $0 "README.md"
  Call un.ForceDeletePath

  Delete "$INSTDIR\editor\spid3r-formatter.vsix"
  RMDir "$INSTDIR\editor"
  Delete "$INSTDIR\*.old"
  Delete "$INSTDIR\*.tmp"
  Delete "$INSTDIR\uninstall.exe"

  ; README.md, docs, examples, benchmarks and stdlib/ are no longer installed,
  ; but an install made by an earlier version still has them: remove them on
  ; uninstall so an upgrade-then-uninstall does not leave orphan files behind.
  RMDir /r "$INSTDIR\docs"
  RMDir /r "$INSTDIR\stdlib"
  RMDir /r "$INSTDIR\examples"
  RMDir /r "$INSTDIR\benchmarks"
  RMDir /r "$INSTDIR\editor"

  RMDir "$INSTDIR"
  RMDir /r "$LOCALAPPDATA\Spid3r"
  SetRebootFlag false
SectionEnd
