$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) { throw 'MSVC C++ tools are required' }
& (Join-Path $installation 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
$output = Join-Path $repo 'target/mf-encoder-tests'
New-Item -ItemType Directory -Force $output | Out-Null
Push-Location $output
try {
    & cl.exe /nologo /std:c++17 /EHsc /W4 /WX (Join-Path $repo 'vendor/webrtc-sys/tests/realtime_encoder.cpp') /Fe:realtime_encoder.exe
    if ($LASTEXITCODE -ne 0) { throw 'Encoder regression tests did not compile' }
    & ./realtime_encoder.exe
    if ($LASTEXITCODE -ne 0) { throw 'Encoder regression tests failed' }
} finally {
    Pop-Location
}
