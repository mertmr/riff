#!/usr/bin/env python3
"""Check release startup and idle RSS in an isolated empty library (macOS/Linux).

Build with ui-qa first. The instrumented binary measures main() through its
first rendered frame, not compositor latency. Each run owns only the
process it launches; existing Riff sessions and saved collections are untouched.
"""
import argparse
import json
import math
import os
from pathlib import Path
import platform
import re
import runpy
import statistics
import subprocess
import tempfile
import time

process_snapshot = runpy.run_path(
    str(Path(__file__).with_name("measure-memory.py"))
)["process_snapshot"]


def positive(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be finite and positive")
    return number


def measure(binary, seconds, startup_limit, artwork):
    with tempfile.TemporaryDirectory(prefix="riff-performance-") as directory:
        root = Path(directory)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("RIFF_")}
        env.update(RIFF_QA_CONFIG_DIR=str(root / "config"), RIFF_QA_STARTUP="1")
        if artwork:
            env.update(RIFF_QA_ARTWORK="1", RIFF_FIXTURES="1")
        with (root / "stdout").open("w+") as output, (root / "stderr").open("w+") as errors:
            start = time.monotonic()
            process = subprocess.Popen([str(binary)], env=env, stdout=output, stderr=errors)
            samples = []
            startup = None
            try:
                while startup is None or time.monotonic() - start < seconds:
                    if process.poll() is not None:
                        errors.seek(0)
                        raise RuntimeError(f"Riff exited with {process.returncode}: {errors.read()}")
                    if startup is None:
                        output.seek(0)
                        for line in output.read().splitlines():
                            try:
                                marker = json.loads(line)
                            except json.JSONDecodeError:
                                continue
                            if isinstance(marker, dict) and "first_frame_ms" in marker:
                                startup = marker
                                startup["launch_to_frame_ms"] = round((time.monotonic() - start) * 1000, 2)
                                break
                        if startup is None and time.monotonic() - start > startup_limit:
                            raise RuntimeError("No startup marker; build with --features ui-qa, and check the native display")
                    processes = process_snapshot()
                    descendants = {process.pid}
                    while True:
                        expanded = descendants | {pid for pid, (parent, _, _) in processes.items()
                                                  if parent in descendants}
                        if expanded == descendants:
                            break
                        descendants = expanded
                    if process.pid in processes:
                        samples.append({
                            "seconds": round(time.monotonic() - start, 3),
                            "riff_rss_mib": round(processes[process.pid][1], 2),
                            "total_rss_mib": round(sum(processes[pid][1] for pid in descendants
                                                       if pid in processes), 2),
                            "child_count": len(descendants) - 1,
                        })
                    time.sleep(0.02 if startup is None else 0.2)
                if not samples:
                    raise RuntimeError("No memory samples collected")
                result = {
                    **startup,
                    "riff_peak_rss_mib": max(sample["riff_rss_mib"] for sample in samples),
                    "combined_peak_rss_mib": max(sample["total_rss_mib"] for sample in samples),
                    "idle_rss_mib": samples[-1]["riff_rss_mib"],
                    "samples": samples,
                }
                if artwork:
                    output.seek(0)
                    loaded = 0
                    for line in output.read().splitlines():
                        try:
                            marker = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if isinstance(marker, dict):
                            loaded = max(loaded, marker.get("artwork_loaded", 0))
                    if loaded < 6:
                        raise RuntimeError("Artwork fixture did not finish loading six covers; allow more --seconds or check the connection")
                    result["artwork_loaded"] = loaded
                if platform.system() == "Darwin":
                    summary = subprocess.run(["vmmap", "-summary", str(process.pid)],
                                             capture_output=True, text=True, timeout=15)
                    match = re.search(r"Physical footprint:\s+([\d.]+)([KMG])", summary.stdout)
                    if match:
                        result["physical_footprint_mib"] = round(float(match[1]) *
                                                                 {"K": 1 / 1024, "M": 1, "G": 1024}[match[2]], 2)
                return result
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/riff-rs"))
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--seconds", type=positive, default=5)
    parser.add_argument("--max-startup-ms", type=positive, default=1000)
    parser.add_argument("--max-rss-mib", type=positive, default=128)
    parser.add_argument("--artwork", action="store_true", help="load the representative Home artwork fixture; may use network")
    parser.add_argument("--output", type=Path, default=Path("target/performance/idle.json"))
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if not args.binary.is_file():
        parser.error("binary does not exist; cargo build --release --features ui-qa")
    runs = []
    for index in range(args.runs):
        run = measure(args.binary.resolve(), args.seconds, max(10, args.max_startup_ms / 1000), args.artwork)
        runs.append(run)
        print(f"Run {index + 1}: {run['launch_to_frame_ms']:.0f} ms, "
              f"{run['combined_peak_rss_mib']:.1f} MiB peak RSS", flush=True)
    worst_startup = max(run["launch_to_frame_ms"] for run in runs)
    worst_rss = max(run["combined_peak_rss_mib"] for run in runs)
    passed = worst_startup <= args.max_startup_ms and worst_rss <= args.max_rss_mib
    report = {
        "platform": platform.platform(), "binary": str(args.binary.resolve()),
        "scenario": "Home with artwork, paused fixture; no playback" if args.artwork else "empty-library native startup and idle; no playback or network requests",
        "startup_metric": "process launch through first native rendered frame; includes polling overhead, excludes compositor latency",
        "memory_metric": "process-tree RSS, MiB; shared pages may be counted more than once",
        "median_startup_ms": round(statistics.median(run["launch_to_frame_ms"] for run in runs), 2),
        "worst_startup_ms": worst_startup, "peak_rss_mib": worst_rss,
        "budgets": {"startup_ms": args.max_startup_ms, "rss_mib": args.max_rss_mib},
        "passed": passed, "runs": runs,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({key: value for key, value in report.items() if key != "runs"}, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
