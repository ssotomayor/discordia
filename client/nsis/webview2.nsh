; WebView2 runtime, settled by the application rather than by this script.
;
; dx includes this file after the main install section, so this section runs
; once Discordia.exe is already on disk.
;
; Deliberately thin: the detection and the bootstrapper invocation live in Rust
; (client/src/webview2.rs) and are reached here by running the freshly installed
; binary with --ensure-webview2. The portable zip calls that same code path on
; startup, so neither distribution can drift from the other — which is why this
; script contains no registry lookup of its own.
;
; ${__FILEDIR__} is this file's own directory, so the File below does not depend
; on makensis's working directory (dx invokes it with -NOCD). The workflow drops
; the bootstrapper here before `dx bundle` runs.

Section "-WebView2 runtime"
    SetOutPath "$INSTDIR"
    ; Beside the executable — the same place the portable zip puts it, which is
    ; where webview2::bootstrapper_path looks.
    File "${__FILEDIR__}\MicrosoftEdgeWebview2Setup.exe"

    DetailPrint "Checking for the Microsoft Edge WebView2 runtime..."
    ExecWait '"$INSTDIR\Discordia.exe" --ensure-webview2' $0
    StrCmp $0 "0" webview2_done 0

    ; Warn rather than abort. Discordia is installed and working at this point;
    ; only the runtime is missing, and the app retries this same check on every
    ; launch — so failing the whole installation would take away more than it
    ; protects.
    MessageBox MB_OK|MB_ICONEXCLAMATION "Discordia was installed, but the Microsoft Edge WebView2 runtime could not be set up (code $0).$\n$\nThis usually means no internet connection during setup. Discordia will try again the next time you start it, or you can run MicrosoftEdgeWebview2Setup.exe from the installation folder."

    webview2_done:
SectionEnd
