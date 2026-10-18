"""Linux Docker smoke test for the exact release binary, without real tokens/data."""

import http.server
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True).strip()


def wait_for(check):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.1)
    raise AssertionError("Timed out waiting for nginx/agent state")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    binary = Path(sys.argv[1]).resolve()
    accepted = threading.Event()
    delivered = threading.Event()
    marker = f"release-smoke-{uuid.uuid4().hex}"
    payloads = []

    class Relay(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            if self.path == "/v1/batches":
                if not accepted.is_set():
                    self.send_response(503)
                else:
                    decoded = subprocess.check_output(["zstd", "-d", "-c"], input=body)
                    payloads.append(decoded)
                    data = b"".join(payloads)
                    if all(value in data for value in (marker.encode(), b"stdout", b"stderr", b"404", b"open()")):
                        delivered.set()
                    self.send_response(202)
            else:
                self.send_response(200)
            self.end_headers()

        def log_message(self, *_):
            pass

    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Relay) as relay:
        threading.Thread(target=relay.serve_forever, daemon=True).start()
        try:
            for image in ("nginx:stable", "nginx:stable-alpine"):
                accepted.clear()
                delivered.clear()
                payloads.clear()
                name = f"lognara-release-{uuid.uuid4().hex}"
                volume = f"{name}-spool"
                port, agent_port = free_port(), free_port()
                with tempfile.TemporaryDirectory() as directory:
                    config = Path(directory) / "default.conf"
                    config.write_text(f"server {{ listen {port}; root /usr/share/nginx/html; }}\n")
                    try:
                        docker("run", "-d", "--name", name, "--init", "--network", "host",
                               "-v", f"{binary}:/usr/local/bin/lognara-agent:ro",
                               "-v", f"{config}:/etc/nginx/conf.d/default.conf:ro",
                               "-v", f"{volume}:/var/lib/lognara-agent/spool",
                               "-e", "LOGNARA_SERVICE=release-test", "-e", "LOGNARA_SERVER=ci",
                               "-e", "LOGNARA_BACKEND=test", "-e", "LOGNARA_RELAY_TOKEN=test-only",
                               "-e", f"LOGNARA_LISTEN_ADDR=127.0.0.1:{agent_port}",
                               "-e", f"LOGNARA_RELAY_URL=http://127.0.0.1:{relay.server_port}/v1/batches",
                               "-e", "LOGNARA_FLUSH_INTERVAL_MS=100",
                               "--entrypoint", "/usr/local/bin/lognara-agent", image,
                               "run", "--", "/docker-entrypoint.sh", "nginx", "-g", "daemon off;")

                        def request():
                            try:
                                urllib.request.urlopen(f"http://127.0.0.1:{port}/{marker}", timeout=1)
                            except urllib.error.HTTPError as error:
                                return error.code == 404
                            except OSError:
                                return False
                            return False

                        wait_for(request)
                        wait_for(lambda: marker in docker("logs", name))
                        docker("stop", "-t", "30", name)
                        assert docker("inspect", "-f", "{{.State.ExitCode}}", name) == "0"
                        # No batch has been ACKed. Restart the same container/volume,
                        # then require recovery of the request issued before shutdown.
                        accepted.set()
                        docker("start", name)
                        wait_for(delivered.is_set)
                        data = b"".join(payloads)
                        assert b"stdout" in data and b"stderr" in data, data
                        assert b"404" in data and b"open()" in data, data
                        logs = docker("logs", name)
                        assert "Configuration complete; ready for start up" in logs
                        docker("stop", "-t", "30", name)
                        assert docker("inspect", "-f", "{{.State.ExitCode}}", name) == "0"
                        print(f"{image}: entrypoint, access/error delivery, recovery and shutdown OK")
                    finally:
                        subprocess.run(["docker", "rm", "-f", name], check=False)
                        subprocess.run(["docker", "volume", "rm", volume], check=False)
        finally:
            relay.shutdown()


if __name__ == "__main__":
    main()
