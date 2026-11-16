param([ValidateRange(10,60)][int]$Seconds=45,[ValidateSet('Encoder','Capture')][string]$Stage='Encoder')
$ErrorActionPreference='Stop'
$repo=Split-Path $PSScriptRoot -Parent
Set-Location $repo
$output=Join-Path $repo ("target/stream-contention/"+$Stage.ToLowerInvariant())
New-Item -ItemType Directory -Force $output | Out-Null
$dataRoot=Join-Path $output ("sfu-"+[guid]::NewGuid())
$devices=@(& nvidia-smi --query-gpu=name --format=csv,noheader)
if($LASTEXITCODE -ne 0 -or $devices.Count -ne 1){throw 'This probe requires exactly one NVIDIA GPU for unambiguous telemetry'}
$vswhere=Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$vs=& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if(-not $vs){throw 'MSVC C++ tools required'}
& (Join-Path $vs 'Common7/Tools/Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
$probe=Join-Path $output 'gpu-contention-probe.exe'
& cl.exe /nologo /O2 /std:c++17 /EHsc /W4 /WX scripts/gpu-contention-probe.cpp "/Fo:$output/gpu-contention-probe.obj" "/Fe:$probe" /link d3d11.lib dxgi.lib d3dcompiler.lib
if($LASTEXITCODE -ne 0){throw 'GPU probe compilation failed'}
$build=& cargo test -p discordia --lib --no-run --message-format=json 2> (Join-Path $output 'build.log')
if($LASTEXITCODE -ne 0){throw 'Native video test compilation failed'}
$testExe=$build | ForEach-Object {try{$_|ConvertFrom-Json}catch{}} | Where-Object {$_.reason -eq 'compiler-artifact' -and $_.executable -and $_.profile.test -and $_.target.name -eq 'discordia'} | Select-Object -Last 1 -ExpandProperty executable
if(-not $testExe){throw 'Native video test executable not found'}
$values=@{
 LIVEKIT_PREFERRED_HW_ENCODER='nvenc'
 DISCORDIA_TEST_REQUIRE_NVENC='1'
 DISCORDIA_TEST_REQUIRE_MF=$null
 DISCORDIA_TEST_MF_LOAD=$null
 DISCORDIA_TEST_MF_FALLBACK=$null
 DISCORDIA_TEST_MODERN_CODEC=$null
 DISCORDIA_TEST_MODERN_NO_E2EE=$null
 DISCORDIA_TEST_LEGACY_START=$null
 DISCORDIA_TEST_NVENC_CONGESTION=$null
 DISCORDIA_TEST_NVENC_RATES='1'
 DISCORDIA_TEST_GPU_CONTENTION='1'
 DISCORDIA_TEST_NVENC_1440P60='1'
 DISCORDIA_TEST_SIMULCAST=$null
 DISCORDIA_TEST_CAPTURE_DELAY_MS=$null
 DISCORDIA_TEST_MONOTONIC_CAPTURE=$null
 DISCORDIA_TEST_SFU_URL=$null
 DISCORDIA_TEST_SFU_DATA_DIR=$dataRoot
 DISCORDIA_TEST_EXPECT_MEDIA_FAMILY=$null
 DISCORDIA_TEST_REQUIRE_RELAY=$null
 DISCORDIA_TEST_CAPTURE_1440P60=if($Stage -eq 'Capture'){'1'}else{$null}
 DISCORDIA_LIVEKIT_PORT='27880'
 DISCORDIA_TEST_MEDIA_LOG='libwebrtc=warn,webrtc_sys=warn,livekit=warn'
}
$saved=@{};$gpu=$null;$test=$null
$stop=Join-Path $output ("stop-"+[guid]::NewGuid())
$samples=@()
try{
 foreach($name in $values.Keys){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process');if($null -eq $values[$name]){if(Test-Path "Env:$name"){Remove-Item "Env:$name"}}else{[Environment]::SetEnvironmentVariable($name,$values[$name],'Process')}}
 $gpu=Start-Process -FilePath $probe -ArgumentList @("$Seconds","`"$stop`"") -WindowStyle Hidden -RedirectStandardOutput (Join-Path $output 'gpu.out.log') -RedirectStandardError (Join-Path $output 'gpu.err.log') -PassThru
 Write-Output "GPU load PID: $($gpu.Id)"
 Start-Sleep -Seconds 2
 if($gpu.HasExited){throw 'GPU load probe stopped early; see its log'}
 $testName=if($Stage -eq 'Encoder'){'features::voice::tests::native_screen_codecs_reach_a_real_decoder'}else{'sysvideo::windows::tests::selected_windows_and_monitors_feed_livekit_and_stop'}
 $test=Start-Process -FilePath $testExe -ArgumentList @('--ignored','--exact',$testName,'--nocapture','--test-threads=1') -WindowStyle Hidden -RedirectStandardOutput (Join-Path $output 'video.out.log') -RedirectStandardError (Join-Path $output 'video.err.log') -PassThru
 Write-Output "Video test PID: $($test.Id)"
 $deadline=[DateTime]::UtcNow.AddSeconds(90)
 while(-not $test.HasExited -and [DateTime]::UtcNow -lt $deadline){
  $sample=& nvidia-smi --query-gpu=timestamp,utilization.gpu,utilization.encoder,memory.used --format=csv,noheader,nounits
  if($LASTEXITCODE -eq 0){$samples+=$sample}
  Start-Sleep -Milliseconds 500
 }
 if(-not $test.HasExited){throw 'Video test exceeded the 90 second deadline'}
 $result=$test.ExitCode
 Write-Output "Video test result: $result"
}finally{
 [IO.File]::WriteAllText($stop,'stop')
 if($gpu -and -not $gpu.HasExited){if(-not $gpu.WaitForExit(3000)){$gpu.Kill();$gpu.WaitForExit()}}
 if($test -and -not $test.HasExited){$null=$test.CloseMainWindow();if(-not $test.WaitForExit(1000)){$test.Kill();$test.WaitForExit()}}
 foreach($pidFile in @(Get-ChildItem (Join-Path $dataRoot 'livekit') -Filter '*.pid' -ErrorAction SilentlyContinue)){
  $lines=Get-Content -LiteralPath $pidFile.FullName
  if($lines.Count -lt 2){continue}
  $childPid=0
  if(-not [int]::TryParse($lines[0],[ref]$childPid)){continue}
  $image=[IO.Path]::GetFullPath($lines[1])
  if(-not $image.StartsWith([IO.Path]::GetFullPath($dataRoot)+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'SFU process record escaped the test directory'}
  try{$child=[Diagnostics.Process]::GetProcessById($childPid)}catch{continue}
  if($child.HasExited -or $child.MainModule.FileName -ne $image -or $child.StartTime -lt $test.StartTime){continue}
  $null=$child.CloseMainWindow()
  if(-not $child.WaitForExit(1000)){$child.Kill();$child.WaitForExit()}
  Write-Output "Owned SFU process exited: $($child.HasExited)"
 }
 if($gpu){Write-Output "GPU process exited: $($gpu.HasExited); result: $($gpu.ExitCode)"}
 if($test){Write-Output "Video process exited: $($test.HasExited)"}
 $samples | Set-Content (Join-Path $output 'gpu.csv')
 foreach($name in $saved.Keys){if($null -eq $saved[$name]){if(Test-Path "Env:$name"){Remove-Item "Env:$name"}}else{[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
}
if($gpu.ExitCode -ne 0){throw 'GPU probe failed; load was not sustained'}
if(-not (Get-Content (Join-Path $output 'gpu.out.log') -First 1).Contains($devices[0].Trim())){throw 'The load probe and encoder telemetry targeted different GPUs'}
$usage=@($samples | ForEach-Object {[int]($_.Split(',')[1].Trim())})
if($usage.Count -eq 0 -or @($usage | Where-Object {$_ -ge 95}).Count -lt $usage.Count * 0.9){throw 'GPU did not remain at least 95% busy for 90% of samples; see gpu.csv'}
$usage | Measure-Object -Minimum -Maximum -Average | Format-List
exit $result
