"""Проверяет работающий Compose: HTTP-запрос должен появиться в core и ленте dock."""

import base64
import json
import os
import time
import uuid
from http.client import HTTPConnection
from http.cookies import SimpleCookie
from urllib.request import Request, urlopen


def check_dock(event):
    origin = "http://localhost:7403"
    with urlopen(origin + "/", timeout=10) as response:
        assert b'<div id="root"></div>' in response.read(), "dock SPA is missing"
    request = Request(origin + "/api/auth/login", data=json.dumps({
        "username": "demo",
        "password": os.environ.get("LOGNARA_EXAMPLE_PASSWORD", "demo-lognara-local"),
    }).encode(), headers={"Content-Type": "application/json", "Origin": origin})
    with urlopen(request, timeout=10) as response:
        assert json.load(response)["username"] == "demo"
        cookies = SimpleCookie(response.headers["Set-Cookie"])
    # Secure-cookie передаём явно: urllib не делает исключения для HTTP localhost.
    cookie = "lognara_dock_session=" + cookies["lognara_dock_session"].value
    request = Request(origin + "/api/me", headers={"Cookie": cookie})
    with urlopen(request, timeout=10) as response:
        assert json.load(response)["username"] == "demo"

    connection = HTTPConnection("localhost", 7403, timeout=30)
    try:
        connection.request("GET", "/api/logs/stream", headers={
            "Cookie": cookie, "Accept": "text/event-stream",
        })
        response = connection.getresponse()
        assert response.status == 200, "dock stream requires a valid session"
        assert response.headers.get_content_type() == "text/event-stream"
        deadline = time.monotonic() + 30
        name, data = "", []
        while time.monotonic() < deadline:
            connection.sock.settimeout(max(0.1, deadline - time.monotonic()))
            raw = response.readline()
            if not raw:
                break
            line = raw.decode().rstrip("\r\n")
            if not line:
                if name == "core_error":
                    raise SystemExit("FAIL: dock cannot query core: " + "\n".join(data))
                if name == "logs":
                    for item in json.loads("\n".join(data)):
                        if item["id"] == event["id"]:
                            assert item["attributes"] == event["attributes"]
                            print("OK: dock SPA -> login -> session -> live log stream")
                            return
                name, data = "", []
            elif line.startswith("event:"):
                name = line[6:].strip()
            elif line.startswith("data:"):
                data.append(line[5:].lstrip())
    except TimeoutError:
        pass
    finally:
        connection.close()
    raise SystemExit("FAIL: request did not appear in dock stream within 30 seconds")


def main():
    path = f"/smoke-test/{uuid.uuid4().hex}?client=local"
    body = b"Lognara example request\x00\xff"
    request = Request("http://127.0.0.1:8881" + path, data=body, method="CUSTOM",
                      headers={"X-Example-Client": "smoke-test"})
    with urlopen(request, timeout=10) as response:
        assert response.status == 200
    query = json.dumps({
        "from": "2020-01-01T00:00:00Z",
        "to": "2100-01-01T00:00:00Z",
        "filters": {"service": ["python-example"]},
        "text": {"query": path.split("/")[-1].split("?")[0], "mode": "all"},
    }).encode()
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        request = Request("http://127.0.0.1:7402/v1/logs/search", data=query, headers={
            "Authorization": "Bearer local-query-token",
            "Content-Type": "application/json",
        })
        with urlopen(request, timeout=10) as response:
            events = json.load(response)["events"]
        for event in events:
            attrs = event["attributes"]
            if attrs.get("path") == path:
                assert attrs["method"] == "CUSTOM"
                assert attrs["client_ip"] and attrs["client_port"] > 0
                assert base64.b64decode(attrs["body_base64"]) == body
                print("OK: request -> Python -> agent -> relay -> core search")
                print(json.dumps(event, indent=2))
                check_dock(event)
                return
        time.sleep(1)
    raise SystemExit("FAIL: request did not appear in core search within 30 seconds")


if __name__ == "__main__":
    main()
