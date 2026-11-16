; The old Electron build (v1.x) registered its autostart under Electron's default name.
; Remove it on install so the old exe stops launching next to this one.
!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "electron.app.Electron"
  StrCpy $1 $0 "" -15
  StrCmp $1 'StandBuddy.exe"' 0 +2
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "electron.app.Electron"
  StrCpy $1 $0 "" -14
  StrCmp $1 "StandBuddy.exe" 0 +2
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "electron.app.Electron"
!macroend

; "Launch at startup" writes this value; don't leave it pointing at a deleted exe.
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "StandBuddy"
!macroend
