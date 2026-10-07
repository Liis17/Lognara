#!/usr/bin/env python3
"""Run a prebuilt memory-profile test, stopping it if RSS exceeds the threshold."""

import argparse
import json
from pathlib import Path
import resource
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", help="prebuilt test executable; otherwise build it first")
    parser.add_argument("--limit-mib", type=int, default=2048)
    args = parser.parse_args()
    if args.limit_mib <= 0:
        parser.error("--limit-mib must be positive")

    if args.binary is None:
        build = subprocess.run(
            ["cargo", "test", "--locked", "--test", "memory_profile", "--no-run", "--message-format=json"],
            cwd=Path(__file__).resolve().parent.parent,
            stdout=subprocess.PIPE, text=True, check=False,
        )
        if build.returncode:
            return build.returncode
        for line in build.stdout.splitlines():
            artifact = json.loads(line)
            if artifact.get("target", {}).get("name") == "memory_profile" and artifact.get("executable"):
                # A fresh wrapper excludes compiler children from the kernel RSS peak.
                return subprocess.call([
                    sys.executable, str(Path(__file__).resolve()), artifact["executable"],
                    "--limit-mib", str(args.limit_mib),
                ])
        parser.error("cargo did not produce the memory_profile test executable")

    limit = args.limit_mib * 1024 * 1024
    peak = 0
    exceeded = False
    process = subprocess.Popen([
        args.binary, "--exact", "bounded_relay_memory_profile", "--ignored",
        "--nocapture", "--test-threads=1",
    ])
    try:
        while process.poll() is None:
            sample = subprocess.run(
                ["ps", "-o", "rss=", "-p", str(process.pid)],
                capture_output=True, text=True, check=False,
            )
            if sample.stdout.strip():
                peak = max(peak, int(sample.stdout.strip()) * 1024)
                if peak > limit:
                    exceeded = True
                    process.kill()
                    break
            time.sleep(0.02)
        status = process.wait()
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()

    # Kernel high-water mark catches peaks between samples. macOS reports bytes;
    # Linux reports KiB. Only the prebuilt test and small ps children ran here.
    high_water = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    if sys.platform != "darwin":
        high_water *= 1024
    peak = max(peak, high_water)
    exceeded |= peak > limit
    print(f"Peak RSS: {peak / 1024 / 1024:.1f} MiB; threshold: {args.limit_mib} MiB", flush=True)
    if exceeded:
        print("RSS threshold exceeded", file=sys.stderr)
        return 1
    return status if status >= 0 else 1


if __name__ == "__main__":
    sys.exit(main())
