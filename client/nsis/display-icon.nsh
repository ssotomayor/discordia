; Dioxus 0.7.10 registers the app for uninstall but omits DisplayIcon.
Section "-Installed app icon"
    WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\com.discordia.app" \
        "DisplayIcon" '"$INSTDIR\Discordia.exe",0'
SectionEnd
