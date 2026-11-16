from pathlib import Path
import subprocess
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
import promote_images as promotion


class RevisionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.patch = patch.object(promotion, "ROOT", self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.git("init", "-q")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.write("Lognara-core/src/main.rs", "first")
        self.initial = self.commit()

    def git(self, *args):
        return promotion.run("git", *args)

    def write(self, path, content):
        file = self.root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "test")
        return self.git("rev-parse", "HEAD")

    def test_docs_changes_allow_same_component_image(self):
        self.write("README.md", "docs")
        self.commit()
        promotion.validate_revision("core", self.initial)

    def test_changed_source_dependency_or_build_input_rejects_image(self):
        for file in ("Lognara-core/src/main.rs", "Lognara-spool/src/lib.rs",
                     ".dockerignore", ".github/workflows/publish-images.yml"):
            with self.subTest(file=file):
                before = self.git("rev-parse", "HEAD")
                self.write(file, "changed")
                self.commit()
                with self.assertRaises(subprocess.CalledProcessError):
                    promotion.validate_revision("core", before)

    def test_unrelated_component_does_not_block_promotion(self):
        self.write("Lognara-dock/src/main.rs", "dock")
        self.commit()
        promotion.validate_revision("relay", self.initial)

    def test_newer_dev_image_cannot_be_promoted_to_older_master(self):
        self.write("README.md", "future")
        future = self.commit()
        with self.assertRaises(subprocess.CalledProcessError):
            promotion.validate_revision("core", future, self.initial)

    def test_missing_or_untrusted_revision_is_rejected(self):
        for revision in ("", "HEAD", "--help", "a" * 40):
            with self.subTest(revision=revision), self.assertRaises((ValueError, subprocess.CalledProcessError)):
                promotion.validate_revision("core", revision)


class RegistryTests(unittest.TestCase):
    def test_unchanged_digest_does_not_allocate_version_or_copy(self):
        import json
        digest = "sha256:" + "a" * 64
        with patch.object(promotion, "inspect", return_value=digest), \
             patch.object(promotion, "revision", return_value="b" * 40), \
             patch.object(promotion, "validate_revision"), \
             patch.object(promotion.subprocess, "run", return_value=SimpleNamespace(
                 returncode=0, stdout=json.dumps({"digest": digest}))), \
             patch.object(promotion, "run") as run:
            promotion.promote("core")
            run.assert_not_called()

    def test_promotion_copies_pinned_digest_and_verifies_every_tag(self):
        digest = "sha256:" + "a" * 64
        with tempfile.TemporaryDirectory() as directory:
            def command(*args, **kwargs):
                if args[0] == "bash":
                    Path(kwargs["env"]["GITHUB_OUTPUT"]).write_text("version=1.2.3\n")
                return ""

            with patch.dict(promotion.os.environ, {
                "GITHUB_SHA": "c" * 40, "GITHUB_STEP_SUMMARY": str(Path(directory) / "summary")}), \
                 patch.object(promotion, "inspect", return_value=digest) as inspect, \
                 patch.object(promotion, "revision", return_value="b" * 40), \
                 patch.object(promotion, "validate_revision"), \
                 patch.object(promotion.subprocess, "run", return_value=SimpleNamespace(
                     returncode=1, stderr="manifest unknown")), \
                 patch.object(promotion, "run", side_effect=command) as run:
                promotion.promote("core")
                copy = run.call_args_list[-1].args
                self.assertEqual(copy[-1], f"{promotion.REGISTRY}/lognara-core@{digest}")
                self.assertIn("--prefer-index=false", copy)
                self.assertEqual(inspect.call_count, 4)
                self.assertIn("lognara-core:1.2.3", Path(directory, "summary").read_text())

    def test_inspect_validates_registry_digest(self):
        with patch.object(promotion, "run", return_value='{"digest":"invalid"}'):
            with self.assertRaises(ValueError):
                promotion.inspect("example")

    def test_revision_reads_single_image_and_platform_map(self):
        import json
        config = {"config": {"Labels": {"org.opencontainers.image.revision": "a" * 40}}}
        for value in (config, {"linux/amd64": config}):
            with patch.object(promotion, "run", return_value=json.dumps(value)):
                self.assertEqual(promotion.revision("example"), "a" * 40)

    def test_failed_revision_check_never_writes_registry(self):
        with patch.object(promotion, "inspect", return_value="sha256:" + "a" * 64), \
             patch.object(promotion, "revision", return_value=""), \
             patch.object(promotion, "run") as run:
            with self.assertRaises(ValueError):
                promotion.promote("core")
            run.assert_not_called()
