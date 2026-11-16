"""Require exactly the checks selected for this event; fail closed on selection errors."""
import json
import os


def check(needs, event, base, ref, full=False):
    if event == "pull_request" and needs["guard"]["result"] != "success":
        return False
    if needs["changes"]["result"] != "success":
        return False
    selected = needs["changes"]["outputs"]
    tests = (event == "pull_request" and base == "dev") or full
    expected = {
        "automation": event != "pull_request" or base == "dev",
        "rust": tests and json.loads(selected["rust"]) != [],
        "web": tests and selected["web"] == "true",
        "dotnet": (tests or ref == "refs/heads/master") and selected["dotnet"] == "true",
    }
    return all(needs[job]["result"] == ("success" if required else "skipped")
               for job, required in expected.items())


if __name__ == "__main__":
    if not check(json.loads(os.environ["NEEDS"]), os.environ["EVENT"],
                 os.environ.get("BASE", ""), os.environ["REF"], os.environ["FULL"] == "true"):
        raise SystemExit("Required CI checks did not pass")
