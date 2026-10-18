import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from select_ci import COMPONENTS, CRATES, select


class SelectTests(unittest.TestCase):
    def test_docs_skip_projects_and_images(self):
        self.assertEqual(select([]), {"rust": [], "web": False, "images": []})

    def test_core_change_selects_only_core(self):
        self.assertEqual(select(["Lognara-core"]), {
            "rust": ["Lognara-core"], "web": False, "images": ["core"],
        })

    def test_web_change_publishes_dock_without_rust_checks(self):
        self.assertEqual(select(["web"]), {
            "rust": [], "web": True, "images": ["dock"],
        })

    def test_spool_dependents_do_not_select_dock(self):
        result = select(["Lognara-spool", "Lognara-agent", "Lognara-relay", "Lognara-core"])
        self.assertEqual(result["rust"], CRATES[:-1])
        self.assertEqual(result["images"], ["core", "relay"])
        self.assertFalse(result["web"])

    def test_manual_run_checks_and_releases_everything(self):
        self.assertEqual(select([], full=True), {
            "rust": CRATES, "web": True, "images": COMPONENTS,
        })

    def test_shared_toolchain_requires_all_checks(self):
        self.assertEqual(select(["ci"]), select([], full=True))

    def test_image_workflow_change_rebuilds_images_only(self):
        self.assertEqual(select(["image-build"]), {
            "rust": [], "web": False, "images": COMPONENTS,
        })


if __name__ == "__main__":
    unittest.main()
