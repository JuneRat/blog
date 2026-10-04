"""Publish the already-tested application image and record its registry identity.

Run only after verifying/loading the internal CI image artifact. Registry authentication is
provided by the caller; this script does not read or package Docker credentials.
"""
import argparse
import json
from pathlib import Path
import re
import subprocess

from github_release import release_version

PLATFORM = "linux/amd64"
IMAGE_ROLES = (("blog", "IMAGE", "BLOG_IMAGE"),)


def docker(*arguments):
    return subprocess.check_output(["docker", *arguments], text=True, stderr=subprocess.PIPE)


def inspect(reference):
    return json.loads(docker("image", "inspect", reference))[0]


def verified_images(bundle, repository, revision):
    """Check the verified image before permitting a registry write."""
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("revision must be a full Git commit SHA")
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*", repository.lower()):
        raise ValueError("repository must be a GitHub owner/repository suitable for a Docker image name")
    images = {}
    for role, filename, _ in IMAGE_ROLES:
        local_tag = (bundle / filename).read_text().strip()
        if local_tag != f"{role}:{revision}":
            raise ValueError(f"{filename} does not match the release commit")
        expected_id = (bundle / f"{filename}_ID").read_text().strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", expected_id):
            raise ValueError(f"{filename}_ID is not an immutable image ID")
        info = inspect(local_tag)
        labels = info.get("Config", {}).get("Labels") or {}
        if info["Id"] != expected_id:
            raise ValueError(f"{role} differs from the verified artifact")
        if labels.get("org.opencontainers.image.revision") != revision:
            raise ValueError(f"{role} revision label does not match the release commit")
        source = labels.get("org.opencontainers.image.source", "")
        if source.lower() != f"https://github.com/{repository}".lower():
            raise ValueError(f"{role} source label does not match the GitHub repository")
        if f"{info['Os']}/{info['Architecture']}" != PLATFORM:
            raise ValueError(f"{role} must be {PLATFORM}")
        destination = f"ghcr.io/{repository.lower()}"
        images[role] = {"id": expected_id, "tag": f"{destination}:sha-{revision}"}
    return images


def remote_digest(tag):
    try:
        manifest = json.loads(docker("manifest", "inspect", "--verbose", tag))
    except subprocess.CalledProcessError as error:
        # Authentication/network errors must never be mistaken for an unused tag.
        if re.search(r"no such manifest|manifest unknown", error.stderr or "", re.I):
            return None
        raise
    digest = manifest.get("Descriptor", {}).get("digest") if isinstance(manifest, dict) else None
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("version tag is not a supported single-platform image")
    return digest


def publish(verified_dir, output, repository, revision, tag="", project=None):
    if output.exists():
        raise ValueError("output directory already exists; refusing to overwrite a publication report")
    version = release_version(tag, project or Path(__file__).resolve().parents[1]) if tag else None
    images = verified_images(verified_dir, repository, revision)
    for role, info in images.items():
        docker("tag", info["id"], info["tag"])
        print(docker("push", info["tag"]), end="", flush=True)
        destination = info["tag"].split(":", 1)[0]
        digests = {value for value in inspect(info["tag"]).get("RepoDigests", [])
                   if re.fullmatch(re.escape(destination) + r"@sha256:[0-9a-f]{64}", value)}
        if len(digests) != 1:
            raise ValueError(f"expected one published registry digest for {role}")
        reference = digests.pop()
        # Confirm the reference a server will pull resolves to the tested image.
        print(docker("pull", reference), end="", flush=True)
        if inspect(reference)["Id"] != info["id"]:
            raise ValueError(f"published {role} does not match the tested image")
        info["reference"] = reference
        if version:
            version_tag = f"{destination}:{version}"
            digest = reference.split("@", 1)[1]
            existing = remote_digest(version_tag)
            if existing is not None and existing != digest:
                raise ValueError(f"{version_tag} already points to another image; use a new version")
            if existing is None:
                docker("tag", info["id"], version_tag)
                print(docker("push", version_tag), end="", flush=True)
            if remote_digest(version_tag) != digest:
                raise ValueError("version tag does not resolve to the verified registry digest")
            info["version_tag"] = version_tag
    # This CI-only report contains image identities, never deployment files.
    manifest = {"repository": repository, "revision": revision, "platform": PLATFORM, "images": images}
    if version:
        manifest.update(release_tag=tag, version=version)
    with output.open("x") as stream:
        stream.write(json.dumps(manifest, indent=2) + "\n")
    for info in images.values():
        print(info["reference"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verified-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--tag", default="", help="version Git tag; omit for SHA-only publication")
    parser.add_argument("--project", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        publish(args.verified_dir, args.output, args.repository, args.revision, args.tag, args.project)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Registry release failed: {error}\n")


if __name__ == "__main__":
    main()
