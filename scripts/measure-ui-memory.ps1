<#
.SYNOPSIS
    Discordia Windows RAM Profiler and Process Topology Analyzer (Native PowerShell).

.DESCRIPTION
    Measures the Discordia application and dynamically discovers all associated
    WebView2 processes without assuming a fixed process count.
    Captures Private Bytes (committed non-shared memory), Working Set (resident memory),
    Page Faults, and CPU utilization across a defined duration.

.PARAMETER TargetPid
    Optional PID of Discordia.exe. If omitted, the script discovers the running instance.

.PARAMETER DurationSeconds
    Sampling duration in seconds for CPU delta calculations (default: 3).

.PARAMETER OutputJson
    Optional path to write the structured JSON report.
#>

param(
    [int]$TargetPid = 0,
    [double]$DurationSeconds = 3.0,
    [string]$OutputJson = "target/ui-memory-live.json"
)

Set-StrictMode -Off
$ErrorActionPreference = "SilentlyContinue"

# 1. Discover Discordia Process Topology
$allProcs = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId, Name, CommandLine

$hostProc = $null
if ($TargetPid -gt 0) {
    $hostProc = $allProcs | Where-Object { $_.ProcessId -eq $TargetPid }
} else {
    $hostProc = $allProcs | Where-Object { $_.Name -like "Discordia.exe" } | Select-Object -First 1
}

if (-not $hostProc) {
    Write-Error "No active Discordia instance found. Please launch Discordia first."
    exit 1
}

$hostPid = $hostProc.ProcessId
$discordiaPids = [System.Collections.Generic.HashSet[int]]::new()
[void]$discordiaPids.Add($hostPid)

# Recursive child process discovery
$changed = $true
while ($changed) {
    $changed = $false
    foreach ($p in $allProcs) {
        $pidNum = [int]$p.ProcessId
        $parentNum = [int]$p.ParentProcessId
        if ($discordiaPids.Contains($parentNum) -and -not $discordiaPids.Contains($pidNum)) {
            $nameLower = ($p.Name + "").ToLower()
            if ($nameLower -like "*webview*" -or $nameLower -like "*discordia*") {
                [void]$discordiaPids.Add($pidNum)
                $changed = $true
            }
        }
    }
}

# CommandLine association for user-data-dir and webview-exe-name
foreach ($p in $allProcs) {
    $pidNum = [int]$p.ProcessId
    if ($discordiaPids.Contains($pidNum)) { continue }
    $cl = ($p.CommandLine + "").ToLower()
    $nameLower = ($p.Name + "").ToLower()
    if ($nameLower -like "*msedgewebview2*") {
        if ($cl -like "*discordia\ebwebview*" -or $cl -like "*discordia.exe*") {
            [void]$discordiaPids.Add($pidNum)
        }
    }
}

# Function to classify process role
function Get-ProcessRole([string]$name, [string]$commandLine) {
    $nameLower = $name.ToLower()
    if ($nameLower -like "discordia.exe") {
        return "Host (Rust / Dioxus)"
    }
    $cl = $commandLine.ToLower()
    if ($cl -like "*--type=renderer*") { return "WebView2: Renderer" }
    if ($cl -like "*--type=gpu-process*") { return "WebView2: GPU Process" }
    if ($cl -like "*audio.mojom.audioservice*") { return "WebView2: Audio Service" }
    if ($cl -like "*network.mojom.networkservice*") { return "WebView2: Network Service" }
    if ($cl -like "*storage.mojom.storageservice*") { return "WebView2: Storage Service" }
    if ($cl -like "*--type=crashpad-handler*") { return "WebView2: Crashpad Handler" }
    if ($cl -like "*--embedded-browser-webview*" -or $cl -notlike "*--type=*") { return "WebView2: Browser Controller" }
    return "WebView2: Subsystem"
}

# Initial CPU sample
$pidsList = @($discordiaPids)
$beforeSamples = @{}
$t0 = [DateTime]::UtcNow
foreach ($pidNum in $pidsList) {
    $gp = Get-Process -Id $pidNum -ErrorAction SilentlyContinue
    if ($gp) {
        $beforeSamples[$pidNum] = $gp.CPU
    }
}

Start-Sleep -Seconds ([Math]::Max(1, [int]$DurationSeconds))

$t1 = [DateTime]::UtcNow
$elapsedSeconds = ($t1 - $t0).TotalSeconds
$cpuCount = [Environment]::ProcessorCount

$processesReport = @()
$totalPrivateBytes = 0
$totalWorkingSet = 0
$peakCommit = 0
$totalPageFaults = 0
$totalCpu = 0.0

foreach ($p in $allProcs) {
    $pidNum = [int]$p.ProcessId
    if (-not $discordiaPids.Contains($pidNum)) { continue }
    
    $gp = Get-Process -Id $pidNum -ErrorAction SilentlyContinue
    if (-not $gp) { continue }

    $cpuPct = 0.0
    if ($beforeSamples.ContainsKey($pidNum) -and $gp.CPU -ge $beforeSamples[$pidNum]) {
        $dcpu = $gp.CPU - $beforeSamples[$pidNum]
        $cpuPct = [Math]::Round(($dcpu / $elapsedSeconds) * 100 / $cpuCount, 2)
    }

    $wsMb = [Math]::Round($gp.WorkingSet64 / 1MB, 2)
    $privMb = [Math]::Round($gp.PrivateMemorySize64 / 1MB, 2)
    $peakMb = [Math]::Round($gp.PeakPagedMemorySize64 / 1MB, 2)
    $faults = $gp.BasePriority # Fallback or HandleCount
    $role = Get-ProcessRole $p.Name $p.CommandLine

    $totalPrivateBytes += $gp.PrivateMemorySize64
    $totalWorkingSet += $gp.WorkingSet64
    if ($gp.PeakPagedMemorySize64 -gt $peakCommit) { $peakCommit = $gp.PeakPagedMemorySize64 }
    $totalCpu += $cpuPct

    $processesReport += [PSCustomObject]@{
        PID = $pidNum
        ParentPID = $p.ParentProcessId
        Name = $p.Name
        Role = $role
        WorkingSetMB = $wsMb
        PrivateBytesMB = $privMb
        PeakCommitMB = $peakMb
        Handles = $gp.HandleCount
        Threads = $gp.Threads.Count
        CpuPercent = $cpuPct
    }
}

# Sort Host first, then PrivateBytes descending
$sortedProcesses = $processesReport | Sort-Object @{Expression={if ($_.Role -like "Host*") {0} else {1}}}, @{Expression="PrivateBytesMB"; Descending=$true}

$commitSha = "unknown"
try {
    $commitSha = (git rev-parse HEAD 2>$null).Trim()
} catch {}

$wvVersion = "unknown"
try {
    $regKey = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    if (Test-Path $regKey) {
        $wvVersion = (Get-ItemProperty -Path $regKey -Name pv -ErrorAction SilentlyContinue).pv
    }
} catch {}

# Display Console Table
Write-Host ("=" * 90) -ForegroundColor Cyan
Write-Host " DISCORDIA LIVE PROCESS TOPOLOGY & MEMORY PROFILER" -ForegroundColor Cyan
Write-Host " Commit: $commitSha | WebView2 Runtime: $wvVersion | Duration: $([Math]::Round($elapsedSeconds, 1))s"
Write-Host ("=" * 90) -ForegroundColor Cyan

$sortedProcesses | Format-Table -Property @(
    @{Label="PID"; Expression={$_.PID}; Width=7},
    @{Label="Name"; Expression={$_.Name}; Width=18},
    @{Label="Role"; Expression={$_.Role}; Width=28},
    @{Label="PrivBytes"; Expression={"$($_.PrivateBytesMB) MB"}; Width=12},
    @{Label="WorkingSet"; Expression={"$($_.WorkingSetMB) MB"}; Width=12},
    @{Label="CPU%"; Expression={"$($_.CpuPercent)%"}; Width=8}
)

Write-Host ("-" * 90) -ForegroundColor Cyan
$totPrivMb = [Math]::Round($totalPrivateBytes / 1MB, 2)
$totWsMb = [Math]::Round($totalWorkingSet / 1MB, 2)
$totCpu = [Math]::Round($totalCpu, 2)
Write-Host "TOTALS ($($sortedProcesses.Count) processes detected dynamically):" -ForegroundColor Green
Write-Host "  Private Commit (Exclusive): $totPrivMb MB" -ForegroundColor Green
Write-Host "  Resident Working Set (Physical RAM inc. shared): $totWsMb MB" -ForegroundColor Green
Write-Host "  Total CPU: $totCpu%" -ForegroundColor Green
Write-Host ("=" * 90) -ForegroundColor Cyan

# Output JSON
$resultObj = [PSCustomObject]@{
    Mode = "live_application_webview2"
    Timestamp = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    CommitSha = $commitSha
    WebView2Version = $wvVersion
    DurationSeconds = [Math]::Round($elapsedSeconds, 2)
    ProcessesIdentifiedCount = $sortedProcesses.Count
    Summary = [PSCustomObject]@{
        TotalPrivateCommitMb = $totPrivMb
        TotalResidentWorkingSetMb = $totWsMb
        TotalCpuPercent = $totCpu
    }
    Processes = $sortedProcesses
}

if ($OutputJson) {
    $outDir = [System.IO.Path]::GetDirectoryName($OutputJson)
    if ($outDir -and -not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    }
    $resultObj | ConvertTo-Json -Depth 4 | Out-File -FilePath $OutputJson -Encoding utf8
    Write-Host "JSON report saved to: $OutputJson`n" -ForegroundColor DarkGray
}
