# Open Windows Terminal with three vertical panes running:
#   1) rendezvous (left)
#   2) gateway server (middle)
#   3) Dioxus client (right)
#
# Usage:  .\dev.ps1
#
# Set FAST=1 to skip the bundled-LiveKit build for faster iteration when
# you don't care about voice end-to-end:
#   $env:FAST=1; .\dev.ps1

param()

$Root = $PSScriptRoot

wt.exe new-tab --title "rendezvous" powershell -NoExit -File "$Root\dev-rendezvous.ps1" `; split-pane -V --title "server" powershell -NoExit -File "$Root\dev-server.ps1" `; split-pane -V --title "client" powershell -NoExit -File "$Root\dev-client.ps1"