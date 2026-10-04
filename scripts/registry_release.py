"""Publish the already-tested application image and package a digest-pinned deployment.

Run only after verifying/loading the offline artifact. Registry authentication is
provided by the caller; this script does not read or package Docker credentials.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
PLATFORM = "linux/amd64"
DELIVERY_FILES = (
    "compose.yaml", ".env.example", "ops/postgres-init.sh",
    "ops/blog-backup.service", "ops/blog-backup.timer",
    "ops/blog-maintenance.service", "ops/blog-maintenance.timer",
    "scripts/database-roles.sql", "scripts/compose-init.sh", "scripts/compose-backup.sh",
)
IMAGE_ROLES = (("blog", "IMAGE", "BLOG_IMAGE"),)


def docker(*arguments):
    return subprocess.check_output(["docker", *arguments], text=True)


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


def package(source, output, repository, revision, images):
    """Copy only deployment inputs; never copy a workstation's .env or data."""
    output.mkdir(parents=True, exist_ok=False)
    for filename in DELIVERY_FILES:
        target = output / filename
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / filename, target)
    shutil.copytree(source / "docs", output / "docs")
    example = (output / ".env.example").read_text()
    for role, filename, variable in IMAGE_ROLES:
        reference = images[role]["reference"]
        placeholder = f"# {variable}={role}:local"
        if example.count(placeholder) != 1:
            raise ValueError(f"expected one {variable} placeholder in .env.example")
        example = example.replace(placeholder, f"{variable}={reference}")
        (output / filename).write_text(reference + "\n")
        (output / f"{filename}_ID").write_text(images[role]["id"] + "\n")
    (output / ".env.example").write_text(example)
    (output / "REVISION").write_text(revision + "\n")
    (output / "PLATFORM").write_text(PLATFORM + "\n")
    # Use the source Compose file, not the offline file which pins a local DB ID.
    database = re.findall(r"^    image: (postgres:[^\s]+@sha256:[0-9a-f]{64})$",
                          (output / "compose.yaml").read_text(), re.M)
    if len(database) != 1:
        raise ValueError("expected one registry-pinned PostgreSQL image")
    (output / "DATABASE_IMAGE").write_text(database[0] + "\n")
    manifest = {"repository": repository, "revision": revision, "platform": PLATFORM, "images": images}
    (output / "REGISTRY_IMAGES.json").write_text(json.dumps(manifest, indent=2) + "\n")
    checksums = []
    for file in sorted(output.rglob("*")):
        if file.is_file():
            checksums.append(f"{hashlib.sha256(file.read_bytes()).hexdigest()}  ./{file.relative_to(output).as_posix()}\n")
    (output / "SHA256SUMS").write_text("".join(checksums))


def publish(bundle, output, repository, revision, source=ROOT):
    if output.exists():
        raise ValueError("output directory already exists; refusing to overwrite a deployment")
    images = verified_images(bundle, repository, revision)
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
    # There is no deployment artifact until the published image is verified.
    package(source, output, repository, revision, images)
    for info in images.values():
        print(info["reference"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--revision", required=True)
    args = parser.parse_args()
    try:
        publish(args.bundle, args.output, args.repository, args.revision)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Registry release failed: {error}\n")


if __name__ == "__main__":
    main()
