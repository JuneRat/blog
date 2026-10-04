#!/usr/bin/env python3
"""Exercise plain Compose installation, backup and recovery in disposable deployments."""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile
import time
from contextlib import contextmanager

from acceptance_support import API, AcceptanceError, Client, SiteScenario, require

ROOT = Path(__file__).resolve().parents[1]


def command(args, *, env=None, data=None):
    result = subprocess.run(args, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, check=False)
    if result.returncode:
        raise AcceptanceError("isolated deployment command failed: " + " ".join(args[:3]))
    return result.stdout.decode()


def wait(action, description, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = action()
            if value: return value
        except (AcceptanceError, OSError):
            pass
        time.sleep(.4)
    raise AcceptanceError(description)


def login(client, *, key=None, token=None, password=None):
    body = {"key": key} if key else ({"installation_token": token} if token else {"username": "acceptance-owner", "password": password})
    value = client.json("POST", "/api/recovery/session", body)
    client.csrf = value["csrf"]


def status(client):
    return client.json("GET", "/api/recovery/session")


def action(client, action, data=None, expected="succeeded"):
    result = client.json("POST", "/api/recovery/action", {"action": action, "input": data or {}})
    if "job_id" not in result: return result
    deadline = time.monotonic() + 240
    while True:
        value = status(client)
        row = next((j for j in value["jobs"] if j["id"] == result["job_id"]), None)
        if row and row["status"] != "running" and not value["busy"]:
            require(row["status"] == expected, action + ": " + row.get("message", row["status"]))
            break
        require(time.monotonic() < deadline, action + " timed out")
        time.sleep(.3)
    if expected != "succeeded": return row
    return row["result"]


def upload(client, data):
    return client.json("POST", "/api/recovery/upload?offset=0&complete=true", data,
                       headers={"Content-Type": "application/octet-stream"})


@contextmanager
def deployment(image, directory, suffix, *, external_database=False):
    project = "codex-browser-" + suffix + "-" + secrets.token_hex(5)
    directory.mkdir()
    compose = directory / "compose.yaml"
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0)); port = listener.getsockname()[1]
    origin = f"http://127.0.0.1:{port}"
    token = secrets.token_hex(32)
    template = ROOT / ("compose.yaml" if external_database else "compose.postgres.yaml")
    compose.write_text(template.read_text().replace("127.0.0.1:8080:8080", f"127.0.0.1:{port}:8080"))
    env = {k:v for k,v in os.environ.items() if not k.startswith(("BLOG_", "COMPOSE_"))}
    owner_password = secrets.token_hex(32)
    db_environment = {"POSTGRES_DB":"blog", "POSTGRES_USER":"postgres",
                      "POSTGRES_PASSWORD":secrets.token_hex(32), "BLOG_OWNER_PASSWORD":owner_password}
    database_url = f"postgres://blog_owner:{owner_password}@db:5432/blog"
    override = directory / "test-overrides.json"
    overrides = {"services":{"blog":{"image":image, "environment":{"BLOG_PUBLIC_BASE_URL":origin,
        "BLOG_INSTALL_TOKEN":token, "BLOG_BACKUP_WORKER":"/fixtures/browser_recovery_fault_fixture.py"},
        "volumes":[str(ROOT / "scripts") + ":/fixtures:ro"]}}}
    if not external_database:
        overrides["services"]["db"] = {"environment":db_environment}
    override.write_text(json.dumps(overrides)); override.chmod(0o600)
    args = ["docker", "compose", "-f", str(compose), "-f", str(override), "-p", project]
    def compose_cmd(*words, data=None): return command([*args, *words], env=env, data=data)
    database_name = project + "-prepared-db"
    database_started = False
    try:
        compose_cmd("up", "-d", "blog")
        guest = Client(origin)
        wait(lambda: guest.request("GET", "/install")[0], "installer unavailable")
        require(not guest.json("GET", "/api/install", headers={"X-Install-Token":token})["database_configured"],
                "deployment unexpectedly preconfigured the application database")
        if external_database:
            # The blog is already serving the installer before a DB is prepared.
            env_file = directory / "database.env"
            env_file.write_text("".join(k + "=" + v + "\n" for k,v in db_environment.items())); env_file.chmod(0o600)
            database_image = re.search(r"^    image: (postgres:[^\s]+)$", (ROOT / "compose.postgres.yaml").read_text(), re.M)[1]
            command(["docker", "run", "-d", "--name", database_name, "--network", project + "_default", "--network-alias", "db",
                "--env-file", str(env_file), "--mount", "type=bind,src=" + str(ROOT / "ops/postgres-init.sh") + ",dst=/docker-entrypoint-initdb.d/10-blog.sh,readonly", database_image])
            database_started = True
            wait(lambda: command(["docker", "exec", database_name, "pg_isready", "-h", "127.0.0.1", "-U", "postgres", "-d", "blog"]), "prepared database unavailable")
        yield guest, token, compose_cmd, project, database_url
    finally:
        if database_started:
            command(["docker", "rm", "-f", "-v", database_name])
        compose_cmd("down", "--volumes", "--remove-orphans")


def exercise(image, directory, report, browser=False):
    with deployment(image, directory / "source", "source") as (guest, token, compose, project, database_url):
        print("==> Browser: user-configured database check, installation and saved configuration", flush=True)
        password = "Browser-" + secrets.token_hex(16) + "!"
        guest.request("POST", "/api/install/check", {"database_url":database_url}, status=403)
        guest.request("POST", "/api/install/check", {"database_url":"postgres://unused:unused@127.0.0.1:1/missing"},
                      status=400, headers={"X-Install-Token":token})
        require(guest.json("POST", "/api/install/check", {"database_url":database_url}, headers={"X-Install-Token":token})["ready"], "database check failed")
        compose("exec", "-T", "blog", "python3", "-c", "from pathlib import Path; assert not Path('/var/lib/blog/config/config.toml').exists()")
        if browser:
            env = os.environ.copy()
            env.update(BLOG_BROWSER_URL=guest.origin, BLOG_BROWSER_PASSWORD=password, BLOG_BROWSER_INSTALL_TOKEN=token,
                       BLOG_BROWSER_DATABASE_URL=database_url)
            result = subprocess.run(["pnpm", "--dir", str(ROOT / "apps/admin"), "exec", "playwright", "test", "installation.spec.ts"],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            require(result.returncode == 0, "installation UI verification failed (see Playwright report)")
            report["installation_ui"] = True
        else:
            guest.json("POST", "/api/install", {"database_url":database_url, "public_base_url":guest.origin,
                       "username":"acceptance-owner", "password":password}, headers={"X-Install-Token":token})
        compose("exec", "-T", "blog", "python3", "-c",
                "import json,os,sys,tomllib; from pathlib import Path; p=Path('/var/lib/blog/config/config.toml'); "
                "assert tomllib.loads(p.read_text())['database']['url']==json.load(sys.stdin)['url']; "
                "assert p.stat().st_mode & 0o777==0o600; assert 'DATABASE_URL' not in os.environ", data=json.dumps({"url":database_url}).encode())
        report["database_check_and_saved_config"] = True
        owner = Client(guest.origin); owner.login(password)
        scenario = SiteScenario(owner, guest, None)
        scenario.assert_installed()
        recovery = Client(guest.origin)
        recovery.request("GET", "/api/recovery/session", status=401)
        login(recovery, password=password)
        recovery.request("POST", "/api/recovery/action", {"action":"keygen"}, status=403,
                         headers={"Origin":"https://other.invalid"})
        recovery.request("POST", "/api/recovery/action", {"action":"keygen"}, status=401,
                         headers={"X-CSRF-Token":"wrong"})
        compose("exec", "-T", "blog", "/usr/local/bin/blog", "user", "create", "acceptance-reader")
        reader_password = "Reader-" + secrets.token_hex(16) + "!"
        compose("exec", "-T", "blog", "/usr/local/bin/blog", "user", "passwd", "--user", "acceptance-reader", "--password-stdin", data=reader_password.encode())
        Client(guest.origin).request("POST", "/api/recovery/session", {"username":"acceptance-reader","password":reader_password}, status=403)
        if browser:
            output = directory / "browser-recovery-key.json"
            env = os.environ.copy()
            env.update(BLOG_BROWSER_URL=guest.origin, BLOG_BROWSER_PASSWORD=password, BLOG_BROWSER_RECOVERY_KEY_OUTPUT=str(output))
            result = subprocess.run(["pnpm", "--dir", str(ROOT / "apps/admin"), "exec", "playwright", "test", "recovery.spec.ts"],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            require(result.returncode == 0, "browser UI verification failed (see Playwright report)")
            key = output.read_text()
            report["browser_ui"] = True
        else:
            key = action(recovery, "keygen")["key"]
            require(not status(recovery)["initialized"], "unconfirmed key enabled backups")
            action(recovery, "key-confirm", {"key":key})
        require(status(recovery)["initialized"], "confirmed key not enabled")
        changed_password = "Changed-" + secrets.token_hex(16) + "!"
        compose("exec", "-T", "blog", "/usr/local/bin/blog", "user", "passwd", "--user", "acceptance-owner", "--password-stdin", data=changed_password.encode())
        recovery.request("GET", "/api/recovery/session", status=401)
        password = changed_password
        login(recovery, password=password)
        owner.login(password)
        report["recovery_session_revocation"] = True
        post = scenario.create("posts", "browser-backup-post", content="Before the backup")
        snapshot_session = owner.clone()
        print("==> Browser: encrypted backup, bounded upload and compatibility validation", flush=True)
        result = action(recovery, "backup")
        archive = result["name"]
        raw, headers = recovery.request("GET", "/api/recovery/download/" + archive)
        require(raw.startswith(b"age-encryption.org/v1\n") and b"Before the backup" not in raw,
                "backup was not encrypted")
        require(headers.get("Content-Disposition", "").startswith("attachment"), "backup did not download")
        uploaded = upload(recovery, raw)
        preview = action(recovery, "inspect", {"name":uploaded["name"], "imported":True, "key":key})
        require(preview["compatible"], "current backup failed compatibility checks")
        damaged = upload(recovery, raw[:-20])
        action(recovery, "inspect", {"name":damaged["name"], "imported":True, "key":key}, expected="failed")
        guest.request("GET", "/readyz")
        print("==> Browser: in-place restore, content rollback and revoked sessions", flush=True)
        owner.json("PATCH", API + "/posts/" + post["id"], {"expected_version":post["version"], "title":"Changed after backup", "content":"New content", "slug":post["slug"]})
        restore = action(recovery, "restore", {"name":archive, "key":key, "confirm":"恢复此站点"})
        require(restore["rollback"], "restore did not create a rollback copy")
        guest.request("GET", "/readyz")
        snapshot_session.request("GET", API + "/me", status=401)
        owner.login(password)
        restored = owner.json("GET", API + "/posts/" + post["id"])
        require(restored["content"] == "Before the backup", "restore did not replace business data")
        login(recovery, key=key)
        report["in_place_restore"] = True
        print("==> Browser: interrupted restore remains closed and encrypted rollback recovers", flush=True)
        owner.json("PATCH", API + "/posts/" + post["id"], {"expected_version":restored["version"],
                   "title":"State before interruption", "content":"Rollback snapshot preserved", "slug":post["slug"]})
        # The wrapper blocks immediately after the database transaction, before files are changed.
        compose("exec", "-T", "blog", "python3", "-c", "from pathlib import Path; Path('/var/lib/blog/config/recovery/TEST_INTERRUPT_RESTORE').touch()")
        submitted = recovery.json("POST", "/api/recovery/action", {"action":"restore", "input":{"name":archive,"key":key,"confirm":"恢复此站点"}})
        def restoring_files():
            return next((j for j in status(recovery)["jobs"] if j["id"] == submitted["job_id"] and j["phase"] == "files"), None)
        interrupted = wait(restoring_files, "restore did not reach the interruption point")
        require(interrupted["rollback"], "interrupted restore has no rollback")
        compose("restart", "blog")
        wait(lambda: guest.request("GET", "/recovery")[0], "recovery page missing after interrupted restore")
        recovery = Client(guest.origin); login(recovery, key=key)
        require(status(recovery)["recovery_required"], "interrupted restore reopened the website")
        guest.request("GET", "/readyz", status=503)
        recovery.request("POST", "/api/recovery/action", {"action":"resume"}, status=400)
        action(recovery, "restore", {"name":interrupted["rollback"], "key":key, "confirm":"恢复此站点"})
        guest.request("GET", "/readyz")
        login(recovery, key=key)
        owner.login(password)
        require(owner.json("GET", API + "/posts/" + post["id"])["content"] == "Rollback snapshot preserved", "rollback did not recover the pre-restore state")
        report["interruption_and_rollback"] = True
        print("==> Browser: remote storage connection, upload, download and failure handling", flush=True)
        fixture_name = project + "-s3"
        fixture = subprocess.Popen(["docker", "run", "--rm", "-i", "--name", fixture_name,
            "--network", project + "_default", "--network-alias", "recovery-storage",
            "--read-only", "--cap-drop", "ALL", "--log-driver", "none", "--mount", "type=bind,src=" + str(ROOT / "scripts") + ",dst=/fixtures,readonly",
            "--entrypoint", "python3", image, "-B", "/fixtures/browser_s3_fixture.py"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        access = secrets.token_hex(12); secret = secrets.token_hex(32)
        try:
            fixture.stdin.write(json.dumps({"access_key":access}) + "\n"); fixture.stdin.flush()
            require(fixture.stdout.readline().strip() == "ready", "S3 protocol fixture did not start")
            action(recovery, "remote-save", {"remote":{"endpoint":"http://recovery-storage:9000", "region":"us-east-1", "bucket":"backups", "prefix":"site-test", "access_key":access, "secret_key":secret}})
            remote_backup = action(recovery, "backup")
            require(not remote_backup.get("warning"), "remote upload failed")
            remote = action(recovery, "remote-list")
            require(any(b["name"] == remote_backup["name"] for b in remote["backups"]), "remote backup is missing")
            downloaded = action(recovery, "remote-download", {"name":remote_backup["name"]})
            action(recovery, "inspect", {"name":downloaded["name"], "imported":True, "key":key})
            # Worker status never returns credentials to the browser.
            public = json.dumps(status(recovery))
            require(access not in public and secret not in public, "remote credentials appeared in public job state")
        finally:
            command(["docker", "rm", "-f", fixture_name]); fixture.wait(timeout=15)
            fixture.stdin.close(); fixture.stdout.close()
        failed_remote = action(recovery, "backup")
        require(failed_remote.get("warning"), "remote outage was not reported")
        guest.request("GET", "/readyz")
        action(recovery, "remote-save", {"remote":None})
        report["s3_protocol_round_trip"] = True
        action(recovery, "schedule", {"settings":{"schedule":"daily", "hour_utc":18, "weekday":0, "keep":3, "remote_keep":5}})
        require(status(recovery)["settings"]["next_run"] > time.time(), "schedule not persisted")
        print("==> Browser: database outage, durable emergency access and restart", flush=True)
        compose("stop", "db")
        compose("restart", "blog")
        wait(lambda: Client(guest.origin).request("GET", "/recovery")[0], "emergency page disappeared with database")
        recovery = Client(guest.origin); login(recovery, key=key)
        require(status(recovery)["maintenance"], "database failure did not keep business site closed")
        compose("start", "db")
        wait(lambda: compose("exec", "-T", "db", "pg_isready", "-U", "postgres", "-d", "blog"), "database did not recover")
        action(recovery, "resume")
        guest.request("GET", "/readyz")
        require(status(recovery)["settings"]["keep"] == 3, "schedule settings lost after restart")
        report["emergency_access"] = True
        print("==> Browser: fresh deployment restored entirely through HTTP", flush=True)
        with deployment(image, directory / "fresh", "fresh", external_database=True) as (fresh, fresh_token, fresh_compose, fresh_project, fresh_database_url):
            other = Client(fresh.origin); login(other, token=fresh_token)
            imported = upload(other, raw)
            action(other, "inspect", {"name":imported["name"], "imported":True, "key":key})
            action(other, "restore", {"name":imported["name"], "imported":True, "key":key, "confirm":"恢复此站点", "allow_without_snapshot":True,
                   "database_url":fresh_database_url,"public_base_url":fresh.origin})
            fresh.request("GET", "/readyz")
            fresh_owner = Client(fresh.origin); fresh_owner.login(password)
            require(fresh_owner.json("GET", API + "/posts/" + post["id"])["content"] == "Before the backup", "fresh restore lost data")
            fresh_compose("restart", "blog")
            wait(lambda: fresh.request("GET", "/readyz")[0], "fresh restore did not survive restart")
            fresh_recovery = Client(fresh.origin); login(fresh_recovery, key=key)
            require(status(fresh_recovery)["initialized"], "fresh restore did not pair its emergency key")
            report["fresh_restore"] = True
            report["external_database"] = True
        # Keep only public evidence: never include keys, login tokens or deployment passwords.
        logs = compose("logs", "--no-color", "blog")
        for secret in (password, key, json.loads(key)["emergency_token"], json.loads(key)["identity"].strip()):
            require(secret not in logs, "private value appeared in application logs")
        report["credentials_not_logged"] = True
        report["archive_bytes"] = len(raw)
        report["archive_sha256"] = hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--browser", action="store_true", help="also exercise the embedded UI with local Playwright")
    args = parser.parse_args()
    report = {"started_at":dt.datetime.now(dt.timezone.utc).isoformat(), "status":"running"}
    started = time.monotonic()
    try:
        info = json.loads(command(["docker", "image", "inspect", args.image]))[0]
        report["image"] = {"id":info["Id"], "revision":info.get("Config", {}).get("Labels", {}).get("org.opencontainers.image.revision"),
                           "architecture":info["Architecture"], "os":info["Os"]}
        report["scripts_sha256"] = {name:hashlib.sha256((ROOT / "scripts" / name).read_bytes()).hexdigest()
                                    for name in ("browser_recovery.py", "test_browser_compose.py")}
        report["compose_sha256"] = {name:hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                    for name in ("compose.yaml", "compose.postgres.yaml")}
        with tempfile.TemporaryDirectory(prefix="blog-browser-acceptance-") as directory:
            exercise(info["Id"], Path(directory), report, args.browser)
        report["status"] = "passed"
        print("Browser backup, in-place restore and fresh deployment recovery passed.")
        return 0
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error) if isinstance(error, AcceptanceError) else type(error).__name__
        print("Browser recovery verification failed: " + report["error"])
        return 1
    finally:
        report["seconds"] = round(time.monotonic() - started, 3)
        if args.report: args.report.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
