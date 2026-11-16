import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from check_ci import check


class CheckTests(unittest.TestCase):
    def needs(self, rust="success", web="success", dotnet="success", automation="success"):
        return {
            "guard": {"result": "success"},
            "changes": {"result": "success", "outputs": {
                "rust": '["Lognara-core"]', "web": "true", "dotnet": "true"}},
            **{name: {"result": result} for name, result in
               dict(rust=rust, web=web, dotnet=dotnet, automation=automation).items()},
        }

    def test_dev_pr_requires_every_selected_test(self):
        needs = self.needs()
        self.assertTrue(check(needs, "pull_request", "dev", "refs/pull/1/merge"))
        for job in ("guard", "changes", "automation", "rust", "web", "dotnet"):
            for result in ("skipped", "failure", "cancelled"):
                failed = copy.deepcopy(needs)
                failed[job]["result"] = result
                self.assertFalse(check(failed, "pull_request", "dev", "refs/pull/1/merge"))

    def test_promotion_pr_requires_guard_without_retesting(self):
        needs = self.needs("skipped", "skipped", "skipped", "skipped")
        self.assertTrue(check(needs, "pull_request", "master", "refs/pull/1/merge"))
        needs["guard"]["result"] = "failure"
        self.assertFalse(check(needs, "pull_request", "master", "refs/pull/1/merge"))

    def test_dev_push_skips_application_tests(self):
        self.assertTrue(check(self.needs("skipped", "skipped", "skipped"),
                              "push", "", "refs/heads/dev"))

    def test_master_push_requires_nuget_package_tests(self):
        self.assertTrue(check(self.needs("skipped", "skipped"), "push", "", "refs/heads/master"))
        self.assertFalse(check(self.needs("skipped", "skipped", "failure"),
                               "push", "", "refs/heads/master"))

    def test_manual_full_check_requires_all_tests(self):
        self.assertTrue(check(self.needs(), "workflow_dispatch", "", "refs/heads/dev", True))
        self.assertFalse(check(self.needs("skipped"), "workflow_dispatch", "", "refs/heads/dev", True))

    def test_docs_only_pr_requires_no_application_jobs(self):
        needs = self.needs("skipped", "skipped", "skipped")
        needs["changes"]["outputs"] = {"rust": "[]", "web": "false", "dotnet": "false"}
        self.assertTrue(check(needs, "pull_request", "dev", "refs/pull/1/merge"))
