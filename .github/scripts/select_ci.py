"""Select checks and container images from paths-filter results."""

import json
import os


CRATES = ["Lognara-agent", "Lognara-relay", "Lognara-spool", "Lognara-core", "Lognara-selflog", "Lognara-dock"]
COMPONENTS = ["core", "relay", "dock"]


def select(changed, full=False):
    full_checks = full or "ci" in changed
    rust = CRATES if full_checks else [crate for crate in CRATES if crate in changed]
    web = full_checks or "web" in changed
    dotnet = full_checks or "dotnet" in changed
    images = [
        component for component in COMPONENTS
        if full_checks or "image-build" in changed or f"Lognara-{component}" in changed
        or (component == "dock" and "web" in changed)
    ]
    return {"rust": rust, "web": web, "dotnet": dotnet, "images": images}


if __name__ == "__main__":
    selected = select(
        json.loads(os.environ.get("CHANGED_FILTERS") or "[]"),
        full=os.environ.get("FULL_CHECKS") == "true",
    )
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        for name, value in selected.items():
            print(f"{name}={json.dumps(value)}", file=output)
