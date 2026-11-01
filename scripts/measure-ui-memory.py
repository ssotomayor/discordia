#!/usr/bin/env python3
"""
Discordia Windows RAM Benchmark & Memory Profiler.

Supports two distinct modes:
1. 'benchmark': Headless VirtualDOM benchmark executing `benchmark_ram_ui`
   scenarios (messages, speaking, game_details) across repeated samples.
2. 'live': Real Discordia application profiling with WebView2 subsystem
   process discovery, classifying each process role, and capturing Private Bytes,
   Working Set, Peak Commit, Page Faults, and CPU utilization.
"""

import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import threading
import time


# ============================================================================
# Win32 Structures and API Bindings
# ============================================================================

class Counters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
        ("PrivateUsage", ctypes.c_size_t),
    ]


class FILETIME(ctypes.Structure):
    _fields_ = [
        ("dwLowDateTime", wintypes.DWORD),
        ("dwHighDateTime", wintypes.DWORD),
    ]

    def to_u64(self):
        return (self.dwHighDateTime << 32) | self.dwLowDateTime


PROCESS_QUERY_INFORMATION = 0x0400
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_VM_READ = 0x0010


def get_git_commit_sha():
    try:
        res = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=3,
        )
        if res.returncode == 0:
            return res.stdout.strip()
    except Exception:
        pass
    return "unknown"


def get_webview2_version():
    try:
        import winreg
        key_path = r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, key_path) as k:
            val, _ = winreg.QueryValueEx(k, "pv")
            if val:
                return str(val)
    except Exception:
        pass
    return "unknown"


# ============================================================================
# Mode 1: Headless VirtualDOM Benchmark (benchmark_ram_ui)
# ============================================================================

def sample_benchmark(binary, scenario):
    query = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    env = dict(os.environ, DISCORDIA_UI_RAM_SCENARIO=scenario)
    process = subprocess.Popen(
        [binary, "benchmark_ram_ui", "--ignored", "--nocapture", "--test-threads=1"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=env,
    )
    stage = [None]
    output = []

    def read_output():
        for line in process.stdout:
            output.append(line)
            if "UI_RAM mounted" in line:
                stage[0] = "mounted"
            elif "UI_RAM finished" in line:
                stage[0] = "finished"

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    mounted = []
    finished = []
    peak_resident = 0
    peak_commit = 0
    deadline = time.monotonic() + 60
    try:
        while process.poll() is None:
            if time.monotonic() > deadline:
                raise TimeoutError("RAM benchmark exceeded 60 seconds")
            counters = Counters()
            counters.cb = ctypes.sizeof(counters)
            if query(process._handle, ctypes.byref(counters), counters.cb):
                peak_resident = max(peak_resident, counters.PeakWorkingSetSize)
                peak_commit = max(peak_commit, counters.PeakPagefileUsage)
                point = (counters.WorkingSetSize, counters.PrivateUsage)
                if stage[0] == "mounted" and len(mounted) < 10:
                    mounted.append(point)
                elif stage[0] == "finished":
                    finished.append(point)
            elif process.poll() is None:
                raise ctypes.WinError(ctypes.get_last_error())
            time.sleep(0.01)
        reader.join(timeout=1)
        if process.returncode or not mounted or not finished:
            raise RuntimeError("RAM benchmark failed:\n" + "".join(output))
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    mib = 1024 * 1024
    return {
        "mounted_resident_mib": statistics.median(p[0] for p in mounted) / mib,
        "mounted_private_mib": statistics.median(p[1] for p in mounted) / mib,
        "finished_resident_mib": statistics.median(p[0] for p in finished) / mib,
        "finished_private_mib": statistics.median(p[1] for p in finished) / mib,
        "peak_resident_mib": peak_resident / mib,
        "peak_commit_mib": peak_commit / mib,
    }


def run_benchmark(binary, count):
    result = {}
    for scenario in ("messages", "speaking", "game_details"):
        samples = [sample_benchmark(binary, scenario) for _ in range(count)]
        result[scenario] = {
            "median": {key: statistics.median(s[key] for s in samples) for key in samples[0]},
            "samples": samples,
        }
    return {
        "mode": "benchmark_headless_vdom",
        "commit_sha": get_git_commit_sha(),
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "scenarios": result,
    }


# ============================================================================
# Mode 2: Live Discordia Real App & WebView2 Measurement
# ============================================================================

def classify_role(name, command_line):
    name_lower = name.lower()
    if "discordia" in name_lower and not "webview" in name_lower:
        return "Host (Rust / Dioxus)"
    if "webview" in name_lower or "msedge" in name_lower:
        cl = command_line.lower()
        if "--type=renderer" in cl:
            return "WebView2: Renderer"
        if "--type=gpu-process" in cl:
            return "WebView2: GPU Process"
        if "audio.mojom.audioservice" in cl:
            return "WebView2: Audio Service"
        if "network.mojom.networkservice" in cl:
            return "WebView2: Network Service"
        if "storage.mojom.storageservice" in cl:
            return "WebView2: Storage Service"
        if "--type=crashpad-handler" in cl:
            return "WebView2: Crashpad Handler"
        if "--embedded-browser-webview" in cl or "--type=" not in cl:
            return "WebView2: Browser Controller"
        return "WebView2: Subsystem"
    return "Other"


def query_windows_processes():
    """
    Enumerate processes and command lines using PowerShell CIM.
    Returns list of dicts: {ProcessId, ParentProcessId, Name, CommandLine}
    """
    cmd = (
        'Get-CimInstance Win32_Process | '
        'Select-Object ProcessId, ParentProcessId, Name, CommandLine | '
        'ConvertTo-Json -Compress'
    )
    res = subprocess.run(
        ["powershell", "-NoProfile", "-Command", cmd],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    if res.returncode != 0 or not res.stdout.strip():
        return []
    try:
        data = json.loads(res.stdout)
        if isinstance(data, dict):
            return [data]
        return data
    except Exception:
        return []


def discover_discordia_topology(target_pid=None):
    """
    Dynamically discover Discordia.exe and all associated WebView2 processes.
    Does not assume a fixed process count.
    """
    all_procs = query_windows_processes()
    
    # 1. Identify Discordia Host PID
    host_pid = None
    if target_pid:
        host_pid = int(target_pid)
    else:
        for p in all_procs:
            name = (p.get("Name") or "").lower()
            if name == "discordia.exe":
                host_pid = p.get("ProcessId")
                break

    if not host_pid:
        return None, [], [], []

    identified = []
    uncertain = []
    access_denied = []

    # 2. Build graph of associated PIDs
    discordia_pids = {host_pid}
    
    # Trace children iteratively (tree)
    changed = True
    while changed:
        changed = False
        for p in all_procs:
            pid = p.get("ProcessId")
            parent_pid = p.get("ParentProcessId")
            if parent_pid in discordia_pids and pid not in discordia_pids:
                name = (p.get("Name") or "").lower()
                if "webview" in name or "discordia" in name:
                    discordia_pids.add(pid)
                    changed = True

    # Also match by command line user-data-dir and webview-exe-name
    for p in all_procs:
        pid = p.get("ProcessId")
        if pid in discordia_pids:
            continue
        cl = (p.get("CommandLine") or "").lower()
        name = (p.get("Name") or "").lower()
        if "msedgewebview2" in name:
            if "discordia\\ebwebview" in cl or "discordia.exe" in cl:
                discordia_pids.add(pid)

    # 3. Categorize discovered processes
    for p in all_procs:
        pid = p.get("ProcessId")
        if pid in discordia_pids:
            name = p.get("Name") or ""
            cl = p.get("CommandLine") or ""
            role = classify_role(name, cl)
            identified.append({
                "pid": pid,
                "parent_pid": p.get("ParentProcessId"),
                "name": name,
                "role": role,
                "command_line": cl,
            })

    return host_pid, identified, uncertain, access_denied


def sample_process_memory(pid):
    kernel32 = ctypes.windll.kernel32
    psapi = ctypes.windll.psapi
    
    handle = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, pid)
    if not handle:
        handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None

    try:
        counters = Counters()
        counters.cb = ctypes.sizeof(counters)
        if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
            return None

        creation = FILETIME()
        exit_time = FILETIME()
        kernel_time = FILETIME()
        user_time = FILETIME()
        cpu_time_100ns = 0
        if kernel32.GetProcessTimes(
            handle,
            ctypes.byref(creation),
            ctypes.byref(exit_time),
            ctypes.byref(kernel_time),
            ctypes.byref(user_time),
        ):
            cpu_time_100ns = kernel_time.to_u64() + user_time.to_u64()

        return {
            "working_set_bytes": counters.WorkingSetSize,
            "private_bytes": counters.PrivateUsage,
            "peak_working_set_bytes": counters.PeakWorkingSetSize,
            "peak_commit_bytes": counters.PeakPagefileUsage,
            "page_faults": counters.PageFaultCount,
            "cpu_time_100ns": cpu_time_100ns,
        }
    finally:
        kernel32.CloseHandle(handle)


def run_live_measurement(target_pid=None, duration=5.0, interval=0.5):
    host_pid, identified, uncertain, access_denied = discover_discordia_topology(target_pid)
    if not host_pid or not identified:
        raise RuntimeError("No active Discordia instance found. Please launch Discordia or provide --pid.")

    num_cores = os.cpu_count() or 1
    pids = [proc["pid"] for proc in identified]
    
    # Initial sample for CPU baseline
    t0 = time.monotonic()
    prev_samples = {}
    for pid in pids:
        s = sample_process_memory(pid)
        if s:
            prev_samples[pid] = (t0, s)

    time.sleep(min(duration, max(interval, 0.5)))
    
    # Final sample
    t1 = time.monotonic()
    dt = max(t1 - t0, 0.001)
    
    per_process = []
    total_private = 0
    total_resident = 0
    total_peak_commit = 0
    total_page_faults = 0
    total_cpu_pct = 0.0

    mib = 1024 * 1024

    for proc in identified:
        pid = proc["pid"]
        curr = sample_process_memory(pid)
        if not curr:
            continue
        
        cpu_pct = 0.0
        if pid in prev_samples:
            _, prev = prev_samples[pid]
            dcpu = (curr["cpu_time_100ns"] - prev["cpu_time_100ns"]) / 1e7
            cpu_pct = max(0.0, round((dcpu / dt) * 100 / num_cores, 2))

        ws_mib = round(curr["working_set_bytes"] / mib, 2)
        priv_mib = round(curr["private_bytes"] / mib, 2)
        peak_commit_mib = round(curr["peak_commit_bytes"] / mib, 2)
        faults = curr["page_faults"]

        total_private += curr["private_bytes"]
        total_resident += curr["working_set_bytes"]
        total_peak_commit = max(total_peak_commit, curr["peak_commit_bytes"])
        total_page_faults += faults
        total_cpu_pct += cpu_pct

        per_process.append({
            "pid": pid,
            "parent_pid": proc["parent_pid"],
            "name": proc["name"],
            "role": proc["role"],
            "working_set_mib": ws_mib,
            "private_bytes_mib": priv_mib,
            "peak_commit_mib": peak_commit_mib,
            "page_faults": faults,
            "cpu_percent": cpu_pct,
        })

    # Sort: Host first, then descending by Private Bytes
    per_process.sort(key=lambda x: (0 if "Host" in x["role"] else 1, -x["private_bytes_mib"]))

    return {
        "mode": "live_application_webview2",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "commit_sha": get_git_commit_sha(),
        "os": platform.platform(),
        "webview2_version": get_webview2_version(),
        "duration_seconds": round(dt, 2),
        "processes_identified_count": len(per_process),
        "uncertain_processes": uncertain,
        "access_denied_pids": access_denied,
        "summary": {
            "total_private_commit_mib": round(total_private / mib, 2),
            "total_resident_working_set_mib": round(total_resident / mib, 2),
            "peak_commit_mib": round(total_peak_commit / mib, 2),
            "total_page_faults": total_page_faults,
            "total_cpu_percent": round(total_cpu_pct, 2),
        },
        "processes": per_process,
    }


def format_live_table(report):
    lines = []
    lines.append("=" * 88)
    lines.append(f" DISCORDIA LIVE MEMORY TOPOLOGY REPORT — Commit: {report['commit_sha'][:8]}")
    lines.append(f" OS: {report['os']} | WebView2 Runtime: {report['webview2_version']}")
    lines.append(f" Timestamp: {report['timestamp']} | Duration: {report['duration_seconds']}s")
    lines.append("=" * 88)
    lines.append(f"{'PID':>6}  {'Name':<18}  {'Role':<26}  {'PrivBytes':>10}  {'WorkingSet':>10}  {'CPU%':>6}")
    lines.append("-" * 88)
    for p in report["processes"]:
        lines.append(
            f"{p['pid']:>6}  {p['name']:<18}  {p['role']:<26}  "
            f"{p['private_bytes_mib']:>8.2f} MB  {p['working_set_mib']:>8.2f} MB  {p['cpu_percent']:>5.2f}%"
        )
    lines.append("-" * 88)
    s = report["summary"]
    lines.append(
        f"TOTALS (Dynamic {report['processes_identified_count']} processes):  "
        f"Private Commit: {s['total_private_commit_mib']:.2f} MB  |  "
        f"Resident WS: {s['total_resident_working_set_mib']:.2f} MB  |  "
        f"CPU: {s['total_cpu_percent']:.2f}%"
    )
    lines.append("=" * 88)
    return "\n".join(lines)


# ============================================================================
# Main Entry Point
# ============================================================================

def main():
    parser = argparse.ArgumentParser(description="Discordia Windows RAM Profiler")
    parser.add_argument("binary", nargs="?", help="Windows Discordia unit-test executable or app")
    parser.add_argument(
        "--mode",
        choices=["benchmark", "live"],
        default="benchmark",
        help="Mode: 'benchmark' for headless VirtualDOM, 'live' for real Discordia app + WebView2",
    )
    parser.add_argument("--pid", type=int, help="Target PID of running Discordia.exe for live mode")
    parser.add_argument("--duration", type=float, default=3.0, help="Sampling duration in seconds for live mode")
    parser.add_argument("--samples", type=int, default=3, help="Number of samples for benchmark mode")
    parser.add_argument("--output", default="target/ui-memory.json", help="Output path for JSON report")

    args = parser.parse_args()

    if os.name != "nt":
        parser.error("Windows process-memory counters require a Windows operating system")

    if args.mode == "live" or args.pid is not None:
        report = run_live_measurement(target_pid=args.pid, duration=args.duration)
        Path(args.output).parent.mkdir(parents=True, exist_ok=True)
        Path(args.output).write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(format_live_table(report))
        print(f"\nJSON report saved to: {args.output}")
    else:
        if not args.binary:
            parser.error("Binary path required for benchmark mode (e.g. target/release/deps/discordia-*.exe)")
        if args.samples < 1:
            parser.error("Samples must be positive")
        report = run_benchmark(args.binary, args.samples)
        Path(args.output).parent.mkdir(parents=True, exist_ok=True)
        Path(args.output).write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps({k: v["median"] for k, v in report["scenarios"].items()}, indent=2))
        print(f"\nBenchmark JSON saved to: {args.output}")


if __name__ == "__main__":
    main()
