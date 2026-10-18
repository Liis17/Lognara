"""Read registry tags and increment the greatest stable release's patch number."""

import base64
import json
import os
import re
from urllib.error import HTTPError
from urllib.parse import urlencode, urljoin, urlsplit
from urllib.request import (
    HTTPRedirectHandler, Request, build_opener, parse_http_list, parse_keqv_list,
)


REGISTRY = "https://docker.barkfluff.com:443"
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def request_json(url, authorization):
    # Cloudflare перед реестром отвечает 403 на User-Agent Python-urllib.
    headers = {"Authorization": authorization, "Accept": "application/json", "User-Agent": "lognara-release"}
    request = Request(url, headers=headers)
    with build_opener(NoRedirect()).open(request, timeout=30) as response:
        return json.load(response), response.headers


def bearer_authorization(challenge, basic):
    scheme, _, parameters = challenge.partition(" ")
    if scheme.lower() != "bearer":
        raise ValueError("Registry rejected credentials")
    parameters = parse_keqv_list(parse_http_list(parameters))
    realm = parameters.pop("realm")
    if urlsplit(realm).scheme != "https":
        raise ValueError("Registry token service must use HTTPS")
    query = urlencode({key: parameters[key] for key in ("service", "scope") if key in parameters})
    token_url = realm + ("&" if "?" in realm else "?") + query
    payload, _ = request_json(token_url, basic)
    token = payload.get("token") or payload.get("access_token")
    if not isinstance(token, str) or not token:
        raise ValueError("Registry token service returned no token")
    return "Bearer " + token


def registry_tags(repository, username, password):
    credentials = base64.b64encode(f"{username}:{password}".encode()).decode()
    basic = "Basic " + credentials
    authorization = basic
    url = f"{REGISTRY}/v2/{repository}/tags/list?n=1000"
    tags = []
    while url:
        try:
            try:
                payload, headers = request_json(url, authorization)
            except HTTPError as error:
                if error.code != 401 or authorization != basic:
                    raise
                authorization = bearer_authorization(error.headers.get("WWW-Authenticate", ""), basic)
                payload, headers = request_json(url, authorization)
        except HTTPError as error:
            if error.code == 404 and not tags:
                payload = json.load(error)
                if any(item.get("code") == "NAME_UNKNOWN" for item in payload.get("errors", [])):
                    return []
            raise
        page = payload["tags"]
        if page is not None and (not isinstance(page, list) or any(not isinstance(tag, str) for tag in page)):
            raise ValueError("Registry returned an invalid tag list")
        tags.extend(page or [])
        link = re.search(r'<([^>]+)>;\s*rel="?next"?', headers.get("Link", ""))
        next_url = urljoin(url, link.group(1)) if link else None
        if next_url:
            target, origin = urlsplit(next_url), urlsplit(REGISTRY)
            if target.scheme != "https" or target.hostname != origin.hostname or (target.port or 443) != 443:
                raise ValueError("Registry pagination points outside the registry")
        url = next_url
    return tags


def next_version(tags):
    versions = [tuple(map(int, tag.split("."))) for tag in tags if VERSION.fullmatch(tag)]
    major, minor, patch = max(versions, default=(0, 0, 0))
    return f"{major}.{minor}.{patch + 1}"


def main():
    component = os.environ["COMPONENT"]
    if component not in ("core", "relay", "dock"):
        raise ValueError("Unknown container component")
    username, password = os.environ["REGISTRY_USERNAME"], os.environ["REGISTRY_PASSWORD"]
    if not username or not password:
        raise ValueError("REGISTRY_USERNAME and REGISTRY_PASSWORD are required")
    version = next_version(registry_tags(f"lognara-{component}", username, password))
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        print(f"version={version}", file=output)
    print(f"Next lognara-{component} release: {version}")


if __name__ == "__main__":
    main()
