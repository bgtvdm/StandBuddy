; StandBuddy installer — per-user, no admin. Built by installer/build.ps1:
;   makensis /DVERSION=<x.y.z> /DEXE=<path to standbuddy.exe> /DOUTFILE=<setup.exe> standbuddy.nsi
; Registry keys and install folder match the v2.x (Tauri) installer, so this one upgrades it
; in place and keeps its install location (e.g. /D=E:\StandBuddy).
; Silent: setup.exe /S [/D=<dir>]   (/D must be last and unquoted, as in every NSIS installer)

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
ManifestDPIAware true

!define PRODUCT "StandBuddy"
!define PUBLISHER "bgtvdm"
!define EXENAME "standbuddy.exe"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
!define DIRKEY "Software\${PUBLISHER}\${PRODUCT}"
!define RUNKEY "Software\Microsoft\Windows\CurrentVersion\Run"
!ifndef VERSION
  !error "pass /DVERSION=x.y.z"
!endif
!ifndef EXE
  !error "pass /DEXE=<path to standbuddy.exe>"
!endif
!ifndef OUTFILE
  !define OUTFILE "${PRODUCT}_${VERSION}_x64-setup.exe"
!endif

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "FileFunc.nsh"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
; Replaced in .onInit unless /D= was given (NSIS sets $INSTDIR from /D before .onInit).
!define PLACEHOLDER "placeholder\${PRODUCT}"
InstallDir "${PLACEHOLDER}"
BrandingText "${PRODUCT} ${VERSION}"

!searchparse /noerrors "${VERSION}" "" V1 "." V2 "." V3
!searchparse /noerrors "${V3}" "" V3 "-"
VIProductVersion "${V1}.${V2}.${V3}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "MIT"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

!define MUI_ICON "..\assets\tray.ico"
!define MUI_UNICON "..\assets\tray.ico"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Start ${PRODUCT}"
!define MUI_FINISHPAGE_RUN_FUNCTION StartApp

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  SetShellVarContext current
  ${If} $INSTDIR == "${PLACEHOLDER}"
    ReadRegStr $0 HKCU "${DIRKEY}" ""
    ${If} $0 != ""
      StrCpy $INSTDIR $0
    ${Else}
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCT}"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function StartApp
  Exec '"$INSTDIR\${EXENAME}"'
FunctionEnd

; The exe can't be replaced or removed while the app runs. Silent: stop it (that is what
; a scripted update wants). Otherwise ask the user to quit it from the tray.
!macro StopRunningApp UN
Function ${UN}StopRunningApp
  retry:
  ClearErrors
  ${If} ${FileExists} "$INSTDIR\${EXENAME}"
    FileOpen $0 "$INSTDIR\${EXENAME}" a
    ${IfNot} ${Errors}
      FileClose $0
      Return
    ${EndIf}
    ${If} ${Silent}
      ; Only the copy in $INSTDIR; the path travels in an env var, so no quoting issues.
      ; CIM, not Get-Process: nsExec starts the 32-bit PowerShell, which can't read a
      ; 64-bit process's .Path.
      System::Call 'kernel32::SetEnvironmentVariable(t "SB_EXE", t "$INSTDIR\${EXENAME}")i'
      nsExec::Exec 'powershell.exe -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process | Where-Object { $$_.ExecutablePath -eq $$env:SB_EXE } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }"'
      Pop $0
      Sleep 1000
      FileOpen $0 "$INSTDIR\${EXENAME}" a
      ${IfNot} ${Errors}
        FileClose $0
        Return
      ${EndIf}
      Abort "${PRODUCT} is running and could not be stopped."
    ${EndIf}
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${PRODUCT} is running. Quit it from its tray icon, then select Retry." IDRETRY retry
    Abort
  ${EndIf}
FunctionEnd
!macroend
!insertmacro StopRunningApp ""
!insertmacro StopRunningApp "un."

Section "Install"
  SetOutPath $INSTDIR
  Call StopRunningApp
  File "/oname=${EXENAME}" "${EXE}"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  WriteRegStr HKCU "${DIRKEY}" "" $INSTDIR
  WriteRegStr HKCU "${UNINSTKEY}" "MainBinaryName" "${EXENAME}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" '"$INSTDIR\${EXENAME}"'
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" '"$INSTDIR"'
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINSTKEY}" "EstimatedSize" $0

  CreateShortcut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\${EXENAME}"

  ; The Electron build (v1.x) registered its autostart under Electron's default name.
  ; Remove it so the old exe stops launching next to this one.
  ReadRegStr $0 HKCU "${RUNKEY}" "electron.app.Electron"
  StrCpy $1 $0 "" -15
  StrCmp $1 'StandBuddy.exe"' 0 +2
  DeleteRegValue HKCU "${RUNKEY}" "electron.app.Electron"
  StrCpy $1 $0 "" -14
  StrCmp $1 "StandBuddy.exe" 0 +2
  DeleteRegValue HKCU "${RUNKEY}" "electron.app.Electron"
SectionEnd

Function un.onInit
  SetShellVarContext current
FunctionEnd

; Settings and stats in %APPDATA%\standbuddy are kept; only the app is removed.
Section "Uninstall"
  Call un.StopRunningApp
  Delete "$INSTDIR\${EXENAME}"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  Delete "$DESKTOP\${PRODUCT}.lnk"
  ; "Launch at startup" must not point at a deleted exe.
  DeleteRegValue HKCU "${RUNKEY}" "${PRODUCT}"
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "${DIRKEY}"
  DeleteRegKey /ifempty HKCU "Software\${PUBLISHER}"
SectionEnd
