#!/usr/bin/env python3
"""Local HTTP benchmark; uses built release binaries, never deletes data directories."""
import argparse
import json
import os
from pathlib import Path
import platform
import secrets
import shutil
import signal
import subprocess
import time
import urllib.request


def tree_bytes(path):
    total = 0
    for root, _, files in os.walk(path):
        for name in files:
            try:
                total += (Path(root) / name).stat().st_size
            except FileNotFoundError:  # WAL/retention may remove a file between samples.
                pass
    return total


def get(url, token=None):
    request = urllib.request.Request(url)
    if token:
        request.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.read().decode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--seconds", default=1800, type=int)
    parser.add_argument("--rate", default=10000, type=int)
    parser.add_argument("--port", default=17403, type=int)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    data = args.output.resolve() / "data"
    crate = Path(__file__).resolve().parents[1]
    url = f"http://127.0.0.1:{args.port}"
    env = dict(os.environ, LOGNARA_DATA_DIR=str(data), LOGNARA_LISTEN_ADDR=f"127.0.0.1:{args.port}",
               LOGNARA_INGEST_TOKEN=secrets.token_urlsafe(32), LOGNARA_QUERY_TOKEN=secrets.token_urlsafe(32),
               LOGNARA_BENCH_URL=url, LOGNARA_BENCH_SECONDS=str(args.seconds), LOGNARA_BENCH_RATE=str(args.rate),
               LOGNARA_BENCH_REPORT=str(args.output.resolve() / "latencies.json"))
    metadata = {"platform": platform.platform(), "cpu_count": os.cpu_count(), "rate": args.rate,
                "seconds": args.seconds, "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=crate, text=True).strip(),
                "config_overrides": {key: value for key, value in os.environ.items()
                                     if key.startswith("LOGNARA_") and "TOKEN" not in key}}
    if platform.system() == "Darwin":
        metadata["memory_bytes"] = int(subprocess.check_output(["sysctl", "-n", "hw.memsize"]))
    else:
        metadata["memory_bytes"] = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    (args.output / "host.json").write_text(json.dumps(metadata, indent=2))
    client = None
    io_monitor = None
    with (args.output / "server.log").open("w") as log, (args.output / "client.log").open("w") as load_log, \
            (args.output / "resources.jsonl").open("w", buffering=1) as samples, \
            (args.output / "iostat.log").open("w") as io_log:
        server = subprocess.Popen([str(crate / "target/release/lognara-core")], env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 60
            while True:
                try:
                    get(url + "/health/ready")
                    break
                except OSError:
                    if server.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("core failed to become ready; see server.log")
                    time.sleep(0.1)
            iostat = shutil.which("iostat") or ("/usr/sbin/iostat" if Path("/usr/sbin/iostat").exists() else None)
            if iostat:
                command = [iostat, "-d", "-w", "5"] if platform.system() == "Darwin" else [iostat, "-dx", "5"]
                io_monitor = subprocess.Popen(command, stdout=io_log, stderr=io_log)
            client = subprocess.Popen([str(crate / "target/release/examples/load")], env=env, stdout=load_log, stderr=load_log)
            started = time.monotonic()
            while client.poll() is None:
                ps = subprocess.check_output(["ps", "-p", str(server.pid), "-o", "rss=", "-o", "%cpu="], text=True).split()
                sample = {"seconds": time.monotonic() - started, "rss_bytes": int(ps[0]) * 1024,
                          "cpu_percent": float(ps[1]), "data_bytes": tree_bytes(data), "wal_bytes": tree_bytes(data / "wal"),
                          "disk_free_bytes": shutil.disk_usage(data).free,
                          "metrics": get(url + "/metrics", env["LOGNARA_QUERY_TOKEN"])}
                samples.write(json.dumps(sample) + "\n")
                time.sleep(5)
            if client.returncode:
                raise RuntimeError("load client failed; see client.log")
        finally:
            if client and client.poll() is None:
                client.terminate()
                client.wait(timeout=40)
            if io_monitor:
                io_monitor.terminate()
                io_monitor.wait(timeout=10)
            if server.poll() is None:
                server.send_signal(signal.SIGTERM)
                try:
                    server.wait(timeout=120)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()
                    raise RuntimeError("core did not finish graceful shutdown")
            (args.output / "storage.json").write_text(json.dumps({"data_bytes": tree_bytes(data),
                "wal_bytes": tree_bytes(data / "wal"), "server_exit_code": server.returncode}, indent=2))
    print(f"Benchmark artifacts: {args.output}")


if __name__ == "__main__":
    main()
