"""Запускает приложение и агент, передаёт остановку обоим процессам."""

import signal
import socket
import subprocess
import sys
import time
from pathlib import Path
from threading import Event


def main():
    stop = Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    agent = subprocess.Popen(["lognara-agent"])
    processes = [agent]
    try:
        deadline = time.monotonic() + 15
        while not stop.is_set() and agent.poll() is None:
            try:
                with socket.create_connection(("127.0.0.1", 7400), timeout=1):
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise RuntimeError("Lognara agent did not start on :7400")
                stop.wait(0.1)
        if stop.is_set():
            return 0
        if agent.poll() is not None:
            return agent.returncode or 1
        processes.append(subprocess.Popen([sys.executable, str(Path(__file__).with_name("server.py"))]))
        while not stop.wait(0.2):
            for process in processes:
                if process.poll() is not None:
                    return process.returncode or 1
        return 0
    finally:
        # Сначала прекращаем приём приложением, затем даём агенту отправить очередь.
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5 if process is not agent else 20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


if __name__ == "__main__":
    sys.exit(main())
