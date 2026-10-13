"""Локальный HTTP-сервер: JSON в stdout и POST в агент того же контейнера."""

import base64
import json
import sys
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.request import Request, urlopen


class RequestHandler(BaseHTTPRequestHandler):
    agent_url = "http://127.0.0.1:7400/v1/logs"

    def __getattr__(self, name):
        if name.startswith("do_"):
            return self.handle_request
        raise AttributeError(name)

    def read_body(self):
        if self.headers.get("Transfer-Encoding", "").lower() == "chunked":
            body = bytearray()
            while True:
                size = int(self.rfile.readline().split(b";", 1)[0], 16)
                if size < 0:
                    raise ValueError("negative chunk size")
                if size == 0:
                    while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                        pass
                    return bytes(body)
                chunk = self.rfile.read(size)
                if len(chunk) != size or self.rfile.read(2) != b"\r\n":
                    raise ValueError("incomplete chunk")
                body.extend(chunk)
        size = int(self.headers.get("Content-Length", "0"))
        if size < 0:
            raise ValueError("negative content length")
        body = self.rfile.read(size)
        if len(body) != size:
            raise ValueError("incomplete body")
        return body

    def handle_request(self):
        self.connection.settimeout(10)
        try:
            body = self.read_body()
        except (ValueError, TimeoutError) as error:
            self.send_error(400, str(error))
            return
        event = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "level": "info",
            "message": f"{self.client_address[0]}:{self.client_address[1]} {self.command} {self.path}",
            "attributes": {
                "client_ip": self.client_address[0],
                "client_port": self.client_address[1],
                "method": self.command,
                "path": self.path,
                "headers": list(self.headers.items()),
                "body": body.decode("utf-8", errors="replace"),
                "body_base64": base64.b64encode(body).decode("ascii"),
            },
        }
        payload = json.dumps(event, ensure_ascii=False).encode("utf-8")
        print(payload.decode("utf-8"), flush=True)
        request = Request(self.agent_url, data=payload, headers={"Content-Type": "application/json"})
        try:
            with urlopen(request, timeout=5) as response:
                if response.status != 202:
                    raise URLError(f"unexpected agent status: {response.status}")
        except (URLError, TimeoutError, OSError) as error:
            print(f"Lognara agent rejected the log: {error}", file=sys.stderr, flush=True)
            self.send_error(503, "Lognara agent did not accept the log; retry the request")
            return
        response_body = b'{"status":"ok"}\n'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(response_body)

    def log_message(self, format, *args):
        pass


if __name__ == "__main__":
    with ThreadingHTTPServer(("0.0.0.0", 8881), RequestHandler) as server:
        print("Python example listening on :8881", flush=True)
        server.serve_forever()
