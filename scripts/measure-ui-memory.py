import argparse
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import statistics
import subprocess
import threading
import time


class Counters(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in (
            "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
            "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage",
            "PagefileUsage", "PeakPagefileUsage", "PrivateUsage",
        )
    ]


def sample(binary, scenario):
    query = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    query.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    query.restype = wintypes.BOOL
    env = dict(os.environ, DISCORDIA_UI_RAM_SCENARIO=scenario)
    process = subprocess.Popen(
        [binary, "benchmark_ram_ui", "--ignored", "--nocapture", "--test-threads=1"],
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env=env,
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


def measure(binary, count):
    result = {}
    for scenario in ("messages", "speaking", "game_details"):
        samples = [sample(binary, scenario) for _ in range(count)]
        result[scenario] = {
            "median": {key: statistics.median(s[key] for s in samples) for key in samples[0]},
            "samples": samples,
        }
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", help="Windows Discordia unit-test executable")
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--output", default="target/ui-memory.json")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("Windows process-memory counters are required")
    if args.samples < 1:
        parser.error("samples must be positive")
    result = measure(args.binary, args.samples)
    Path(args.output).write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps({k: v["median"] for k, v in result.items()}, indent=2))
