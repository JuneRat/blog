#!/usr/bin/env python3
"""Upgrade an owned Compose fixture from an older release to a candidate image.

Both images must already exist locally. No deployment .env, existing database
or external SMTP service is used. Only this run's random project is removed.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tempfile
import time
import uuid

from acceptance_support import API, PNG, AcceptanceError, Client, require
from test_compose import wait_for

PROJECT = Path(__file__).resolve().parents[1]


def image_identity(name):
    result = subprocess.run(["docker", "image", "inspect", name], capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, "upgrade image is unavailable")
    info = json.loads(result.stdout)[0]
    return {"name": name, "id": info["Id"], "architecture": info["Architecture"],
            "revision": (info["Config"].get("Labels") or {}).get("org.opencontainers.image.revision")}


def exercise(root, old, new, report):
    project = "blog-upgrade-test-" + uuid.uuid4().hex[:12]
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("BLOG_", "COMPOSE_", "PG"))
           and key not in ("DATABASE_URL", "IDP_SECRET", "GH_SECRET", "RUST_LOG")}
    for filename in ("compose.yaml", ".env.example", "scripts/compose-init.sh", "ops/postgres-init.sh"):
        target = root / filename
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(PROJECT / filename, target)
    initialized = subprocess.run(["sh", str(root / "scripts/compose-init.sh")], cwd=root, env=env,
                                 capture_output=True, timeout=10)
    require(initialized.returncode == 0, "upgrade environment initialization failed")
    env_file = root / ".env"
    owner = re.search(r"^BLOG_OWNER_PASSWORD=([a-f0-9]{64})$", env_file.read_text(), re.M).group(1)
    with env_file.open("a") as stream:
        stream.write(f"\nBLOG_IMAGE={old['id']}\nBLOG_HTTP_HOST=127.0.0.1\nBLOG_HTTP_PORT=0\nBLOG_METRICS_PORT=0\n")
    base = ["docker", "compose", "--project-directory", str(root), "-f", str(root / "compose.yaml"), "-p", project]
    private_values = re.findall(r"^BLOG_\w*PASSWORD=(.+)$", env_file.read_text(), re.M)

    def compose(*args):
        result = subprocess.run([*base, *args], cwd=root, env=env, capture_output=True, text=True, timeout=180)
        if result.returncode:
            logs = subprocess.run([*base, "logs", "--no-color", "--tail", "12", "blog"], cwd=root, env=env,
                                  capture_output=True, text=True, timeout=30)
            detail = result.stderr + "\n" + logs.stdout
            for value in private_values:
                detail = detail.replace(value, "[redacted]")
            detail = re.sub(r"[a-f0-9]{64}|postgres(?:ql)?://[^\s\"']+", "[redacted]", detail)
            report["failure"] = {"operation": args[0], "diagnostic": detail[-4000:]}
        require(result.returncode == 0, f"upgrade Compose {args[0]} failed")
        return result.stdout.strip()

    def client():
        address = compose("port", "blog", "8080")
        require(bool(re.fullmatch(r"127\.0\.0\.1:\d+", address)), "upgrade fixture must bind loopback")
        return Client("http://" + address)

    def sql(query):
        return compose("exec", "-T", "db", "psql", "-XAt", "-v", "ON_ERROR_STOP=1",
                       "-U", "postgres", "-d", "blog", "-c", query)

    try:
        print("==> Upgrade: install the older release", flush=True)
        compose("up", "-d", "--no-build", "--pull", "never")
        admin = client()
        token = wait_for(lambda: re.search(r"安装码：([a-f0-9]{64})", compose("logs", "--no-color", "blog")),
                         "older release installation token missing").group(1)
        report["before"] = admin.json("GET", "/version")
        require(report["before"]["revision"] == old["revision"], "older binary and image label differ")
        password = "Upgrade-" + secrets.token_hex(16)
        private_values.extend([password, token])
        admin.json("POST", "/api/install", {"database_url": f"postgres://blog_owner:{owner}@db:5432/blog",
                   "public_base_url": admin.origin, "username": "acceptance-owner", "password": password},
                   headers={"X-Install-Token": token})
        admin.login(password)
        site_id = sql("SELECT value->>'id' FROM settings WHERE key='installation'")
        before = sql("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version").splitlines()
        require("9" not in before and "10" not in before, "baseline must precede account links and revisions")
        report["migrations_before"] = [int(version) for version in before]

        print("==> Upgrade: create published content, drafts and media on the old schema", flush=True)
        media = admin.json("POST", API + "/media?filename=upgrade.png", PNG, status=201,
                           headers={"Content-Type": "image/png"})
        body = "Original upgrade content\n\n![before](" + media["url"] + ")"
        published, drafts = [], []
        for kind in ("posts", "pages"):
            for status in ("published", "draft"):
                slug = f"upgrade-{kind}-{status}"
                item = admin.json("POST", API + "/" + kind, {"slug": slug, "title": slug, "content": body}, status=201)
                path = API + "/" + kind + "/" + item["id"]
                if status == "published":
                    admin.json("POST", path + "/publish", {"expected_version": item["version"]})
                    published.append((path, ("/posts/" if kind == "posts" else "/") + slug))
                else:
                    drafts.append(path)

        print("==> Upgrade: replace the stopped writer and migrate the same volumes", flush=True)
        theme_hash = compose("exec", "-T", "blog", "sha256sum", "/opt/blog/themes/default/theme.json")
        compose("stop", "blog")
        # Recreate the ownership of volumes originally populated by pre-0007
        # images, even when this test is launched with today's Compose file.
        compose("run", "--rm", "--no-deps", "--user", "0:0", "--cap-add", "CHOWN", "--entrypoint", "chown",
                "blog", "0:0", "/opt/blog/themes")
        env_file.write_text(env_file.read_text().replace("BLOG_IMAGE=" + old["id"], "BLOG_IMAGE=" + new["id"]))
        compose("up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "90", "blog")
        guest = client()
        report["after"] = guest.json("GET", "/version")
        require(report["after"]["revision"] == new["revision"], "candidate binary and image label differ")
        require(compose("exec", "-T", "blog", "stat", "-c", "%u:%g", "/opt/blog/themes") == "10001:10001",
                "legacy theme volume ownership was not migrated")
        require(compose("exec", "-T", "blog", "id", "-u") == "10001", "upgraded HTTP service must remain non-root")
        require(compose("exec", "-T", "blog", "sha256sum", "/opt/blog/themes/default/theme.json") == theme_hash,
                "ownership migration changed existing theme content")
        # The browser's prior session remains usable after the image replacement.
        admin.origin = guest.origin
        admin.me()
        require(sql("SELECT value->>'id' FROM settings WHERE key='installation'") == site_id, "upgrade reinstalled the site")
        after = sql("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version").splitlines()
        report["migrations_after"] = [int(version) for version in after]
        expected = [int(path.name.split("_", 1)[0]) for path in sorted((PROJECT / "migrations/postgres").glob("*.sql"))]
        require(report["migrations_after"] == expected, "candidate did not apply all migrations")
        require(guest.request("GET", media["url"])[0] == PNG, "upgrade changed the media object")
        for path in drafts:
            item = admin.json("GET", path)
            require(item["status"] == "draft" and item["content"] == body, "upgrade changed an existing draft")

        print("==> Upgrade: edit, publish and restore history of pre-upgrade content", flush=True)
        for path, public in published:
            item = admin.json("GET", path)
            require(item["content"] == body and not item["has_pending_changes"], "upgrade changed published content")
            edited = admin.json("PATCH", path, {"content": "Pending upgrade content", "expected_version": item["version"]})
            require(edited["has_pending_changes"], "legacy publication did not create an editing draft")
            require(b"Original upgrade content" in guest.request("GET", public)[0], "editing changed the live legacy publication")
            history = admin.json("GET", path + "/revisions")
            originals = [revision for revision in history
                         if admin.json("GET", path + "/revisions/" + revision["id"])["content"] == body]
            require(len(originals) == 1, "first edit must retain the pre-upgrade content in history")
            current = admin.json("POST", path + "/publish", {"expected_version": edited["version"]})
            require(b"Pending upgrade content" in guest.request("GET", public)[0], "new publication missing")
            restored = admin.json("POST", path + "/revisions/" + originals[0]["id"] + "/restore",
                                  {"expected_version": current["version"]})
            require(restored["content"] == body and restored["has_pending_changes"], "legacy history restore failed")
            require(b"Pending upgrade content" in guest.request("GET", public)[0], "restore implicitly published history")
            admin.json("POST", path + "/publish", {"expected_version": restored["version"]})
            require(b"Original upgrade content" in guest.request("GET", public)[0], "restored history could not be published")
        report["checks"] = ["legacy theme volume ownership and content", "non-root HTTP process",
                            "site identity and session", "media bytes", "existing post/page drafts",
                            "post/page editing isolation", "first-edit history", "publish and restore legacy history"]
    finally:
        compose("down", "--volumes", "--remove-orphans", "--timeout", "30")
        report["cleanup"] = "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--from-image", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    started = time.monotonic()
    report = {"started_at": dt.datetime.now(dt.timezone.utc).isoformat(), "status": "running"}
    report["script_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    try:
        old, new = image_identity(args.from_image), image_identity(args.image)
        require(old["id"] != new["id"] and old["revision"] and new["revision"], "two distinct versioned images are required")
        report["images"] = {"from": old, "to": new}
        with tempfile.TemporaryDirectory(prefix="blog-upgrade-test-") as directory:
            exercise(Path(directory), old, new, report)
        report["status"] = "passed"
    except (AcceptanceError, OSError, subprocess.SubprocessError) as error:
        report["status"] = "failed"
        print("Release upgrade failed: " + str(error))
        return 1
    finally:
        report["seconds"] = round(time.monotonic() - started, 3)
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
