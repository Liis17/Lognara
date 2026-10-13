"""Проверяет работающий Compose: HTTP-запрос должен появиться в поиске core."""

import base64
import json
import time
import uuid
from urllib.request import Request, urlopen


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
                print(json.dumps(event, ensure_ascii=False, indent=2))
                return
        time.sleep(1)
    raise SystemExit("FAIL: request did not appear in core search within 30 seconds")


if __name__ == "__main__":
    main()
