#!/usr/bin/env python3
"""Sample an existing Riff process and all children. No playback/UI changes."""
import argparse
import json
import re
import subprocess
import sys
import time
from pathlib import Path


def process_snapshot():
    output = subprocess.check_output(
        ["ps", "-axo", "pid=,ppid=,rss=,comm="], text=True
    )
    processes = {}
    for line in output.splitlines():
        fields = line.strip().split(None, 3)
        if len(fields) == 4:
            pid, parent, rss, executable = fields
            processes[int(pid)] = (int(parent), int(rss) / 1024, executable)
    return processes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int, help="running Riff PID")
    parser.add_argument("--seconds", type=float, default=65)
    parser.add_argument("--output", type=Path, default=Path("target/memory/live.json"))
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error("--seconds must be positive")
    if args.pid not in process_snapshot():
        parser.error("PID is not running")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    samples = []
    start = time.monotonic()
    while time.monotonic() - start < args.seconds:
        processes = process_snapshot()
        if args.pid not in processes:
            break
        descendants = {args.pid}
        while True:
            previous = len(descendants)
            descendants.update(
                pid for pid, (parent, _, _) in processes.items()
                if parent in descendants
            )
            if len(descendants) == previous:
                break
        children = [
            {"pid": pid, "rss_mib": round(processes[pid][1], 2),
             "executable": processes[pid][2]}
            for pid in sorted(descendants - {args.pid})
        ]
        main_rss = processes[args.pid][1]
        children_rss = sum(processes[pid][1] for pid in descendants - {args.pid})
        samples.append({
            "seconds": round(time.monotonic() - start, 2),
            "riff_rss_mib": round(main_rss, 2),
            "children_rss_mib": round(children_rss, 2),
            "total_rss_mib": round(main_rss + children_rss, 2),
            "children": children,
        })
        time.sleep(min(1, max(0, args.seconds - (time.monotonic() - start))))
    report = {
        "pid": args.pid, "metric": "resident set size (RSS), MiB",
        "duration_seconds": round(time.monotonic() - start, 2),
        "sample_count": len(samples),
        "riff_peak_mib": max(s["riff_rss_mib"] for s in samples),
        "children_peak_mib": max(s["children_rss_mib"] for s in samples),
        "combined_peak_mib": max(s["total_rss_mib"] for s in samples),
        "last": samples[-1],
        "notes": ["RSS includes shared pages; it is not macOS physical footprint.",
                  "Start sampling before playback to include startup and yt-dlp peaks."],
    }
    if sys.platform == "darwin":
        footprints = {}
        for pid in sorted(descendants):
            result = subprocess.run(
                ["vmmap", "-summary", str(pid)], capture_output=True, text=True
            )
            if result.returncode == 0:
                path = args.output.with_name(f"{args.output.stem}-{pid}-vmmap.txt")
                path.write_text(result.stdout)
                footprints[str(pid)] = {
                    "summary": str(path),
                    "values": re.findall(r"Physical footprint[^\n]*", result.stdout),
                }
        report["macos_physical_footprint"] = footprints
    print(json.dumps(report, indent=2))
    report["samples"] = samples
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
