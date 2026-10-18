"""Release history and retry scenarios, using isolated git repositories."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "scripts/release_agent.py"
spec = importlib.util.spec_from_file_location("release_agent", SCRIPT)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.previous_directory = Path.cwd()
        os.chdir(self.directory.name)
        release.git("init", "-b", "master")
        release.git("config", "user.name", "Release Test")
        release.git("config", "user.email", "test@example.invalid")
        self.first = self.commit("Lognara-agent/main.rs", "Первый агент")
        self.tag = release.release_tag(self.first)
        release.git("tag", self.tag)

    def tearDown(self):
        os.chdir(self.previous_directory)
        self.directory.cleanup()

    def commit(self, path, message):
        file = Path(path)
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(message, encoding="utf-8")
        release.git("add", path)
        release.git("commit", "-m", message)
        return release.git("rev-parse", "HEAD")

    def test_first_release_includes_history_and_rendered_nginx(self):
        notes = release.release_notes("owner/repo", self.first, self.tag, None)
        self.assertIn("Первый агент", notes)
        self.assertIn("Первый выпуск", notes)
        self.assertIn(f"ARG LOGNARA_VERSION={self.tag}", notes)
        self.assertIn("github.com/owner/repo/releases/download/", notes)
        self.assertNotIn("@@", notes)
        self.assertLess(notes.index("## Изменения"), notes.index("## Файлы"))
        self.assertLess(notes.index("## Файлы"), notes.index("### Запуск nginx"))

    def test_changelog_includes_spool_and_excludes_other_components(self):
        self.commit("Lognara-spool/lib.rs", "Изменение spool")
        self.commit("Lognara-core/main.rs", "Только core")
        sha = self.commit("Lognara-agent/main.rs", "Новый агент")
        notes = release.release_notes("owner/repo", sha, release.release_tag(sha), self.tag)
        self.assertIn("Изменение spool", notes)
        self.assertIn("Новый агент", notes)
        self.assertNotIn("Только core", notes)
        self.assertNotIn("Первый агент", notes)
        self.assertIn(f"/compare/{self.tag}...", notes)

    def test_previous_release_ignores_unrelated_and_descendant_commits(self):
        sha = self.commit("Lognara-agent/main.rs", "Следующий агент")
        descendant = self.commit("Lognara-agent/main.rs", "Поздний агент")
        release.git("checkout", "--orphan", "unrelated")
        release.git("rm", "-rf", ".")
        unrelated = self.commit("other.txt", "Другая история")
        commits = {self.tag: self.first, "later": descendant, "other": unrelated}
        self.assertEqual(release.previous_release(sha, commits), self.tag)
        self.assertFalse(release.is_latest(sha, commits))
        self.assertTrue(release.is_latest(descendant, commits))

    def test_only_published_agent_releases_are_selected(self):
        base = {"tag_name": self.tag, "draft": False, "prerelease": False}
        releases = [base, dict(base, draft=True), dict(base, prerelease=True),
                    dict(base, tag_name="v0.1.0")]
        self.assertEqual(release.published_agents(releases), [base])

    def test_published_retry_does_not_upload_or_edit(self):
        existing = {"tag_name": self.tag, "draft": False}
        with patch.object(release, "command", wraps=release.command) as run:
            release.publish("owner/repo", self.first, Path("assets"), [existing])
        self.assertFalse(any(call.args[0] == "gh" for call in run.call_args_list))

    def test_new_release_uploads_before_publishing_and_targets_sha(self):
        self.check_publication([])

    def test_draft_retry_uploads_without_creating_another_release(self):
        self.check_publication([{"tag_name": self.tag, "draft": True, "prerelease": False}])

    def check_publication(self, existing):
        assets = Path("assets")
        assets.mkdir()
        for name in ("lognara-agent-linux-amd64.tar.gz", "lognara-agent-linux-arm64.tar.gz", "SHA256SUMS"):
            (assets / name).write_text("fixture")
        original = release.command
        calls = []

        def run(*args):
            if args[0] == "gh":
                calls.append(args)
                return ""
            return original(*args)

        with patch.object(release, "command", side_effect=run):
            release.publish("owner/repo", self.first, assets, existing)
        self.assertEqual([call[2] for call in calls],
                         ["upload", "edit"] if existing else ["create", "upload", "edit"])
        if not existing:
            self.assertIn(self.first, calls[0])
            self.assertIn("--draft", calls[0])
        self.assertIn("--draft=false", calls[-1])


if __name__ == "__main__":
    unittest.main()
