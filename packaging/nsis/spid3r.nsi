; ============================================================================
;  Spid3r — Windows installer (NSIS 3.x)
;
;  Per-user install (no administrator prompt):
;    * installs the toolchain to %LOCALAPPDATA%\Programs\Spid3r
;    * adds that directory to the CURRENT USER's PATH (new cmd/PowerShell
;      windows can run `spid3r` directly)
;    * registers an uninstaller in "Apps & features"
;    * optional: examples + docs, and a .matrix file association
;
;  Build:  makensis spid3r.nsi      (from this directory)
;  Test:   Spid3r-Setup-<ver>.exe /S /D=C:\some\dir
; ============================================================================

Unicode true
!include "MUI2.nsh"
!include "WordFunc.nsh"

!define APPNAME        "Spid3r"
!define APPVERSION     "0.1.0"
!define PUBLISHER      "Spid3r"
!define UNINSTKEY      "Software\Microsoft\Windows\CurrentVersion\Uninstall\Spid3r"
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
BrandingText "Spid3r — pure Rust, no runtime dependencies"

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
!define MUI_FINISHPAGE_TITLE "Spid3r is installed"
!define MUI_FINISHPAGE_TEXT "Open a NEW terminal (cmd or PowerShell) and run:$\r$\n$\r$\n    spid3r version$\r$\n    spid3r run hello.matrix$\r$\n$\r$\nThe installer added$\r$\n$INSTDIR$\r$\nto your user PATH. Windows only picks that up in terminals started after installation."
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\doc.html"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Show the Spid3r developer documentation"
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ------------------------------------------------------------ sections -----
Section "Spid3r compiler & runtime (required)" SecCore
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "payload\spid3r.exe"
  File "assets\spid3r.ico"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; --- add to the current user's PATH (deduplicated) ---
  ReadRegStr $R0 HKCU "Environment" "Path"
  ${WordAdd} "$R0" ";" "E+$INSTDIR" $R1
  StrCmp "$R0" "$R1" +2
    WriteRegExpandStr HKCU "Environment" "Path" "$R1"
  StrCmp "$R0" "$R1" +2
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=5000

  WriteRegStr HKCU "Software\Spid3r" "InstallDir" "$INSTDIR"

  ; --- Apps & features entry ---
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName"     "${APPNAME} ${APPVERSION} — programming language"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion"  "${APPVERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher"       "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon"     "$INSTDIR\spid3r.ico"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
SectionEnd

Section "Examples, documentation & reference" SecDocs
  SetOutPath "$INSTDIR"
  File "payload\README.md"
  File "payload\doc.html"
  SetOutPath "$INSTDIR\docs"
  File /r "payload\docs\*.*"
  SetOutPath "$INSTDIR\examples"
  File /r "payload\examples\*.*"
SectionEnd

Section "Associate .matrix files with Spid3r" SecAssoc
  WriteRegStr HKCU "Software\Classes\.matrix" "" "Spid3r.Matrix"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix" "" "Spid3r program"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\DefaultIcon" "" "$INSTDIR\spid3r.ico"
  WriteRegStr HKCU "Software\Classes\Spid3r.Matrix\shell\open\command" "" '"$INSTDIR\spid3r.exe" run "%1"'
  System::Call 'shell32::SHChangeNotify(l 0x08000000, l 0, p 0, p 0)'
SectionEnd

; --------------------------------------------------------- descriptions ----
LangString DESC_SecCore  ${LANG_ENGLISH} "The spid3r toolchain: run / compile / exec / check / test / fmt / debug / repl, plus automatic PATH registration."
LangString DESC_SecDocs  ${LANG_ENGLISH} "The 23 example programs, the documentation folder, and the self-contained developer reference (doc.html)."
LangString DESC_SecAssoc ${LANG_ENGLISH} "Double-clicking a .matrix source file runs it with ``spid3r run``."

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecCore}  $(DESC_SecCore)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDocs}  $(DESC_SecDocs)
  !insertmacro MUI_DESCRIPTION_TEXT ${SecAssoc} $(DESC_SecAssoc)
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

  ; --- uninstall entry + install record ---
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "Software\Spid3r"

  ; --- files ---
  Delete "$INSTDIR\spid3r.exe"
  Delete "$INSTDIR\spid3r.ico"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\doc.html"
  Delete "$INSTDIR\uninstall.exe"
  RMDir /r "$INSTDIR\docs"
  RMDir /r "$INSTDIR\examples"
  RMDir "$INSTDIR"
SectionEnd
