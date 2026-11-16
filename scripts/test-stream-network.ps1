param([ValidateSet('ipv4','ipv6','tcp','turn')][string[]]$Cases=@('ipv4','ipv6','tcp','turn'))
$ErrorActionPreference='Stop'
$repo=Split-Path $PSScriptRoot -Parent
Set-Location $repo
$output=Join-Path $repo 'target/stream-network'
New-Item -ItemType Directory -Force $output | Out-Null
$build=& cargo test -p discordia --lib --no-run --message-format=json 2> (Join-Path $output 'build.log')
if($LASTEXITCODE -ne 0){throw 'Video test build failed; see target/stream-network/build.log'}
$testExe=$build | ForEach-Object {try{$_|ConvertFrom-Json}catch{}} | Where-Object {$_.reason -eq 'compiler-artifact' -and $_.executable -and $_.profile.test -and $_.target.name -eq 'discordia'} | Select-Object -Last 1 -ExpandProperty executable
if(-not $testExe){throw 'Native video test executable not found'}
$archive=Get-ChildItem target/debug/build -Filter 'discordia-server-*' -Directory | ForEach-Object {Get-Item (Join-Path $_.FullName 'out/livekit-server.gz') -ErrorAction SilentlyContinue} | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if(-not $archive){throw 'Build the bundled SFU first'}
$sfuExe=Join-Path $output 'Discordia-RTC.exe'
$inputFile=[IO.File]::OpenRead($archive.FullName)
try {
 $gzip=[IO.Compression.GZipStream]::new($inputFile,[IO.Compression.CompressionMode]::Decompress)
 $binary=[IO.File]::Create($sfuExe)
 try{$gzip.CopyTo($binary)}finally{$binary.Dispose();$gzip.Dispose()}
}finally{$inputFile.Dispose()}
function Stop-OwnedProcess($process){
 if($null -eq $process -or $process.HasExited){return}
 $null=$process.CloseMainWindow()
 if(-not $process.WaitForExit(1000)){$process.Kill();if(-not $process.WaitForExit(3000)){throw "Task process did not stop: $($process.Id)"}}
}
$values=@{
 DISCORDIA_TEST_REQUIRE_NVENC=$null
 DISCORDIA_TEST_NVENC_RATES=$null
 DISCORDIA_TEST_GPU_CONTENTION=$null
 DISCORDIA_TEST_NVENC_1440P=$null
 DISCORDIA_TEST_NVENC_1440P60=$null
 DISCORDIA_TEST_SIMULCAST=$null
 DISCORDIA_TEST_REQUIRE_MF=$null
 DISCORDIA_TEST_MF_LOAD=$null
 DISCORDIA_TEST_MF_FALLBACK=$null
 DISCORDIA_TEST_MODERN_CODEC=$null
 DISCORDIA_TEST_MODERN_NO_E2EE=$null
 DISCORDIA_TEST_CAPTURE_DELAY_MS=$null
 DISCORDIA_TEST_MONOTONIC_CAPTURE=$null
 DISCORDIA_TEST_LEGACY_START=$null
 DISCORDIA_TEST_SFU_URL=$null
 DISCORDIA_TEST_EXPECT_MEDIA_FAMILY=$null
 DISCORDIA_TEST_EXPECT_MEDIA_PROTOCOL=$null
 DISCORDIA_TEST_REQUIRE_RELAY=$null
 DISCORDIA_TEST_MEDIA_LOG='libwebrtc=warn,webrtc_sys=warn,livekit=warn'
}
$saved=@{}
$results=@()
$secret=[guid]::NewGuid().ToString('N')+[guid]::NewGuid().ToString('N')
$values['DISCORDIA_TEST_SFU_SECRET']=$secret
try{
 foreach($name in $values.Keys){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process');if($null -eq $values[$name]){if(Test-Path "Env:$name"){Remove-Item "Env:$name"}}else{[Environment]::SetEnvironmentVariable($name,$values[$name],'Process')}}
 foreach($case in $Cases){
  $server=$null;$test=$null
  try{
   $v6=$case -eq 'ipv6'
   $mediaIp=if($v6){'::1'}else{'127.0.0.1'}
   $localV4=@([Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() | Where-Object {$_.OperationalStatus -eq 'Up'} | ForEach-Object {$_.GetIPProperties().UnicastAddresses} | ForEach-Object {$_.Address} | Where-Object {$_.AddressFamily -eq 'InterNetwork' -and -not [Net.IPAddress]::IsLoopback($_)} | ForEach-Object {$_.ToString()} | Select-Object -Unique)
   if($case -eq 'turn'){
    if($localV4.Count -eq 0){throw 'TURN test needs a non-loopback IPv4 interface'}
    $mediaIp=$localV4[0]
   }
   $cidr=if($v6){'::/0'}else{'0.0.0.0/0'}
   $signalHost=if($v6){'127.0.0.1'}else{'[::1]'}
   $forceTcp=if($case -eq 'tcp'){'true'}else{'false'}
   $allowed=@('127.0.0.0/8','::1/128')+@($localV4 | ForEach-Object {"$_/32"})
   $allowedJson=ConvertTo-Json -InputObject $allowed -Compress
   $turn=if($case -eq 'turn'){@"
turn:
  enabled: true
  udp_port: 27883
  tls_port: 0
  relay_range_start: 27884
  relay_range_end: 27904
  allow_restricted_peer_cidrs: $allowedJson
"@}else{''}
   $config=@"
port: 27880
bind_addresses: ["127.0.0.1", "::1"]
rtc:
  tcp_port: 27881
  udp_port: 27882
  node_ip: "$mediaIp"
  use_external_ip: false
  advertise_internal_ip: true
  enable_loopback_candidate: true
  force_tcp: $forceTcp
  ips:
    includes: ["$cidr"]
keys:
  discordia-audit: $secret
logging:
  level: warn
$turn
"@
   $configPath=Join-Path $output "$case.yaml"
   [IO.File]::WriteAllText($configPath,$config)
   $server=Start-Process -FilePath $sfuExe -ArgumentList @('--config',"`"$configPath`"") -WindowStyle Hidden -RedirectStandardOutput (Join-Path $output "$case-server.out.log") -RedirectStandardError (Join-Path $output "$case-server.err.log") -PassThru
   Write-Output "$case SFU PID: $($server.Id)"
   $ready=$false
   for($attempt=0;$attempt -lt 100;$attempt++){
    if($server.HasExited){throw "SFU $case exited; see server log"}
    $socket=[Net.Sockets.TcpClient]::new()
    try{$socket.Connect('127.0.0.1',27880);$ready=$true;break}catch{Start-Sleep -Milliseconds 100}finally{$socket.Dispose()}
   }
   if(-not $ready){throw 'SFU startup timed out'}
   $env:DISCORDIA_TEST_SFU_URL="ws://${signalHost}:27880"
   $env:DISCORDIA_TEST_EXPECT_MEDIA_FAMILY=if($v6){'ipv6'}else{'ipv4'}
   $env:DISCORDIA_TEST_EXPECT_MEDIA_PROTOCOL=if($case -eq 'tcp'){'tcp'}else{'udp'}
   if($case -eq 'turn'){$env:DISCORDIA_TEST_REQUIRE_RELAY='1'}elseif(Test-Path Env:DISCORDIA_TEST_REQUIRE_RELAY){Remove-Item Env:DISCORDIA_TEST_REQUIRE_RELAY}
   $test=Start-Process -FilePath $testExe -ArgumentList @('--ignored','--exact','features::voice::tests::native_screen_codecs_reach_a_real_decoder','--nocapture','--test-threads=1') -WindowStyle Hidden -RedirectStandardOutput (Join-Path $output "$case-test.out.log") -RedirectStandardError (Join-Path $output "$case-test.err.log") -PassThru
   Write-Output "$case test PID: $($test.Id)"
   if(-not $test.WaitForExit(60000)){throw 'Video test exceeded 60 seconds'}
   $results+=[pscustomobject]@{Case=$case;ExitCode=$test.ExitCode}
   Write-Output "$case result: $($test.ExitCode)"
  }finally{Stop-OwnedProcess $test;Stop-OwnedProcess $server}
 }
}finally{
 foreach($name in $saved.Keys){if($null -eq $saved[$name]){if(Test-Path "Env:$name"){Remove-Item "Env:$name"}}else{[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}}
}
$results | ConvertTo-Json | Set-Content (Join-Path $output 'results.json')
$results | Format-Table
if($results | Where-Object ExitCode -ne 0){exit 1}
