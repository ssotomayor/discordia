; One icon at the top level, no "Uninstall" entry and no folder: Windows offers
; Uninstall from the icon's context menu through the registry key just written.
Section "-Start Menu"
    CreateShortcut "$SMPROGRAMS\Discordia.lnk" "$INSTDIR\Discordia.exe"
    Delete "$SMPROGRAMS\Discordia\Discordia.lnk"
    Delete "$SMPROGRAMS\Discordia\Uninstall Discordia.lnk"
    RMDir "$SMPROGRAMS\Discordia"
SectionEnd

; The generated uninstaller removes the folder, not a shortcut beside it.
Function un.onUninstSuccess
    Delete "$SMPROGRAMS\Discordia.lnk"
    RMDir "$SMPROGRAMS\Discordia"
FunctionEnd
