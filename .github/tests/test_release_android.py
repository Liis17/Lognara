"""Android release preparation and publication tests with isolated git histories."""

import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/release_android.py"
spec = importlib.util.spec_from_file_location("release_android", SCRIPT)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class AndroidReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.previous_directory = Path.cwd()
        os.chdir(self.directory.name)
        release.git("init", "-b", "master")
        release.git("config", "user.name", "Release Test")
        release.git("config", "user.email", "test@example.invalid")
        self.commit_count = 0
        self.first = self.commit("lognara-android-client/Main.kt", "Первый Android-клиент")

    def tearDown(self):
        os.chdir(self.previous_directory)
        self.directory.cleanup()

    def commit(self, path, message):
        file = Path(path)
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(message, encoding="utf-8")
        release.git("add", path)
        date = datetime.datetime(2026, 11, 18, tzinfo=datetime.timezone.utc)
        date += datetime.timedelta(minutes=self.commit_count)
        self.commit_count += 1
        formatted = date.strftime("%Y-%m-%dT%H:%M:%SZ")
        with patch.dict(os.environ, {
            "GIT_AUTHOR_DATE": formatted,
            "GIT_COMMITTER_DATE": formatted,
        }):
            release.git("commit", "-m", message)
        return release.git("rev-parse", "HEAD")

    def make_assets(self, directory="assets"):
        assets = Path(directory)
        assets.mkdir()
        (assets / release.APK_NAME).write_bytes(b"signed apk fixture")
        return assets

    def test_version_and_channel_tags_use_utc_commit_time(self):
        timestamp = int(release.git("show", "-s", "--format=%ct", self.first))
        expected_code = (timestamp - release.VERSION_EPOCH) // 60
        stable = release.release_version(self.first, "stable")
        dev = release.release_version(self.first, "dev")
        self.assertEqual(stable["version_code"], expected_code)
        self.assertEqual(stable["version_name"], "2026.11.18.0000")
        self.assertEqual(stable["tag"], f"android-20261118-000000-{self.first[:12]}")
        self.assertEqual(release.release_title(stable), "Lognara Android 2026.11.18.0000")
        self.assertEqual(dev["version_name"], "2026.11.18.0000-dev")
        self.assertEqual(dev["tag"], f"android-dev-20261118-000000-{self.first[:12]}")
        self.assertEqual(release.release_title(dev), "Lognara Android dev 2026.11.18.0000-dev")
        self.assertEqual(stable["commit"], self.first)

    def test_previous_release_is_selected_from_the_same_channel(self):
        stable_tag = release.release_tag(self.first, "stable")
        release.git("tag", stable_tag)
        dev_commit = self.commit("lognara-android-client/Main.kt", "Изменение dev")
        dev_tag = release.release_tag(dev_commit, "dev")
        release.git("tag", dev_tag)
        current = self.commit("lognara-android-client/Main.kt", "Текущая версия")
        releases = [
            {"tag_name": stable_tag, "draft": False, "prerelease": False},
            {"tag_name": dev_tag, "draft": False, "prerelease": True},
            {"tag_name": "agent-20261118-000000-0123456789ab",
             "draft": False, "prerelease": False},
            {"tag_name": release.release_tag(current, "dev"), "draft": True, "prerelease": True},
        ]
        stable = release.release_commits(release.published_android_releases(releases, "stable"))
        dev = release.release_commits(release.published_android_releases(releases, "dev"))
        self.assertEqual(release.previous_release(current, stable), (stable_tag, self.first))
        self.assertEqual(release.previous_release(current, dev), (dev_tag, dev_commit))

    def test_changelog_includes_only_android_commits_since_previous_release(self):
        previous_tag = release.release_tag(self.first, "stable")
        release.git("tag", previous_tag)
        self.commit("lognara-android-client/Main.kt", "Изменение Android")
        self.commit("README.md", "Только документация")
        current = self.commit("lognara-android-client/Main.kt", "Текущий Android")
        version = release.release_version(current, "stable")
        notes = release.release_notes("owner/repo", version, (previous_tag, self.first))
        self.assertIn("Изменение Android", notes)
        self.assertIn("Текущий Android", notes)
        self.assertNotIn("Первый Android-клиент", notes)
        self.assertNotIn("Только документация", notes)
        self.assertIn(f"/compare/{previous_tag}...", notes)
        self.assertIn(f"Исходный коммит: {current}.", notes)
        self.assertIn("Для установки скачайте APK", notes)

    def test_first_release_includes_android_history_but_not_other_paths(self):
        self.commit("README.md", "Только документация")
        current = self.commit("lognara-android-client/Main.kt", "Вторая Android-версия")
        version = release.release_version(current, "dev")
        notes = release.release_notes("owner/repo", version, None)
        self.assertIn("Первый выпуск Android-клиента", notes)
        self.assertIn("Первый Android-клиент", notes)
        self.assertIn("Вторая Android-версия", notes)
        self.assertNotIn("Только документация", notes)

    def test_assets_include_signed_apk_hash_and_checksums(self):
        assets = self.make_assets()
        version = release.release_version(self.first, "stable")
        metadata = release.prepare_assets(assets, version)
        expected_hash = hashlib.sha256((assets / release.APK_NAME).read_bytes()).hexdigest()
        self.assertEqual(metadata["sha256"], expected_hash)
        self.assertEqual(json.loads((assets / "version.json").read_text()), {
            "versionCode": version["version_code"],
            "versionName": version["version_name"],
            "channel": "stable",
            "commit": self.first,
            "apk": "lognara-android.apk",
            "sha256": expected_hash,
        })
        self.assertIn(f"{expected_hash}  lognara-android.apk",
                      (assets / "SHA256SUMS").read_text())

    def test_dry_run_prepares_release_assets_without_github_calls(self):
        assets = self.make_assets()
        result = subprocess.run(
            [
                sys.executable, str(SCRIPT), "publish", str(assets),
                "--repository", "owner/repo", "--sha", self.first,
                "--channel", "dev", "--dry-run",
            ],
            cwd=self.directory.name, capture_output=True, text=True, check=True,
        )
        self.assertIn("no GitHub calls made", result.stdout)
        self.assertTrue((assets / "version.json").is_file())
        self.assertTrue((assets / "SHA256SUMS").is_file())
        self.assertTrue((assets / "release-notes.md").is_file())

    def test_published_retry_leaves_assets_unchanged(self):
        tag = release.release_tag(self.first, "dev")
        release.git("tag", tag)
        existing = {"tag_name": tag, "draft": False, "prerelease": True}
        with patch.object(release, "command", wraps=release.command) as run:
            release.publish("owner/repo", self.first, "dev", Path("missing"), [existing])
        self.assertFalse(any(call.args[0] == "gh" for call in run.call_args_list))

    def test_new_and_draft_releases_upload_before_publish(self):
        self.check_publication([])
        tag = release.release_tag(self.first, "dev")
        release.git("tag", tag)
        self.check_publication([{"tag_name": tag, "draft": True, "prerelease": True}])

    def test_stable_release_is_not_prerelease_or_latest(self):
        assets = self.make_assets()
        original = release.command
        calls = []

        def run(*args):
            if args[0] == "gh":
                calls.append(args)
                return ""
            return original(*args)

        with patch.object(release, "command", side_effect=run):
            release.publish("owner/repo", self.first, "stable", assets, [])
        self.assertIn("--title", calls[0])
        self.assertIn("Lognara Android 2026.11.18.0000", calls[0])
        self.assertIn("--latest=false", calls[0])
        self.assertNotIn("--prerelease", calls[0])
        self.assertIn("--prerelease=false", calls[-1])
        self.assertIn("--latest=false", calls[-1])

    def check_publication(self, existing):
        assets = self.make_assets(f"assets-{len(existing)}")
        original = release.command
        calls = []

        def run(*args):
            if args[0] == "gh":
                calls.append(args)
                return ""
            return original(*args)

        with patch.object(release, "command", side_effect=run):
            release.publish("owner/repo", self.first, "dev", assets, existing)
        operations = [call[2] for call in calls]
        self.assertEqual(operations, ["upload", "edit"] if existing else ["create", "upload", "edit"])
        if not existing:
            self.assertIn(self.first, calls[0])
            self.assertIn("--draft", calls[0])
            self.assertIn("--latest=false", calls[0])
            self.assertIn("--prerelease", calls[0])
        self.assertIn("--draft=false", calls[-1])
        self.assertIn("--latest=false", calls[-1])
        self.assertIn("--prerelease=true", calls[-1])


if __name__ == "__main__":
    unittest.main()
