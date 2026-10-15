$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$output = Join-Path $repo 'target/frame-pacing-probe'
New-Item -ItemType Directory -Force $output | Out-Null
$source = Get-Content (Join-Path $repo 'client/src/sysvideo/windows.rs') -Raw
$start = $source.IndexOf('struct FramePacer {')
$end = $source.IndexOf('impl GraphicsCaptureApiHandler for Handler {')
if ($start -lt 0 -or $end -le $start) { throw 'Cannot locate the production FramePacer' }
$source.Substring($start, $end - $start) | Set-Content (Join-Path $output 'frame_pacer_under_test.rs')
Copy-Item (Join-Path $PSScriptRoot 'frame-pacing-probe.rs') (Join-Path $output 'probe.rs')
if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installation) { throw 'MSVC C++ tools are required' }
    & (Join-Path $installation 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
}
$binary = Join-Path $output 'frame-pacing-probe.exe'
& rustc --edition=2024 -D warnings (Join-Path $output 'probe.rs') -o $binary
if ($LASTEXITCODE -ne 0) { throw 'Frame pacing probe did not compile' }
$results = & $binary
if ($LASTEXITCODE -ne 0) { throw 'Frame pacing baseline checks failed' }
$results | Set-Content (Join-Path $output 'results.csv')
$results
