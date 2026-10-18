import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError, URLError

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import next_image_version as release


def http_error(code, payload=None, headers=None):
    return HTTPError(
        "https://docker.barkfluff.com:443/v2/lognara-core/tags/list", code,
        "Registry error", headers or {}, io.BytesIO(json.dumps(payload or {}).encode()),
    )


class VersionTests(unittest.TestCase):
    def test_first_release(self):
        self.assertEqual(release.next_version([]), "0.0.1")
        self.assertEqual(release.next_version(["master", "sha-abc", "latest"]), "0.0.1")

    def test_numeric_order_and_patch_increment(self):
        self.assertEqual(release.next_version(["1.9.99", "1.10.9", "1.10.10"]), "1.10.11")
        self.assertEqual(release.next_version(["1.99.99", "2.0.0"]), "2.0.1")

    def test_non_release_tags_do_not_affect_version(self):
        self.assertEqual(release.next_version([
            "0.0.3", "99.0.0-rc.1", "99.0.0+build", "v99.0.0", "01.0.0", "master",
        ]), "0.0.4")

    @patch.object(release, "request_json")
    def test_versions_are_independent_between_repositories(self, request):
        request.side_effect = [({"tags": ["0.0.7"]}, {}), ({"tags": ["1.2.3"]}, {})]
        self.assertEqual(release.next_version(release.registry_tags("lognara-core", "u", "p")), "0.0.8")
        self.assertEqual(release.next_version(release.registry_tags("lognara-dock", "u", "p")), "1.2.4")
        self.assertIn("/v2/lognara-core/", request.call_args_list[0].args[0])
        self.assertIn("/v2/lognara-dock/", request.call_args_list[1].args[0])


class RegistryTests(unittest.TestCase):
    @patch.object(release, "request_json")
    def test_pagination_accepts_default_https_port(self, request):
        request.side_effect = [
            ({"tags": ["0.0.1"]}, {"Link": '<https://docker.barkfluff.com/v2/lognara-core/tags/list?last=0.0.1>; rel="next"'}),
            ({"tags": ["0.0.2"]}, {}),
        ]
        self.assertEqual(release.registry_tags("lognara-core", "u", "p"), ["0.0.1", "0.0.2"])

    @patch.object(release, "request_json")
    def test_reads_all_pages_with_basic_credentials(self, request):
        request.side_effect = [
            ({"tags": ["0.0.9"]}, {"Link": '</v2/lognara-core/tags/list?n=1&last=0.0.9>; rel="next"'}),
            ({"tags": ["0.0.10", "master"]}, {}),
        ]
        tags = release.registry_tags("lognara-core", "user", "pass")
        self.assertEqual(release.next_version(tags), "0.0.11")
        self.assertEqual(request.call_args_list[0].args[1], "Basic dXNlcjpwYXNz")
        self.assertTrue(request.call_args_list[1].args[0].endswith("n=1&last=0.0.9"))

    @patch.object(release, "request_json")
    def test_missing_repository_is_first_release(self, request):
        request.side_effect = http_error(404, {"errors": [{"code": "NAME_UNKNOWN"}]})
        self.assertEqual(release.registry_tags("lognara-core", "u", "p"), [])

    @patch.object(release, "request_json", return_value=({"tags": None}, {}))
    def test_empty_repository(self, request):
        self.assertEqual(release.registry_tags("lognara-core", "u", "p"), [])

    @patch.object(release, "request_json")
    def test_http_errors_cannot_reset_version(self, request):
        for code in (401, 403, 404, 429, 500):
            with self.subTest(code=code):
                request.side_effect = http_error(code)
                with self.assertRaises((HTTPError, ValueError)):
                    release.registry_tags("lognara-core", "u", "p")

    @patch.object(release, "request_json", side_effect=URLError("unavailable"))
    def test_network_failure_cannot_reset_version(self, request):
        with self.assertRaises(URLError):
            release.registry_tags("lognara-core", "u", "p")

    @patch.object(release, "request_json")
    def test_bearer_authentication(self, request):
        request.side_effect = [
            http_error(401, headers={"WWW-Authenticate":
                'Bearer realm="https://auth.example/token",service="registry",scope="repository:lognara-core:pull"'}),
            ({"token": "test-token"}, {}),
            ({"tags": ["0.0.1"]}, {}),
        ]
        self.assertEqual(release.registry_tags("lognara-core", "u", "p"), ["0.0.1"])
        self.assertIn("scope=repository%3Alognara-core%3Apull", request.call_args_list[1].args[0])
        self.assertEqual(request.call_args_list[2].args[1], "Bearer test-token")

    @patch.object(release, "request_json")
    def test_failed_token_authentication_stops_release(self, request):
        request.side_effect = [
            http_error(401, headers={"WWW-Authenticate": 'Bearer realm="https://auth.example/token"'}),
            http_error(403),
        ]
        with self.assertRaises(HTTPError):
            release.registry_tags("lognara-core", "u", "p")

    @patch.object(release, "request_json")
    def test_invalid_registry_payload_stops_release(self, request):
        for payload in ({}, {"tags": "invalid"}, {"tags": [42]}):
            with self.subTest(payload=payload):
                request.return_value = payload, {}
                with self.assertRaises((KeyError, ValueError)):
                    release.registry_tags("lognara-core", "u", "p")

    @patch.object(release, "request_json")
    def test_pagination_cannot_send_credentials_to_another_server(self, request):
        request.return_value = {"tags": []}, {"Link": '<https://other.example/tags>; rel="next"'}
        with self.assertRaises(ValueError):
            release.registry_tags("lognara-core", "u", "p")
        self.assertEqual(request.call_count, 1)

    def test_token_service_must_use_https(self):
        with self.assertRaises(ValueError):
            release.bearer_authorization('Bearer realm="http://auth.example/token"', "Basic test")


class OutputTests(unittest.TestCase):
    @patch.object(release, "registry_tags", return_value=["0.0.9"])
    def test_writes_version_for_actions(self, tags):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.dict(os.environ, {
                "COMPONENT": "core", "REGISTRY_USERNAME": "u", "REGISTRY_PASSWORD": "p",
                "GITHUB_OUTPUT": str(output),
            }):
                release.main()
            self.assertEqual(output.read_text(), "version=0.0.10\n")
            tags.assert_called_once_with("lognara-core", "u", "p")

    @patch.object(release, "registry_tags", side_effect=URLError("unavailable"))
    def test_failure_does_not_emit_version(self, tags):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.dict(os.environ, {
                "COMPONENT": "core", "REGISTRY_USERNAME": "u", "REGISTRY_PASSWORD": "p",
                "GITHUB_OUTPUT": str(output),
            }), self.assertRaises(URLError):
                release.main()
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
