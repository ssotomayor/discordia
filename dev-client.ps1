Set-Location $PSScriptRoot
if ($env:FAST -eq "1") { $env:LIVEKIT_BUNDLE_SKIP = "1" }
dx serve --package dioxusfun
