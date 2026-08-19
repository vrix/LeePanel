!macro NSIS_HOOK_PREUNINSTALL
  ${If} ${FileExists} "$INSTDIR\resources\unregister_leepanel_mcp.ps1"
    nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\resources\unregister_leepanel_mcp.ps1" -LeePanelPath "$INSTDIR\leepanel.exe"'
  ${ElseIf} ${FileExists} "$INSTDIR\unregister_leepanel_mcp.ps1"
    nsExec::ExecToLog 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\unregister_leepanel_mcp.ps1" -LeePanelPath "$INSTDIR\leepanel.exe"'
  ${EndIf}
!macroend
