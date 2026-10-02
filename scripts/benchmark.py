"""Read-only startup/resource benchmark; fixtures live only in a temp repo."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
from support import CliStore, git


def measure(command):
    begin = time.perf_counter()
    proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stdout, stderr = proc.communicate()
    wall = (time.perf_counter() - begin) * 1000
    if proc.returncode:
        raise RuntimeError((command, proc.returncode, stdout, stderr))
    json.loads(stdout)
    sample = {"wall_ms": wall, "cpu_ms": None, "peak_working_set_bytes": None}
    if os.name == "nt":
        from ctypes import wintypes
        class Memory(ctypes.Structure):
            _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [(name, ctypes.c_size_t) for name in ("PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage", "PrivateUsage")]
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
        times = [wintypes.FILETIME() for _ in range(4)]
        handle = wintypes.HANDLE(int(proc._handle))
        if kernel.GetProcessTimes(handle, *map(ctypes.byref, times)):
            sample["cpu_ms"] = sum((v.dwHighDateTime << 32) + v.dwLowDateTime for v in times[2:]) / 10000
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Memory), wintypes.DWORD]
        mem = Memory()
        mem.cb = ctypes.sizeof(mem)
        if psapi.GetProcessMemoryInfo(handle, ctypes.byref(mem), mem.cb):
            sample["peak_working_set_bytes"] = mem.PeakWorkingSetSize
    return sample


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=10)
    args = parser.parse_args()
    native = ROOT / "target/release" / ("lane.exe" if os.name == "nt" else "lane")
    programs = {"rust": [str(native)]}
    results = {}
    with tempfile.TemporaryDirectory(prefix="lane-benchmark-") as temp:
        repo = Path(temp) / "repo"
        repo.mkdir()
        git(repo, "init", "-b", "main")
        git(repo, "config", "user.name", "benchmark")
        git(repo, "config", "user.email", "benchmark@local.invalid")
        (repo / "app.txt").write_text("base")
        git(repo, "add", "app.txt")
        git(repo, "commit", "-m", "base")
        store = CliStore(repo)
        store.ensure()
        store.spawn("sample", expected_paths=["app.txt"])
        for label, program in programs.items():
            results[label] = {"program": program, "size_bytes": Path(program[0]).stat().st_size if len(program) == 1 else None, "executable_sha256":hashlib.sha256(Path(program[0]).read_bytes()).hexdigest(), "measurements": {}}
            for operation, command in (("version", ["--version"]), ("schema", ["schema"]), ("validate", ["--project", str(repo), "validate", "sample"])):
                measure(program + command)  # warmed filesystem/process image
                samples = [measure(program + command) for _ in range(args.runs)]
                medians = {key: statistics.median([s[key] for s in samples if s[key] is not None]) if any(s[key] is not None for s in samples) else None for key in samples[0]}
                results[label]["measurements"][operation] = {"median":medians, "samples":samples}
    report = {"format":"lane-native-benchmark/v1", "platform":platform.platform(), "python":sys.version, "runs":args.runs, "results":results, "scope":"Warmed repeated invocations. CPU and peak memory cover the direct process only, not Git children. Wall time includes all children. Windows CPU timing can round to zero for short operations. Not an absolute ranking of languages."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({label: {op:v["median"] for op,v in data["measurements"].items()} for label,data in results.items()}, indent=2))


if __name__ == "__main__":
    main()
