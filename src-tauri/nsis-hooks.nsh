; NSIS hooks for the Windows installer.
; Referenced by tauri.conf.json -> bundle.windows.nsis.installerHooks.

; A Start-menu entry next to the app shortcut, so uninstalling is one click
; without going through Settings.
!macro NSIS_HOOK_POSTINSTALL
  CreateShortcut "$SMPROGRAMS\Uninstall ${PRODUCTNAME}.lnk" "$INSTDIR\uninstall.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$SMPROGRAMS\Uninstall ${PRODUCTNAME}.lnk"
!macroend

; The configuration folder lives in the user's home (~/.llm_lite_proxy), which
; is outside the bundle-id directories the built-in "delete app data" checkbox
; clears, so the same checkbox has to reach it too.
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RmDir /r "$PROFILE\.llm_lite_proxy"
  ${EndIf}
!macroend
