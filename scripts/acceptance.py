#!/usr/bin/env python3
"""Real HTTP acceptance: install, author, moderate, back up and restore a fresh site.

Requires BLOG_TEST_ADMIN_URL on loopback, a built blog binary and admin SPA.
BLOG_TEST_PG_CONTAINER selects PostgreSQL tools inside the same test container.
Only randomly named databases created by this run are removed. Existing site
configuration, databases and media are never used. Reports contain no credentials.
"""

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlsplit, urlunsplit
import uuid

import recovery
from acceptance_support import API, Client, AcceptanceError, SiteScenario, require

PROJECT = Path(__file__).resolve().parent.parent


def clean_env():
    # Never inherit a developer's installed site, recovery mode, database service
    # file or proxy routing. PostgreSQL credentials are supplied explicitly later.
    return {key: value for key, value in os.environ.items()
            if key != "DATABASE_URL" and not key.startswith(("BLOG_", "PG"))}


def admin_url(value):
    try:
        config = recovery.db_config(value)
        parsed = urlsplit(value)
        require(config["PGHOST"] in ("127.0.0.1", "localhost", "::1"),
                "BLOG_TEST_ADMIN_URL must point to a loopback test instance")
        require(not parsed.query and not parsed.fragment,
                "BLOG_TEST_ADMIN_URL must not contain connection overrides")
        return value
    except (ValueError, recovery.RecoveryError):
        raise AcceptanceError("set an explicit PostgreSQL BLOG_TEST_ADMIN_URL") from None


def database_url(url, name):
    require(bool(recovery.SAFE_NAME.fullmatch(name)), "invalid test database name")
    parsed = urlsplit(url)
    return urlunsplit((parsed.scheme, parsed.netloc, "/" + name, "", ""))


def run_command(command, env, label, success=True, timeout=120):
    # Do not echo arguments, environments or subprocess output: installation
    # logs include the one-time token and failures can include connection URLs.
    try:
        result = subprocess.run(command, cwd=PROJECT, env=env, capture_output=True,
                                text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        raise AcceptanceError(f"{label}: timed out") from None
    require((result.returncode == 0) == success,
            f"{label}: unexpected exit status {result.returncode}")
    return result.stdout


class TestDatabase(recovery.PgTools):
    def execute(self, tool, args, database="postgres"):
        command, env = self.command(tool, args, database)
        safe_env = clean_env()
        if not self.container:
            safe_env.update({key: value for key, value in env.items() if key in self.config})
        return run_command(command, safe_env, tool, timeout=30).strip()

    def query(self, sql, database=None):
        return self.execute("psql", ["-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql],
                            database or self.config["PGDATABASE"])


class Acceptance(SiteScenario):
    def __init__(self, args, root):
        self.args = args
        self.root = root
        self.pg = TestDatabase(admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", "")),
                               os.environ.get("BLOG_TEST_PG_CONTAINER"))
        suffix = uuid.uuid4().hex
        self.source = "blog_acceptance_" + suffix
        self.target = "blog_restore_acceptance_" + suffix
        self.owned = set()
        self.process = None
        self.log = None
        self.bind = "127.0.0.1:0"
        self.origin = None
        self.password = "Test-" + secrets.token_hex(16) + "!"
        self.config = root / "config.toml"
        self.media_dir = root / "media"
        self.restore_dir = root / "restore"
        self.backup_dir = root / "backup"
        self.steps = []
        self.evidence = {}

    def env(self, database=None, restored=False, isolated=False):
        env = clean_env()
        theme = self.restore_dir / "resources/installed-themes/default" if restored else PROJECT / "themes/default"
        media = self.restore_dir / "resources/media" if restored else self.media_dir
        env.update(BLOG_CONFIG_FILE=str(self.config), BLOG_ADMIN_DIST=str(self.args.admin_dist),
                   BLOG_MIGRATIONS_DIR=str(PROJECT / "migrations/postgres"),
                   BLOG_THEME_DIR=str(theme), BLOG_MEDIA_DIR=str(media), BLOG_SECURE_COOKIES="0",
                   BLOG_RECOVERY_MODE="1" if isolated else "0", RUST_LOG="warn")
        if database:
            env["DATABASE_URL"] = database_url(self.args.admin_url, database)
        return env

    def stage(self, name, action):
        print(f"==> {name}", flush=True)
        start = time.monotonic()
        item = {"name": name, "status": "failed"}
        self.steps.append(item)
        try:
            action()
            item["status"] = "passed"
        finally:
            item["seconds"] = round(time.monotonic() - start, 3)

    def query(self, sql, restored=False):
        return self.pg.query(sql, self.target if restored else self.source)

    def prepare(self):
        require(self.args.binary.is_file(), "build the server first: cargo build -p server --bin blog")
        require((self.args.admin_dist / "index.html").is_file(),
                "build the admin SPA first: pnpm --dir apps/admin build")
        self.evidence.update(server_sha256=recovery.digest(self.args.binary),
                             admin_entry_sha256=recovery.digest(self.args.admin_dist / "index.html"),
                             scenarios_sha256=recovery.digest(PROJECT / "scripts/acceptance_support.py"),
                             runner_sha256=recovery.digest(Path(__file__)))
        for name in (self.source, self.target):
            require(not self.pg.query(f"SELECT 1 FROM pg_database WHERE datname='{name}'", "postgres"),
                    "random test database already exists; refusing to reuse it")
        self.pg.execute("createdb", ["--template=template0", self.source])
        self.owned.add(self.source)

    def start(self, database=None, restored=False, isolated=False, installer=False):
        require(self.process is None, "previous server must stop before starting another")
        path = self.root / f"server-{len(self.steps)}.log"
        self.log = path.open("wb")
        self.process = subprocess.Popen([str(self.args.binary), "serve", "--addr", self.bind],
                                        cwd=PROJECT, env=self.env(database, restored, isolated),
                                        stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "server exited before becoming ready")
            log = path.read_text(errors="replace")
            pattern = r"首次安装：(http://127\.0\.0\.1:\d+)/install" if installer else r"公开站点已启动：(http://127\.0\.0\.1:\d+)"
            address = re.search(pattern, log)
            token = re.search(r"安装码：([a-f0-9]{64})", log)
            if address and (token or not installer):
                self.origin = address.group(1)
                self.bind = urlsplit(self.origin).netloc
                self.guest = Client(self.origin)
                return token.group(1) if token else None
            time.sleep(0.05)
        raise AcceptanceError("server readiness timed out")

    def stop(self):
        if self.process is not None:
            process = self.process
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None

    def install(self):
        token = self.start(installer=True)
        _, headers = self.guest.request("GET", "/", status=303)
        require(headers.get("Location") == "/install", "fresh site must redirect to installation")
        self.guest.request("GET", "/healthz", status=503)
        self.guest.request("GET", "/install")
        self.guest.request("GET", "/api/install", status=403)
        info = self.guest.json("GET", "/api/install", headers={"X-Install-Token": token})
        require(info["database_configured"] is False, "installation must start without saved database")
        installed = self.guest.json("POST", "/api/install", {
            "database_url": database_url(self.args.admin_url, self.source), "public_base_url": self.origin,
            "username": "acceptance-owner", "password": self.password,
        }, headers={"X-Install-Token": token})
        require(installed["redirect"] == "/admin/", "installation must return a working admin entry")
        self.assert_installed()
        require(self.config.stat().st_mode & 0o077 == 0, "saved database credentials must be private")
        require(not self.config.with_suffix(".install-state.json").exists(),
                "completed installation must remove its temporary journal")
        config = self.config.read_text()
        require(self.password not in config and token not in config,
                "deployment configuration must not persist passwords or one-time tokens")
        self.installation_id = self.query("SELECT value->>'id' FROM settings WHERE key='installation'")
        require(re.fullmatch(r"[0-9a-f]{64}", self.installation_id) is not None,
                "database must retain the installation completion marker")
        require(self.query("SELECT count(*) FROM audit_logs WHERE action='installation.complete'") == "1",
                "first administrator and installation completion must be audited once")


    def identity(self):
        self.admin = Client(self.origin)
        me = self.admin.login(self.password)
        require({"admin.manage", "audit.read", "settings.manage"} <= set(me["permissions"]),
                "installed administrator must receive the permission registry")
        self.admin.json("PUT", API + "/me/profile", {
            "display_name": "Acceptance Owner", "bio": "安装与恢复验收", "expected_version": me["version"],
        })
        after = self.admin.me()
        require(after["user_id"] == me["user_id"] and after["version"] == me["version"] + 1,
                "profile update must preserve the authenticated session")
        self.admin.json("PUT", API + "/me/profile", {
            "display_name": "Stale edit", "bio": None, "expected_version": me["version"],
        }, status=409)
        previous = self.admin.clone()
        new_password = "Changed-" + secrets.token_hex(16) + "!"
        self.admin.json("POST", API + "/me/password", {
            "current_password": self.password, "new_password": new_password,
        })
        self.password = new_password
        self.admin.me()
        previous.request("GET", API + "/me", status=401)
        self.stop()
        self.start()  # No DATABASE_URL: boot entirely from the installation journal.
        self.assert_installed()
        require(self.admin.me()["display_name"] == "Acceptance Owner",
                "session and profile must survive a real process restart")


    def schedule_and_stop(self):
        scheduled = []
        for kind in ("posts", "pages"):
            item = self.create(kind, "acceptance-scheduled-" + kind)
            scheduled.append((kind, item))
        self.due = dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=8)
        for kind, item in scheduled:
            result = self.action(kind, item, "schedule", published_at=self.due.isoformat())
            require(result["status"] == "scheduled", f"{kind}: appointment must be persisted")
        self.old_session = self.admin.clone()
        self.stop()  # Prove our only writer is gone before asserting maintenance.
        require(dt.datetime.now(dt.timezone.utc) < self.due, "source writer stopped too late for scheduling drill")

    def recovery_command(self, action, args, database=None):
        command = [sys.executable, "-B", str(PROJECT / "scripts/recovery.py"), action, *map(str, args)]
        if self.pg.container and action != "verify":
            command.extend(["--docker-container", self.pg.container])
        env = self.env()
        env["DATABASE_URL"] = database_url(self.args.admin_url, database) if database else self.args.admin_url
        return json.loads(run_command(command, env, "recovery " + action))

    def backup_restore(self):
        require(self.process is None, "backup requires the owned writer to be stopped")
        self.recovery_command("backup", ["--output", self.backup_dir, "--theme-dir", PROJECT / "themes/default",
                                          "--media-dir", self.media_dir, "--maintenance-confirmed"], self.source)
        self.recovery_command("verify", [self.backup_dir])
        manifest = json.loads((self.backup_dir / "manifest.json").read_text())
        result = self.recovery_command("restore", [self.backup_dir, "--target-db", self.target,
                                                    "--output", self.restore_dir, "--isolation-confirmed"])
        self.owned.add(self.target)
        require(result["counts"] == {**manifest["database_counts"], "sessions": 0},
                "all business row counts must survive restore; sessions must be revoked")
        require(result["verified_media"] == 1 and result["verified_references"] == 7,
                "restore must verify original media and all references")
        require(self.query("SELECT value->>'id' FROM settings WHERE key='installation'", True) == self.installation_id,
                "restore must retain the installation completion marker")
        self.evidence.update(schema=result["schema"]["id"], migrations=result["schema"]["migrations"],
                             backup_counts=manifest["database_counts"],
                             verified_media=result["verified_media"], verified_references=result["verified_references"])

    def isolated_verification(self):
        for command in (["serve", "--addr", "127.0.0.1:0"], ["publish-due"]):
            run_command([str(self.args.binary), *command], self.env(self.target, restored=True),
                        "recovery isolation guard", success=False, timeout=30)
        # Let the appointment become due while no server exists, then start the
        # verification server. An accidentally enabled initial scheduler tick
        # would publish immediately, so no 30-second polling delay is needed.
        deadline = time.monotonic() + 10
        while dt.datetime.now(dt.timezone.utc) <= self.due:
            require(time.monotonic() < deadline, "scheduled time wait exceeded its deadline")
            time.sleep(0.05)
        self.start(self.target, restored=True, isolated=True)
        self.assert_installed()
        self.old_session.request("GET", API + "/me", status=401)
        self.admin = Client(self.origin)
        self.admin.login(self.password)
        self.assert_public_content()
        self.assert_comments()
        for kind, prefix in (("posts", "/posts/"), ("pages", "/")):
            require(self.query(f"SELECT status FROM {kind} WHERE slug='acceptance-scheduled-{kind}'", True) == "scheduled",
                    "recovery verification must not run scheduled publication")
            self.guest.request("GET", prefix + "acceptance-scheduled-" + kind, status=404)
        self.verification_session = self.admin.clone()
        self.stop()

    def release_and_reopen(self):
        require(self.process is None, "stop verification before releasing the recovered database")
        self.recovery_command("release", ["--output", self.restore_dir, "--verification-confirmed"])
        require(self.query("SELECT count(*) FROM sessions", True) == "0", "release must revoke verification sessions")
        # Adapt the saved deployment file to the new connection, retaining the
        # original installation identity. Boot without DATABASE_URL to catch a
        # recovered installed site accidentally reopening installation.
        self.config.write_text("config_version = 1\n[database]\nurl = " +
                               json.dumps(database_url(self.args.admin_url, self.target)) +
                               "\n[server]\npublic_base_url = " + json.dumps(self.origin) + "\n")
        self.start(restored=True)
        self.assert_installed()
        self.verification_session.request("GET", API + "/me", status=401)
        self.admin = Client(self.origin)
        self.admin.login(self.password)
        self.assert_public_content()
        self.assert_comments()
        deadline = time.monotonic() + 10
        for kind, prefix in (("posts", "/posts/"), ("pages", "/")):
            while self.query(f"SELECT status FROM {kind} WHERE slug='acceptance-scheduled-{kind}'", True) != "published":
                require(time.monotonic() < deadline, "normal startup must resume due publication")
                time.sleep(0.1)
            self.guest.request("GET", prefix + "acceptance-scheduled-" + kind)
        self.create("posts", "acceptance-after-recovery")
        detail = self.admin.json("GET", API + "/media/" + self.media["id"])["media"]
        require(detail["deleted_at"] is not None, "media trash state must survive restoration")
        self.admin.json("POST", API + "/media/" + self.media["id"] + "/restore",
                        {"expected_version": detail["version"]}, status=204)
        self.assert_image()
        _, headers = self.admin.request("POST", "/auth/logout", status=303)
        require(headers.get("Location") == "/", "logout must return to the public site")
        self.admin.request("GET", API + "/me", status=401)
        self.stop()

    def cleanup(self):
        self.stop()
        # A failed pg_restore still belongs to this run only if its database
        # guard matches the random tag written into our private restore output.
        marker = self.restore_dir / "ISOLATED"
        errors = []
        if self.target not in self.owned and marker.is_file():
            try:
                tag = marker.read_text().strip()
                actual = self.pg.query("SELECT COALESCE(shobj_description(oid,'pg_database'), 'missing guard') "
                                       f"FROM pg_database WHERE datname='{self.target}'", "postgres")
                if tag.startswith(recovery.ISOLATION_PREFIX) and tag == actual:
                    self.owned.add(self.target)
                elif actual:
                    errors.append(self.target + " (ownership unproven; left untouched)")
            except (AcceptanceError, OSError):
                errors.append(self.target + " (could not verify ownership)")
        for name in sorted(self.owned):
            try:
                self.pg.query(f'DROP DATABASE "{name}" WITH (FORCE)', "postgres")
                self.owned.remove(name)
            except AcceptanceError:
                errors.append(name)
        require(not errors, "could not clean owned test databases: " + ", ".join(errors))

    def run(self):
        for name, action in (
            ("fresh database", self.prepare), ("first-run installation", self.install),
            ("identity and persistent sessions", self.identity), ("media, taxonomy and writing", self.media_and_content),
            ("content and media trash", self.lifecycles), ("nested comments and moderation", self.comments),
            ("scheduled publication and writer shutdown", self.schedule_and_stop), ("backup and isolated restore", self.backup_restore),
            ("isolated HTTP verification", self.isolated_verification), ("release and recovered site", self.release_and_reopen),
        ):
            self.stage(name, action)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/debug/blog")
    parser.add_argument("--admin-dist", type=Path, default=PROJECT / "apps/admin/dist")
    parser.add_argument("--report", type=Path, help="new JSON report path (no secrets, never overwrites)")
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    args.admin_dist = args.admin_dist.resolve()
    report = {"format": 1, "status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat()}
    start = time.monotonic()
    try:
        args.admin_url = admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", ""))
        require(args.report is None or not args.report.exists(), "report already exists; choose a new path")
        with tempfile.TemporaryDirectory(prefix="blog-acceptance-") as directory:
            suite = Acceptance(args, Path(directory))
            try:
                suite.run()
            finally:
                try:
                    suite.stage("cleanup", suite.cleanup)
                finally:
                    report.update(steps=suite.steps, evidence=suite.evidence)
        report["status"] = "passed"
    except AcceptanceError as error:
        report["error"] = str(error)
    except KeyboardInterrupt:
        report["error"] = "interrupted"
    except Exception as error:
        # Unknown exceptions can embed request bodies or environment values.
        report["error"] = "unexpected " + type(error).__name__
    report["seconds"] = round(time.monotonic() - start, 3)
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=PROJECT, capture_output=True, text=True, check=False)
    report["git_commit"] = revision.stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=PROJECT,
                           capture_output=True, text=True, check=False)
    report["tracked_changes"] = bool(dirty.stdout.strip())
    if args.report:
        try:
            with args.report.open("x") as output:
                json.dump(report, output, ensure_ascii=False, indent=2)
                output.write("\n")
        except OSError:
            print("acceptance: could not create the requested report", file=sys.stderr)
            return 1
    if report["status"] != "passed":
        print("acceptance: " + report["error"], file=sys.stderr)
        return 1
    print(f"Acceptance passed in {report['seconds']}s; temporary databases and files removed.")
    return 0


def interrupt(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    sys.exit(main())
