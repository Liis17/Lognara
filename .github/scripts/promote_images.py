"""Promote an existing dev manifest only when its sources match master."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = "docker.barkfluff.com"
SOURCES = {
    "core": ["Lognara-core", "Lognara-agent", "Lognara-relay", "Lognara-spool"],
    "relay": ["Lognara-relay", "Lognara-spool"],
    "dock": ["Lognara-dock"],
}
COMMON = [".dockerignore", ".cargo", "Cargo.toml", "Cargo.lock", "rust-toolchain*",
          "rustfmt.toml", ".rustfmt.toml", "clippy.toml", ".clippy.toml",
          ".github/workflows/publish-images.yml", ".github/scripts/promote_images.py"]


def run(*args, **kwargs):
    try:
        return subprocess.run(args, cwd=ROOT, check=True, text=True, capture_output=True, **kwargs).stdout.strip()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise


def validate_revision(component, revision, head="HEAD"):
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Missing or invalid image revision")
    run("git", "merge-base", "--is-ancestor", revision, head)
    run("git", "diff", "--exit-code", revision, head, "--", *SOURCES[component], *COMMON)


def inspect(image):
    # Buildx resolves the registry manifest and config without pulling image layers.
    data = json.loads(run("docker", "buildx", "imagetools", "inspect", image,
                          "--format", "{{json .Manifest}}"))
    digest = data["digest"]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("Invalid registry digest")
    return digest


def revision(image):
    config = json.loads(run("docker", "buildx", "imagetools", "inspect", image,
                            "--format", "{{json .Image}}"))
    # Buildx selects the platform config for this single-platform image.
    if "config" not in config:
        config = config["linux/amd64"]
    return config["config"].get("Labels", {}).get("org.opencontainers.image.revision", "")


def promote(component):
    image = f"{REGISTRY}/lognara-{component}"
    digest = inspect(f"{image}:dev")
    source = f"{image}@{digest}"
    validate_revision(component, revision(source))
    # Missing master is normal for the initial release; other registry failures propagate.
    result = subprocess.run(["docker", "buildx", "imagetools", "inspect", f"{image}:master",
                             "--format", "{{json .Manifest}}"], cwd=ROOT, text=True, capture_output=True)
    if result.returncode == 0:
        if json.loads(result.stdout)["digest"] == digest:
            print(f"{component}: already promoted {digest}")
            return
    elif not any(message in result.stderr.lower() for message in ("manifest unknown", "not found")):
        raise RuntimeError(result.stderr)
    with tempfile.TemporaryDirectory() as directory:
        output = Path(directory) / "version"
        run("bash", ".github/scripts/next-image-version.sh",
            env=dict(os.environ, COMPONENT=component, GITHUB_OUTPUT=str(output)))
        version = dict(line.split("=", 1) for line in output.read_text().splitlines())["version"]
    tags = [f"{image}:{version}", f"{image}:master", f"{image}:sha-{os.environ['GITHUB_SHA']}"]
    command = ["docker", "buildx", "imagetools", "create", "--prefer-index=false"]
    for tag in tags:
        command.extend(["--tag", tag])
    run(*command, source)
    for tag in tags:
        if inspect(tag) != digest:
            raise ValueError(f"Promoted digest mismatch: {tag}")
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
        summary.write(f"Promoted {image}:{version} from {source}\n")


if __name__ == "__main__":
    promote(os.environ["COMPONENT"])
