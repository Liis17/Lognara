import base64
import contextlib
import io
import json
import sys
import threading
import unittest
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from queue import Queue

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from server import RequestHandler


class ServerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.logs = Queue()

        class Agent(BaseHTTPRequestHandler):
            status = 202

            def do_POST(self):
                cls.logs.put((self.path, self.headers.get("Content-Type"), json.loads(
                    self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(self.status)
                self.end_headers()

            def log_message(self, *args):
                pass

        cls.agent_handler = Agent
        cls.agent = ThreadingHTTPServer(("127.0.0.1", 0), Agent)

        class Handler(RequestHandler):
            agent_url = f"http://127.0.0.1:{cls.agent.server_port}/v1/logs"

        cls.app = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.threads = []
        for server in (cls.agent, cls.app):
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            cls.threads.append(thread)

    @classmethod
    def tearDownClass(cls):
        for server in (cls.app, cls.agent):
            server.shutdown()
            server.server_close()
        for thread in cls.threads:
            thread.join()

    def request(self, method, body=None, chunked=False):
        connection = HTTPConnection("127.0.0.1", self.app.server_port, timeout=3)
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            connection.request(method, "/any/path?user=demo", body=body,
                               headers={"X-Test-Client": "example"}, encode_chunked=chunked)
            response = connection.getresponse()
            status, response_body = response.status, response.read()
        connection.close()
        return status, response_body, output.getvalue()

    def test_all_methods_and_paths_are_logged_and_forwarded(self):
        for method in ("GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD", "TRACE", "CONNECT", "CUSTOM"):
            with self.subTest(method=method):
                status, body, output = self.request(method, b"hello")
                self.assertEqual(status, 200)
                self.assertEqual(body, b"" if method == "HEAD" else b'{"status":"ok"}\n')
                path, content_type, event = self.logs.get(timeout=1)
                self.assertEqual(path, "/v1/logs")
                self.assertEqual(content_type, "application/json")
                self.assertEqual(json.loads(output), event)
                attrs = event["attributes"]
                self.assertEqual(attrs["client_ip"], "127.0.0.1")
                self.assertGreater(attrs["client_port"], 0)
                self.assertEqual(attrs["method"], method)
                self.assertEqual(attrs["path"], "/any/path?user=demo")
                self.assertEqual(attrs["body"], "hello")
                self.assertIn(["X-Test-Client", "example"], attrs["headers"])

    def test_chunked_binary_body_is_preserved(self):
        status, _, _ = self.request("CUSTOM", iter([b"first", b"\x00\xfflast"]), chunked=True)
        self.assertEqual(status, 200)
        event = self.logs.get(timeout=1)[2]
        self.assertEqual(base64.b64decode(event["attributes"]["body_base64"]), b"first\x00\xfflast")

    def test_agent_failure_returns_retryable_status_and_keeps_stdout_log(self):
        self.agent_handler.status = 503
        try:
            status, _, output = self.request("POST", b"retry me")
            self.assertEqual(status, 503)
            self.assertEqual(json.loads(output)["attributes"]["body"], "retry me")
            self.logs.get(timeout=1)
        finally:
            self.agent_handler.status = 202


if __name__ == "__main__":
    unittest.main()
