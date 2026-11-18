"""Prepare Android release metadata and publish complete assets via gh."""

import argparse
import datetime
import hashlib
import json
import re
import subprocess
from pathlib import Path


VERSION_EPOCH = 1735689600
APK_NAME = "lognara-android.apk"
TAG_PATTERNS = {
    "stable": re.compile(r"android-\d{8}-\d{6}-[0-9a-f]{12}"),
    "dev": re.compile(r"android-dev-\d{8}-\d{6}-[0-9a-f]{12}"),
}


def command(*args):
    return subprocess.check_output(args, text=True, encoding="utf-8").strip()


def git(*args):
    return command("git", *args)


def validate_channel(channel):
    if channel not in TAG_PATTERNS:
        raise ValueError(f"Unsupported Android release channel: {channel}")


def release_tag(sha, channel):
    validate_channel(channel)
    commit = git("rev-parse", sha)
    timestamp = int(git("show", "-s", "--format=%ct", commit))
    date = datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc)
    prefix = "android" if channel == "stable" else "android-dev"
    return f"{prefix}-{date:%Y%m%d-%H%M%S}-{commit[:12]}"


def release_version(sha, channel):
    validate_channel(channel)
    commit = git("rev-parse", sha)
    timestamp = int(git("show", "-s", "--format=%ct", commit))
    if timestamp < VERSION_EPOCH:
        raise ValueError("Android release commit predates the versionCode epoch")
    date = datetime.datetime.fromtimestamp(timestamp, datetime.timezone.utc)
    version_code = (timestamp - VERSION_EPOCH) // 60
    if version_code > 2_147_483_647:
        raise ValueError("Android release versionCode exceeds the Int range")
    version_name = f"{date:%Y.%m.%d.%H%M}"
    if channel == "dev":
        version_name += "-dev"
    return {
        "version_code": version_code,
        "version_name": version_name,
        "channel": channel,
        "commit": commit,
        "tag": release_tag(commit, channel),
    }


def published_android_releases(releases, channel):
    validate_channel(channel)
    is_prerelease = channel == "dev"
    return [
        release for release in releases
        if not release.get("draft", False)
        and bool(release.get("prerelease", False)) == is_prerelease
        and TAG_PATTERNS[channel].fullmatch(release.get("tag_name", ""))
    ]


def release_commits(releases):
    return {
        release["tag_name"]: git("rev-parse", f'{release["tag_name"]}^{{commit}}')
        for release in releases
    }


def previous_release(sha, commits):
    by_commit = {commit: tag for tag, commit in commits.items() if commit != sha}
    for ancestor in git("rev-list", "--topo-order", sha).splitlines():
        if ancestor in by_commit:
            tag = by_commit[ancestor]
            return tag, commits[tag]
    return None


def release_notes(repository, version, previous):
    sha = version["commit"]
    tag = version["tag"]
    previous_commit = previous[1] if previous else None
    revision = f"{previous_commit}..{sha}" if previous_commit else sha
    lines = ["## Изменения", ""]
    if previous is None:
        lines += [
            "Первый выпуск Android-клиента: изменения за всю историю пути "
            "lognara-android-client/.",
            "",
        ]
    log = git(
        "log", "--full-history", "--format=%H%x09%s", revision, "--",
        "lognara-android-client/",
    )
    for entry in log.splitlines():
        commit, subject = entry.split("\t", 1)
        lines.append(
            f"- {subject} ([{commit[:12]}](https://github.com/{repository}/commit/{commit}))"
        )
    if not log:
        lines.append("- Изменений Android-клиента нет.")
    if previous:
        lines += [
            "",
            f"[Сравнить с прошлым выпуском](https://github.com/{repository}/compare/{previous[0]}...{tag})",
        ]
    lines += [
        "",
        "## Версия и установка",
        "",
        f"Исходный коммит: {sha}.",
        f"Канал обновлений: {version['channel']}.",
        "",
        "Для установки скачайте APK и откройте его на Android. Для обновления установите APK "
        "того же канала поверх текущей версии.",
        "",
        "## Файлы",
        "",
    ]
    base = f"https://github.com/{repository}/releases/download/{tag}"
    lines += [
        f"- APK: [{APK_NAME}]({base}/{APK_NAME})",
        f"- Версия и SHA-256 APK: [version.json]({base}/version.json)",
        f"- Контрольные суммы: [SHA256SUMS]({base}/SHA256SUMS)",
        "",
    ]
    return "\n".join(lines)


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def prepare_assets(assets, version):
    assets.mkdir(parents=True, exist_ok=True)
    apk = assets / APK_NAME
    if not apk.is_file():
        raise FileNotFoundError(apk)
    metadata = {
        "versionCode": version["version_code"],
        "versionName": version["version_name"],
        "channel": version["channel"],
        "commit": version["commit"],
        "apk": APK_NAME,
        "sha256": sha256_file(apk),
    }
    version_file = assets / "version.json"
    version_file.write_text(
        json.dumps(metadata, ensure_ascii=False, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    checksums = [
        f"{metadata['sha256']}  {APK_NAME}",
        f"{sha256_file(version_file)}  version.json",
    ]
    (assets / "SHA256SUMS").write_text("\n".join(checksums) + "\n", encoding="utf-8")
    return metadata


def release_title(version):
    prefix = "Lognara Android" if version["channel"] == "stable" else "Lognara Android dev"
    return f"{prefix} {version['version_name']}"


def publish(repository, sha, channel, assets, releases, dry_run=False):
    version = release_version(sha, channel)
    tag = version["tag"]
    existing = next((release for release in releases if release["tag_name"] == tag), None)
    if existing:
        if existing.get("draft", False):
            if existing.get("target_commitish") != version["commit"]:
                raise RuntimeError(f"Android draft release {tag} targets another commit")
        else:
            tagged_commit = git("rev-parse", f"{tag}^{{commit}}")
            if tagged_commit != version["commit"]:
                raise RuntimeError(f"Android release tag {tag} points at another commit")
            print(f"Release {tag} already published; leaving its assets unchanged")
            return

    channel_releases = published_android_releases(releases, channel)
    commits = release_commits(channel_releases)
    previous = previous_release(version["commit"], commits)
    metadata = prepare_assets(assets, version)
    notes = assets / "release-notes.md"
    notes.write_text(release_notes(repository, version, previous), encoding="utf-8")
    files = [assets / APK_NAME, assets / "version.json", assets / "SHA256SUMS"]
    title = release_title(version)

    if dry_run:
        print(
            f"Prepared {tag}: {title}; channel={channel}; "
            f"versionCode={metadata['versionCode']}; no GitHub calls made"
        )
        return

    common = ["--repo", repository]
    if not existing:
        create = [
            "gh", "release", "create", tag, *common, "--target", version["commit"],
            "--draft", "--latest=false", "--title", title, "--notes-file", str(notes),
        ]
        if channel == "dev":
            create.append("--prerelease")
        command(*create)
    command(
        "gh", "release", "upload", tag, *common, "--clobber",
        *(str(path) for path in files),
    )
    command(
        "gh", "release", "edit", tag, *common, "--notes-file", str(notes),
        "--draft=false", "--latest=false", f"--prerelease={str(channel == 'dev').lower()}",
    )


def fetch_releases(repository):
    pages = json.loads(command(
        "gh", "api", "--paginate", "--slurp",
        f"repos/{repository}/releases?per_page=100",
    ))
    return [release for page in pages for release in page]


def write_github_output(path, version):
    values = {
        "version_code": version["version_code"],
        "version_name": version["version_name"],
        "channel": version["channel"],
        "tag": version["tag"],
        "commit": version["commit"],
    }
    with Path(path).open("a", encoding="utf-8") as output:
        for name, value in values.items():
            print(f"{name}={value}", file=output)


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="operation", required=True)
    version_parser = commands.add_parser("version")
    version_parser.add_argument("--sha", required=True)
    version_parser.add_argument("--channel", choices=TAG_PATTERNS, required=True)
    version_parser.add_argument("--github-output", required=True)
    publish_parser = commands.add_parser("publish")
    publish_parser.add_argument("assets", type=Path)
    publish_parser.add_argument("--repository", required=True)
    publish_parser.add_argument("--sha", required=True)
    publish_parser.add_argument("--channel", choices=TAG_PATTERNS, required=True)
    publish_parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if args.operation == "version":
        write_github_output(args.github_output, release_version(args.sha, args.channel))
        return
    releases = [] if args.dry_run else fetch_releases(args.repository)
    publish(
        args.repository, args.sha, args.channel, args.assets, releases, args.dry_run,
    )


if __name__ == "__main__":
    main()
