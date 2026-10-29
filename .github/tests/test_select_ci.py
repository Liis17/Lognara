import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from select_ci import COMPONENTS, CRATES, select


class SelectTests(unittest.TestCase):
    def test_docs_skip_projects_and_images(self):
        self.assertEqual(select([]), {"rust": [], "web": False, "dotnet": False, "images": []})

    def test_core_change_selects_only_core(self):
        self.assertEqual(select(["Lognara-core"]), {
            "rust": ["Lognara-core"], "web": False, "dotnet": False, "images": ["core"],
        })

    def test_web_change_publishes_dock_without_rust_checks(self):
        self.assertEqual(select(["web"]), {
            "rust": [], "web": True, "dotnet": False, "images": ["dock"],
        })

    def test_spool_dependents_include_diagnostics_and_dock(self):
        result = select(CRATES)
        self.assertEqual(result["rust"], CRATES)
        self.assertEqual(result["images"], COMPONENTS)
        self.assertFalse(result["web"])

    def test_manual_run_checks_and_releases_everything(self):
        self.assertEqual(select([], full=True), {
            "rust": CRATES, "web": True, "dotnet": True, "images": COMPONENTS,
        })

    def test_shared_toolchain_requires_all_checks(self):
        self.assertEqual(select(["ci"]), select([], full=True))

    def test_image_workflow_change_rebuilds_images_only(self):
        self.assertEqual(select(["image-build"]), {
            "rust": [], "web": False, "dotnet": False, "images": COMPONENTS,
        })

    def test_dotnet_change_does_not_select_rust_web_or_images(self):
        self.assertEqual(select(["dotnet"]), {
            "rust": [], "web": False, "dotnet": True, "images": [],
        })


if __name__ == "__main__":
    unittest.main()
