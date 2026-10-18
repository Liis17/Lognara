"""Prepare agent release history and notes, then publish complete assets via gh."""

import datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys


def command(*args):
    return subprocess.check_output(args, text=True, encoding="utf-8").strip()


def git(*args):
    return command("git", *args)


def release_tag(sha):
    timestamp = int(git("show", "-s", "--format=%ct", sha))
    date = datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc)
    return f"agent-{date:%Y%m%d-%H%M%S}-{sha[:12]}"


def published_agents(releases):
    return [r for r in releases if not r["draft"] and not r["prerelease"]
            and re.fullmatch(r"agent-\d{8}-\d{6}-[0-9a-f]{12}", r["tag_name"])]


def release_commits(releases):
    # Resolve tags, rather than target_commitish (which may be a branch name).
    return {r["tag_name"]: git("rev-parse", f'{r["tag_name"]}^{{commit}}')
            for r in published_agents(releases)}


def previous_release(sha, commits):
    by_commit = {commit: tag for tag, commit in commits.items() if commit != sha}
    for ancestor in git("rev-list", "--topo-order", sha).splitlines():
        if ancestor in by_commit:
            return by_commit[ancestor]
    return None


def is_latest(sha, commits):
    # Publication is serialized by the workflow, including this decision.
    for commit in commits.values():
        if commit != sha and subprocess.run(
            ["git", "merge-base", "--is-ancestor", sha, commit], check=False
        ).returncode == 0:
            return False
    return True


def release_notes(repository, sha, tag, previous):
    revision = f"{previous}..{sha}" if previous else sha
    lines = ["## Изменения", ""]
    if previous is None:
        lines += ["Первый выпуск агента: изменения за всю историю агента и spool.", ""]
    log = git("log", "--full-history", "--format=%H%x09%s", revision, "--",
              "Lognara-agent/", "Lognara-spool/")
    for entry in log.splitlines():
        commit, subject = entry.split("\t", 1)
        lines.append(f"- {subject} ([{commit[:12]}](https://github.com/{repository}/commit/{commit}))")
    if not log:
        lines.append("- Изменений агента и spool нет.")
    if previous:
        lines += ["", f"[Сравнить с прошлым выпуском](https://github.com/{repository}/compare/{previous}...{tag})"]
    lines += ["", "## Файлы", "", f"Исходный коммит: `{sha}`.", "",
              "Статические Linux-бинарники musl; версия Cargo.toml не меняется автоматически.", ""]
    base = f"https://github.com/{repository}/releases/download/{tag}"
    for arch in ("amd64", "arm64"):
        name = f"lognara-agent-linux-{arch}.tar.gz"
        lines.append(f"- Linux {arch}: [{name}]({base}/{name})")
    lines += [f"- Контрольные суммы: [SHA256SUMS]({base}/SHA256SUMS)", ""]
    template = (Path(__file__).resolve().parents[1] / "docs/release-nginx.md").read_text(encoding="utf-8")
    lines.append(template.replace("@@TAG@@", tag).replace("@@REPOSITORY@@", repository))
    return "\n".join(lines)


def publish(repository, sha, assets, releases):
    tag = release_tag(sha)
    existing = next((r for r in releases if r["tag_name"] == tag), None)
    if existing and not existing["draft"]:
        if git("rev-parse", f"{tag}^{{commit}}") != sha:
            raise RuntimeError("Release tag points at another commit")
        print(f"Release {tag} already published; leaving its assets unchanged")
        return
    commits = release_commits(releases)
    notes = assets / "release-notes.md"
    notes.write_text(release_notes(repository, sha, tag, previous_release(sha, commits)), encoding="utf-8")
    files = [assets / f"lognara-agent-linux-{arch}.tar.gz" for arch in ("amd64", "arm64")]
    files.append(assets / "SHA256SUMS")
    for file in files:
        if not file.is_file():
            raise FileNotFoundError(file)
    common = ["--repo", repository]
    if not existing:
        command("gh", "release", "create", tag, *common, "--target", sha,
                "--draft", "--title", f"Lognara Agent {tag}", "--notes-file", str(notes))
    command("gh", "release", "upload", tag, *common, "--clobber", *(str(f) for f in files))
    command("gh", "release", "edit", tag, *common, "--notes-file", str(notes),
            "--draft=false", f"--latest={str(is_latest(sha, commits)).lower()}")


def main():
    repository = os.environ["GITHUB_REPOSITORY"]
    sha = git("rev-parse", os.environ["GITHUB_SHA"])
    pages = json.loads(command("gh", "api", "--paginate", "--slurp",
                               f"repos/{repository}/releases?per_page=100"))
    publish(repository, sha, Path(sys.argv[1]), [r for page in pages for r in page])


if __name__ == "__main__":
    main()
