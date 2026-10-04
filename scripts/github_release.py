"""Validate release versions, prepare standalone Compose files and publish a Release.

Only public source templates and the verified image report become attachments.
Existing releases/assets are verified, never overwritten on a workflow retry.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
COMPOSE_FILES = ("compose.yaml", "compose.postgres.yaml")
DATA_FILES = (*COMPOSE_FILES, "published-image.json")
ASSETS = (*DATA_FILES, "SHA256SUMS")
VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"


def version_notes(project, version):
    text = (project / "CHANGELOG.md").read_text()
    match = re.search(r"(?ms)^## " + re.escape(version) + r"\s*\n(.*?)(?=^## |\Z)", text)
    if not match or not match[1].strip():
        raise ValueError(f"CHANGELOG.md must contain a nonempty '## {version}' section")
    return match[1].strip()


def release_version(tag, project=None):
    if not re.fullmatch("v" + VERSION, tag):
        raise ValueError("release tag must be vMAJOR.MINOR.PATCH, optionally with a prerelease suffix")
    version = tag[1:]
    if "-" in version:
        for part in version.split("-", 1)[1].split("."):
            if part.isdigit() and len(part) > 1 and part.startswith("0"):
                raise ValueError("numeric prerelease identifiers must not have leading zeroes")
    if len(version) > 128:
        raise ValueError("release version is too long for a Docker tag")
    if project is not None:
        # Require an explicit package version; fail closed on workspace inheritance.
        source = (project / "crates/server/Cargo.toml").read_text()
        package = re.search(r"(?ms)^\[package\][ \t]*\n(.*?)(?=^\[|\Z)", source)
        versions = re.findall(r'^version\s*=\s*"([^"\n]+)"[ \t]*(?:#.*)?$',
                              package[1] if package else "", re.M)
        frontend = json.loads((project / "apps/admin/package.json").read_text())
        if versions != [version] or frontend.get("version") != version:
            raise ValueError("release tag must match server and admin package versions")
        version_notes(project, version)
    return version


def read_report(path, tag, repository=None, revision=None):
    version = release_version(tag)
    report = json.loads(path.read_text())
    owner_repo = report["repository"]
    commit = report["revision"]
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]*/[a-zA-Z0-9][a-zA-Z0-9._-]*", owner_repo):
        raise ValueError("invalid release repository")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid release revision")
    if repository is not None and owner_repo.lower() != repository.lower():
        raise ValueError("release report belongs to another repository")
    if revision is not None and commit != revision:
        raise ValueError("release report belongs to another commit")
    if report.get("release_tag") != tag or report.get("version") != version:
        raise ValueError("release report does not match the version tag")
    if report["platform"] != "linux/amd64" or set(report["images"]) != {"blog"}:
        raise ValueError("release must contain one Linux amd64 blog image")
    info = report["images"]["blog"]
    destination = f"ghcr.io/{owner_repo.lower()}"
    if (not re.fullmatch(r"sha256:[0-9a-f]{64}", info.get("id", ""))
            or info.get("tag") != f"{destination}:sha-{commit}"
            or info.get("version_tag") != f"{destination}:{version}"
            or not re.fullmatch(re.escape(destination) + r"@sha256:[0-9a-f]{64}", info["reference"])):
        raise ValueError("release image references do not match the publication")
    return report


def compose_image(source, reference):
    lines = source.splitlines(keepends=True)
    in_blog = False
    replaced = 0
    for index, line in enumerate(lines):
        if re.match(r"^  blog:\s*$", line):
            in_blog = True
        elif line.strip() and not line.lstrip().startswith("#") and re.match(r"^\S|^  \S", line):
            in_blog = False
        if in_blog and re.match(r"^    image:\s*\S", line):
            lines[index] = f"    image: {reference}\n"
            replaced += 1
    if replaced != 1:
        raise ValueError("Compose template must contain exactly one blog.image")
    return "".join(lines)


def prepare(project, manifest, output, tag, repository, revision):
    version = release_version(tag, project)
    report = read_report(manifest, tag, repository, revision)
    info = report["images"]["blog"]
    reference = info["version_tag"] + "@" + info["reference"].split("@", 1)[1]
    files = {name: compose_image((project / name).read_text(), reference) for name in COMPOSE_FILES}
    files["published-image.json"] = json.dumps(report, indent=2) + "\n"
    source = f"https://github.com/{report['repository']}/blob/{report['revision']}"
    notes = f"""{version_notes(project, version)}

### 安装

平台：Linux amd64 / x86_64。

- 已有 PostgreSQL：下载附件 `compose.yaml`。
- 同时启动 PostgreSQL：下载附件 `compose.postgres.yaml`。

在 1Panel 编排中粘贴其中一份文件，填写域名、安装码及所需数据库密码，然后启动并访问 `/install`。数据库连接由安装向导验证，配置保存在持久卷中。

两份 Compose 已固定本次验收镜像：

```yaml
image: {reference}
```

版本标签：`{info['version_tag']}`；提交标签：`{info['tag']}`。两者指向同一镜像。附件 `published-image.json` 和 `SHA256SUMS` 提供镜像身份与文件校验信息。

[1Panel 部署指南]({source}/docs/1panel.md) · [备份与恢复]({source}/docs/compose-backup.md)

升级前先在后台备份。镜像回退不会撤销数据库迁移；恢复时需要匹配的备份格式与数据库结构。

源代码提交：`{report['revision']}`。
"""
    files["SHA256SUMS"] = "".join(
        f"{hashlib.sha256(files[name].encode()).hexdigest()}  {name}\n" for name in DATA_FILES)
    files["RELEASE_NOTES.md"] = notes
    output.mkdir()  # Never overwrite an existing directory or user configuration.
    for name, text in files.items():
        (output / name).write_text(text)
    return reference


def gh(*arguments):
    return subprocess.check_output(["gh", *arguments], text=True, stderr=subprocess.PIPE)


def find_release(repository, tag):
    # The by-tag endpoint is documented for published releases. List with the
    # authenticated writer token so an interrupted draft can also be resumed.
    pages = json.loads(gh("api", f"repos/{repository}/releases?per_page=100", "--paginate", "--slurp"))
    matches = [item for page in pages for item in page if item["tag_name"] == tag]
    if len(matches) > 1:
        raise ValueError("multiple Releases use this tag; refusing an ambiguous publication")
    return matches[0] if matches else None


def verify_remote_assets(directory, repository, tag, names):
    if not names:
        return
    with tempfile.TemporaryDirectory(prefix="blog-release-assets-") as temporary:
        arguments = ["release", "download", tag, "--repo", repository, "--dir", temporary]
        for name in names:
            arguments.extend(("--pattern", name))
        gh(*arguments)
        for name in names:
            if (Path(temporary) / name).read_bytes() != (directory / name).read_bytes():
                raise ValueError(f"existing Release asset differs: {name}; refusing to overwrite")


def publish(directory, repository, revision, tag):
    read_report(directory / "published-image.json", tag, repository, revision)
    expected_sums = "".join(
        f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n" for name in DATA_FILES)
    if (directory / "SHA256SUMS").read_text() != expected_sums:
        raise ValueError("Release attachment checksum mismatch")
    actual_revision = gh("api", f"repos/{repository}/commits/{tag}", "--jq", ".sha").strip()
    if actual_revision != revision:
        raise ValueError("remote version tag does not point to the verified commit")
    notes = (directory / "RELEASE_NOTES.md").read_text()
    existing = find_release(repository, tag)
    if existing is None:
        arguments = ["release", "create", tag, "--repo", repository, "--verify-tag", "--draft",
                     "--target", revision, "--title", tag, "--generate-notes",
                     "--notes-file", str(directory / "RELEASE_NOTES.md")]
        if "-" in tag:
            arguments.append("--prerelease")
        gh(*arguments)
        existing = find_release(repository, tag)
        if existing is None:
            raise ValueError("created Release could not be read back")
    if (existing["tag_name"] != tag or existing["target_commitish"] != revision
            or existing["prerelease"] != ("-" in tag)
            or not (existing.get("body") or "").startswith(notes.strip())):
        raise ValueError("existing Release does not match this publication; refusing to edit it")
    present = {asset["name"] for asset in existing["assets"]}
    verify_remote_assets(directory, repository, tag, [name for name in ASSETS if name in present])
    missing = [name for name in ASSETS if name not in present]
    if not existing["draft"]:
        if missing:
            raise ValueError("published Release is missing expected assets; refusing to edit it")
        return existing["html_url"]
    if missing:
        gh("release", "upload", tag, "--repo", repository, *(str(directory / name) for name in missing))
    verify_remote_assets(directory, repository, tag, ASSETS)
    gh("release", "edit", tag, "--repo", repository, "--verify-tag", "--draft=false")
    result = find_release(repository, tag)
    if not result or result["draft"]:
        raise ValueError("Release publication could not be confirmed")
    return result["html_url"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    validate = commands.add_parser("validate")
    validate.add_argument("--project", type=Path, default=ROOT)
    validate.add_argument("--tag", required=True)
    assets = commands.add_parser("prepare")
    assets.add_argument("--project", type=Path, default=ROOT)
    assets.add_argument("--manifest", type=Path, required=True)
    assets.add_argument("--output", type=Path, required=True)
    release = commands.add_parser("publish")
    release.add_argument("--directory", type=Path, required=True)
    for command in (assets, release):
        command.add_argument("--tag", required=True)
        command.add_argument("--repository", required=True)
        command.add_argument("--revision", required=True)
    args = parser.parse_args()
    try:
        if args.command == "validate":
            print(release_version(args.tag, args.project))
        elif args.command == "prepare":
            print(prepare(args.project, args.manifest, args.output, args.tag, args.repository, args.revision))
        else:
            print(publish(args.directory, args.repository, args.revision, args.tag))
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"GitHub release failed: {error}\n")


if __name__ == "__main__":
    main()
