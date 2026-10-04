#!/usr/bin/env python3
"""Exercise the built Compose image using only disposable containers and volumes.

No host Rust/Node build or existing deployment configuration is used. A random
Compose project and an isolated copy of the deployment files own every resource
removed by this test. Server logs (including installation tokens) stay private.
"""
import argparse
import base64
from contextlib import contextmanager
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import select
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError
from urllib.request import Request
from urllib.parse import urlsplit
import uuid

from acceptance_support import API, PNG, AcceptanceError, Client, SiteScenario, require
from acceptance_smtp_tls_support import certificates
from compose_recovery import dotenv

PROJECT = Path(__file__).resolve().parents[1]


def wait_for(action, message, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = action()
            if value:
                return value
        except AcceptanceError:
            pass
        time.sleep(0.5)
    raise AcceptanceError(message)


@contextmanager
def smtp_sink(image, container, name, scripts, env, config, evidence, logs):
    """Share only the owned application's loopback; tokens use private pipes."""
    process = subprocess.Popen([
        "docker", "run", "--rm", "-i", "--name", name, "--log-driver", "none",
        "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
        "--tmpfs", "/tmp:rw,noexec,nosuid,size=1m,mode=1777",
        "--user", "10001:10001", "--network", "container:" + container,
        "--mount", f"type=bind,src={scripts},dst=/fixtures,readonly",
        "--entrypoint", "python3", image, "-B", "/fixtures/acceptance_smtp_tls_support.py", "--port", "2525",
    ], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    private_values = [config["username"], config["password"], config["key"],
                      base64.b64encode(("\0" + config["username"] + "\0" + config["password"]).encode()).decode()]

    def response():
        require(bool(select.select([process.stdout], [], [], 30)[0]), "SMTP fixture response timed out")
        line = process.stdout.readline(4097)
        require(bool(line) and len(line) <= 4096, "SMTP fixture closed or sent an oversized response")
        try:
            result = json.loads(line)
        except ValueError:
            raise AcceptanceError("invalid SMTP fixture response") from None
        require(isinstance(result, dict) and "error" not in result, "SMTP fixture delivery failed")
        return result

    def token(recipient, origin):
        process.stdin.write(json.dumps({"recipient": recipient, "origin": origin}) + "\n")
        process.stdin.flush()
        value = response().get("token", "")
        require(isinstance(value, str) and bool(re.fullmatch(r"[a-f0-9]{64}", value)), "invalid SMTP token response")
        private_values.extend((value, recipient))
        return value

    try:
        process.stdin.write(json.dumps(config) + "\n")
        process.stdin.flush()
        require(response() == {"port": 2525}, "SMTP fixture did not bind its loopback port")
        yield token
        process.stdin.write(json.dumps({"snapshot": True}) + "\n")
        process.stdin.flush()
        counters = response().get("smtp", {})
        verify_smtp_counters(counters, config["security"])
        application_logs = logs()
        require(not any(value in application_logs for value in private_values), "SMTP private data appeared in application logs")
        evidence.update(transport=config["security"] + " authenticated container-loopback SMTP",
                        smtp=counters, private_logs_verified=True)
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            subprocess.run(["docker", "rm", "--force", name], env=env, capture_output=True, timeout=30, check=True)
            process.wait(timeout=5)
        process.stdout.close()


def verify_smtp_counters(counters, security):
    require(isinstance(counters, dict) and security in ("tls", "starttls"), "invalid SMTP evidence")
    require(all(counters.get(key) == 2 for key in
                ("accepted_messages", "received_messages", "authenticated", "tls_handshakes", "ehlo_tls")),
            "both account emails must use authenticated TLS")
    require(not counters.get("cleartext_auth") and not counters.get("cleartext_mail"),
            "credentials or mail were sent without TLS")
    if security == "starttls":
        require(counters.get("starttls_commands") == 2, "both emails must upgrade STARTTLS")


def exercise(image, root, ops_image, report, smtp_security="starttls"):
    last_stage = time.monotonic()

    def stage(name):
        nonlocal last_stage
        current = time.monotonic()
        if report["steps"]:
            report["steps"][-1].update(status="passed", seconds=round(current - last_stage, 3))
        report["steps"].append({"name": name, "status": "running"})
        last_stage = current
        print("==> Compose: " + name, flush=True)

    project = "blog-compose-test-" + uuid.uuid4().hex[:12]
    password = "Compose-" + secrets.token_hex(16)
    special_secret = "literal-$MISSING-'quoted'-\\line\nsecond"
    env_file = root / ".env"
    (root / "scripts").mkdir()
    shutil.copyfile(PROJECT / "scripts/compose-init.sh", root / "scripts/compose-init.sh")
    shutil.copyfile(PROJECT / "scripts/compose-backup.sh", root / "scripts/compose-backup.sh")
    for filename in ("acceptance_smtp.py", "acceptance_support.py", "acceptance_smtp_tls_support.py"):
        shutil.copyfile(PROJECT / "scripts" / filename, root / "scripts" / filename)
    certs = certificates(root / "certificates")
    smtp_config = {"security": smtp_security, "username": "smtp-" + secrets.token_hex(8),
                   "password": secrets.token_hex(24) + "-$MISSING-'quoted'-\\line\nsecond",
                   "certificate": (certs / "valid.crt").read_text(), "key": (certs / "valid.key").read_text()}
    smtp_environment = {"BLOG_SMTP_HOST": "127.0.0.1", "BLOG_SMTP_PORT": "2525",
                        "BLOG_SMTP_SECURITY": smtp_security, "BLOG_SMTP_FROM": "blog@acceptance.invalid",
                        "BLOG_SMTP_USERNAME": smtp_config["username"], "BLOG_SMTP_PASSWORD": smtp_config["password"],
                        "BLOG_SMTP_CA_PEM": (certs / "ca.crt").read_text()}
    report["smtp_security"] = smtp_security
    report["smtp_ca_sha256"] = hashlib.sha256(smtp_environment["BLOG_SMTP_CA_PEM"].encode()).hexdigest()
    shutil.copyfile(PROJECT / ".env.example", root / ".env.example")
    initialized = subprocess.run(["sh", str(root / "scripts/compose-init.sh")], cwd=root,
                                 capture_output=True, text=True, timeout=10)
    require(initialized.returncode == 0, "environment initialization failed")
    owner_password = re.search(r"^BLOG_OWNER_PASSWORD=([a-f0-9]{64})$", env_file.read_text(), re.M).group(1)
    with env_file.open("a") as stream:
        stream.write(f"\nBLOG_IMAGE={image}\nBLOG_OPS_IMAGE={ops_image}\nCOMPOSE_PROJECT_NAME={project}\nBLOG_HTTP_HOST=127.0.0.1\nBLOG_HTTP_PORT=0\nBLOG_METRICS_PORT=0\nRUST_LOG=info,sqlx=warn\n")
        stream.write(dotenv({"GH_SECRET": special_secret, "BLOG_DB_MAX_CONNECTIONS": "9", "BLOG_DB_STATEMENT_TIMEOUT_MS": "20000"}))
        stream.write(dotenv(smtp_environment))
    shutil.copyfile(PROJECT / "compose.legacy.yaml", root / "compose.yaml")
    (root / "ops").mkdir()
    shutil.copyfile(PROJECT / "ops/postgres-init.sh", root / "ops/postgres-init.sh")
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("BLOG_", "COMPOSE_"))
           and key not in ("DATABASE_URL", "RUST_LOG", "IDP_SECRET", "GH_SECRET")}
    base = ["docker", "compose", "--project-directory", str(root),
            "-f", str(root / "compose.yaml"), "-p", project]

    def compose(*args, data=None):
        result = subprocess.run([*base, *args], cwd=root, env=env,
                                input=data, capture_output=True, text=True, timeout=180)
        # Do not echo command output: a failed operation may contain credentials.
        require(result.returncode == 0, f"Compose {args[0]} failed (exit {result.returncode})")
        return result.stdout.strip()

    def client(port="8080"):
        address = compose("port", "blog", port)
        require(re.fullmatch(r"127\.0\.0\.1:\d+", address), "unexpected published address")
        return Client("http://" + address)

    def sql(query):
        return compose("exec", "-T", "db", "psql", "-XAt", "-v", "ON_ERROR_STOP=1",
                       "-U", "postgres", "-d", "blog", "-c", query)

    def healthy(service):
        container = compose("ps", "-q", service)
        result = subprocess.run(["docker", "inspect", "--format", "{{json .State.Health.Status}}", container],
                                capture_output=True, text=True, check=True)
        return json.loads(result.stdout) == "healthy"

    restored = root / "restored"
    restored_again = root / "restored-again"

    def operation(*args, directory=root, data=None, success=True):
        result = subprocess.run(["sh", str(directory / "scripts/compose-backup.sh"), *args],
                                cwd=directory, env=env, input=data, capture_output=True, text=True, timeout=240)
        # The container wrapper emits sanitized status lines. Keep arbitrary
        # Docker output and application stderr (which can include secrets) private.
        diagnostic = next((line for line in result.stderr.splitlines()
                           if line.startswith(f"Compose {args[0]}:")
                           or line.startswith(f"Compose {args[0]} failed (")), "")
        if (result.returncode == 0) != success and not diagnostic:
            # Docker/host failures occur before the sanitized Python wrapper.
            # Read this disposable deployment's resolved environment and redact
            # every value before exposing bounded CLI diagnostics in CI logs.
            config = subprocess.run(["docker", "compose", "--project-directory", str(directory),
                                     "config", "--format", "json"], cwd=directory, env=env,
                                    capture_output=True, text=True, timeout=30)
            if config.returncode == 0:
                diagnostic = result.stderr
                values = {str(value) for service in json.loads(config.stdout)["services"].values()
                          for value in service.get("environment", {}).values() if value is not None and str(value)}
                for value in sorted(values, key=len, reverse=True):
                    diagnostic = diagnostic.replace(value, "[redacted]")
                diagnostic = re.sub(r"postgres(?:ql)?://[^\s]+", "[database URL]", diagnostic)[-2000:]
        require((result.returncode == 0) == success,
                f"Compose recovery {args[0]} unexpected exit {result.returncode}"
                + (f"; {diagnostic}" if diagnostic else ""))
        return result.stdout

    def restored_compose(*args, data=None):
        result = subprocess.run(["docker", "compose", "--project-directory", str(restored), *args],
                                cwd=restored, env=env, input=data, capture_output=True, text=True, timeout=120)
        require(result.returncode == 0, f"Restored Compose {args[0]} failed")
        return result.stdout.strip()

    try:
        compose("config", "--quiet")
        # .env is auto-discovered, but cluster/owner passwords stay out of blog.
        model = json.loads(compose("config", "--format", "json"))
        application_env = model["services"]["blog"]["environment"]
        for key, value in smtp_environment.items():
            require(application_env.get(key, "").replace("$$", "$") == value,
                    "initial Compose model changed SMTP configuration: " + key)
        volume_init = model["services"]["theme-volume-init"]
        require(volume_init["network_mode"] == "none" and volume_init["read_only"]
                and volume_init["cap_add"] == ["CHOWN"] and not volume_init.get("environment")
                and [mount["target"] for mount in volume_init["volumes"]] == ["/opt/blog/themes"],
                "theme volume initialization must have no network, credentials or other writable volumes")
        require("BLOG_POSTGRES_PASSWORD" not in application_env and "BLOG_OWNER_PASSWORD" not in application_env,
                "database administration secrets must not enter the HTTP service")
        require(application_env.get("DATABASE_URL") is None,
                "fresh installation must not receive an empty or implicit database URL")
        require(application_env.get("RUST_LOG") == "info,sqlx=warn", "application settings must come from .env")
        # `compose config` escapes dollars so its YAML/JSON can be used as Compose input again.
        require(application_env.get("GH_SECRET", "").replace("$$", "$") == special_secret,
                "dotenv changed literal secret characters")
        require(application_env.get("BLOG_LOG_FORMAT") == "json", "Compose must default to JSON logs")
        ports = model["services"]["blog"]["ports"]
        require(any(port["target"] == 9090 and port["host_ip"] == "127.0.0.1" for port in ports),
                "metrics must only publish on loopback")
        stage("fresh installation")
        compose("up", "-d", "--no-build", "--pull", "never")
        guest = client()
        token = wait_for(
            lambda: re.search(r"安装码：([a-f0-9]{64})", compose("logs", "--no-color", "blog")),
            "installation token was not emitted",
        ).group(1)
        guest.request("GET", "/healthz", status=503)
        guest.request("GET", "/readyz", status=503)
        guest.request("GET", "/livez")
        build = guest.json("GET", "/version")
        report["build"] = build
        require(build["version"] and build["revision"], "build information missing")
        expected_revision = os.environ.get("BLOG_EXPECT_REVISION")
        if expected_revision:
            require(build["revision"] == expected_revision, "binary revision differs from its release")
        metrics = client("9090")
        body, _ = metrics.request("GET", "/metrics")
        require(b"blog_installation_complete 0" in body, "installer metrics must not report an active pool")
        metrics.request("GET", "/api/install", status=404)
        guest.request("GET", "/api/install", status=403)
        install_info = guest.json("GET", "/api/install", headers={"X-Install-Token": token})
        require("database_configured" in install_info, "authorized installation info missing")
        installation = {
            "database_url": f"postgres://blog_owner:{owner_password}@db:5432/blog",
            "public_base_url": guest.origin,
            "username": "acceptance-owner", "password": password,
        }
        request = Request(guest.origin + "/api/install", method="POST",
                          data=json.dumps(installation).encode(),
                          headers={"Origin": guest.origin, "Content-Type": "application/json", "X-Install-Token": token})
        try:
            with guest.opener.open(request, timeout=30) as response:
                require(response.status == 200, "installation did not complete")
        except HTTPError as error:
            detail = json.loads(error.read()).get("error", "installation failed")
            for secret in (installation["database_url"], owner_password, password, token):
                detail = detail.replace(secret, "[redacted]")
            raise AcceptanceError(f"installation: HTTP {error.code}: {detail}") from None
        guest.request("GET", "/healthz")
        guest.request("GET", "/readyz")
        guest.request("GET", "/livez")
        guest.request("GET", "/metrics", status=404)
        require(guest.json("GET", "/version") == build, "build identity changed after installation")
        guest.request("GET", "/api/install", status=404)
        require(sql("SELECT rolsuper OR rolcreatedb OR rolcreaterole FROM pg_roles WHERE rolname='blog_owner'") == "f",
                "schema owner must not be a cluster administrator")

        stage("bundled assets, migration paths and persistent content")
        scenario = SiteScenario(guest, Client(guest.origin), sql)
        scenario.assert_installed()
        require(compose("exec", "-T", "blog", "id", "-u") == "10001", "server must run as non-root")
        compose("exec", "-T", "-w", "/tmp", "blog", "blog", "migrate")
        require(compose("exec", "-T", "blog", "stat", "-c", "%a", "/var/lib/blog/config/config.toml") == "600",
                "persisted configuration must be private")
        guest.login(password)
        mail_origin = guest.origin
        stage("SMTP invitation, recovery and revoked credentials")
        report["smtp"] = {}
        with smtp_sink(ops_image, compose("ps", "-q", "blog"), project + "-smtp", root / "scripts", env,
                       smtp_config, report["smtp"], lambda: compose("logs", "--no-color", "blog")) as receive_token:
            report["smtp"].update(scenario.account_emails(receive_token))
        stage("editing drafts, revision history and theme releases")
        scenario.media_and_content()
        scenario.editing_revisions()
        scenario.theme_releases()
        scenario.lifecycles()
        scenario.comments()
        media = scenario.media
        installation_id = sql("SELECT value->>'id' FROM settings WHERE key='installation'")

        stage("telemetry, JSON logs and dependency failure")
        _, headers = guest.request("GET", "/readyz?token=not-a-log-field")
        request_id = headers["x-request-id"]
        body, _ = metrics.request("GET", "/metrics")
        require(b"blog_installation_complete 1" in body and b'route="/api/install"' in body,
                "installation must preserve counters and activate pool metrics")
        require(b'blog_database_pool_connections{state="max"} 9' in body, "configured pool capacity was not applied after installation")
        require(b"blog_http_request_duration_seconds_bucket" in body, "latency histogram missing")
        def readiness_errors(body):
            return sum(float(line.rsplit(b" ", 1)[1]) for line in body.splitlines()
                       if line.startswith(b"blog_http_requests_total{") and b'route="/readyz"' in line
                       and b'status="503"' in line)
        failures_before = readiness_errors(body)
        # A stalled database exercises the readiness deadline, not just a refused socket.
        compose("pause", "db")
        try:
            started = time.monotonic()
            guest.request("GET", "/readyz", status=503)
            require(time.monotonic() - started < 3.5, "readiness did not respect its dependency timeout")
            guest.request("GET", "/livez")
            guest.request("GET", "/healthz", status=503)
            body, _ = metrics.request("GET", "/metrics")
            require(readiness_errors(body) > failures_before, "runtime failures must increment metrics after installation")
        finally:
            compose("unpause", "db")
        wait_for(lambda: guest.request("GET", "/readyz"), "readiness did not recover")
        # Docker's cached health status can lag the HTTP recovery until its next probe.
        # Later maintenance restarts depend on that health status as well.
        wait_for(lambda: healthy("db"), "database healthcheck did not recover")
        logs = compose("logs", "--no-color", "--no-log-prefix", "blog")
        records = [json.loads(line) for line in logs.splitlines() if line.strip()]
        require(any(record.get("fields", {}).get("request_id") == request_id
                    and record.get("fields", {}).get("status") == 200 for record in records),
                "JSON completion log must contain the response request ID and status")
        require(not any(secret in logs for secret in (password, owner_password, "not-a-log-field")),
                "request logs leaked credentials or query parameters")

        stage("persistent task queue, retry, periodic retention and publication")
        def restart_tasks(before_start):
            compose("stop", "blog")
            before_start()
            compose("up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "90", "blog")
            nonlocal guest, metrics
            guest, metrics = client(), client("9090")
            guest.login(password)
            scenario.admin, scenario.guest = guest, Client(guest.origin)
        scenario.tasks(restart_tasks)

        stage("default and optional dedicated retention connections")
        # No extra role or environment variable is needed after installation.
        retention = json.loads(operation("maintenance"))
        require(not retention["dry_run"], "default retention must execute with the saved site connection")
        require(sql("SELECT count(*) FROM pg_roles WHERE rolname IN ('blog_app','blog_maintenance')") == "0",
                "default deployment unexpectedly created additional roles")
        maintenance_password = secrets.token_hex(32)
        sql(f"CREATE ROLE blog_app LOGIN; CREATE ROLE blog_maintenance LOGIN PASSWORD '{maintenance_password}';")
        compose("exec", "-T", "db", "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", "blog",
                "-v", "app_role=blog_app", "-v", "maintenance_role=blog_maintenance",
                data=(PROJECT / "scripts/database-roles.sql").read_text())
        with env_file.open("a") as stream:
            stream.write(dotenv({"BLOG_MAINTENANCE_DATABASE_URL":
                                 f"postgres://blog_maintenance:{maintenance_password}@db:5432/blog"}))
        model = json.loads(compose("--profile", "ops", "config", "--format", "json"))
        require("BLOG_MAINTENANCE_DATABASE_URL" not in model["services"]["blog"]["environment"],
                "maintenance credentials must stay out of the HTTP service")
        maintenance_env = model["services"]["maintenance"]["environment"]
        require(not {"BLOG_OWNER_PASSWORD", "BLOG_POSTGRES_PASSWORD"} & maintenance_env.keys(),
                "retention must not receive database bootstrap passwords")
        require(any(volume.get("target") == "/var/lib/blog/config" and volume.get("read_only")
                    for volume in model["services"]["maintenance"]["volumes"]),
                "retention must read the installed config through a read-only mount")
        retention = json.loads(operation("maintenance"))
        require(not retention["dry_run"], "scheduled retention must execute")
        # Return to the default account mode for the full backup/restore round trip.
        env_file.write_text("\n".join(line for line in env_file.read_text().splitlines()
                                      if not line.startswith("BLOG_MAINTENANCE_DATABASE_URL=")) + "\n")
        guest.request("GET", "/readyz")
        removable = guest.json("POST", API + "/media?filename=purge.png", PNG, status=201,
                               headers={"Content-Type": "image/png"})
        guest.json("DELETE", API + "/media/" + removable["id"],
                   {"expected_version": removable["version"]}, status=204)
        operation("media-plan", "purge.json", removable["id"])
        plan_file = root / "backups/plans/purge.json"
        require(plan_file.stat().st_mode & 0o777 == 0o600, "media plan must be private")
        operation("media-apply", "purge.json", success=False)
        guest.request("GET", removable["url"])
        for attempt in range(2):
            result = json.loads(operation("media-apply", "purge.json",
                                          "--maintenance-confirmed", "--break-links-confirmed"))
            require(result["records_purged"] == 1 and not result["failures"], "media purge did not finish")
            require(result["files_deleted"] == 1 - attempt and result["files_already_absent"] == attempt,
                    "retry must use the original durable receipt")
            guest = client()
            wait_for(lambda: guest.request("GET", "/readyz"), "media purge did not restart the source")
            guest.request("GET", removable["url"], status=404)
        require(sql(f"SELECT count(*) FROM audit_logs WHERE action='media.purge' AND target_id='{removable['id']}'") == "1",
                "media purge retry duplicated the receipt")
        guest.login(password)
        scenario.admin, scenario.guest = guest, Client(guest.origin)
        metrics = client("9090")

        stage("automatic startup migration and persistent content")
        compose("stop", "blog")
        # CI also exercises the root-owned theme volume left by older images.
        compose("run", "--rm", "--no-deps", "--user", "0:0", "--cap-add", "CHOWN", "--entrypoint", "chown",
                "blog", "0:0", "/opt/blog/themes")
        # Recreate the pre-0002 state only in this disposable fixture, with data
        # already present. Starting the owner-backed service must apply 0002.
        sql("DROP INDEX media_trash_idx; DROP INDEX audit_logs_actor_time_idx; "
            "DROP INDEX audit_logs_action_time_idx; DELETE FROM _sqlx_migrations WHERE version=2;")
        compose("down", "--timeout", "30")  # Deliberately retain all three named volumes.
        compose("up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "90")
        guest = client()
        guest.request("GET", "/healthz")
        guest.request("GET", "/livez")
        require(guest.json("GET", "/version") == build, "build identity changed after restart")
        require(compose("exec", "-T", "blog", "stat", "-c", "%u:%g", "/opt/blog/themes") == "10001:10001",
                "startup did not migrate legacy theme volume ownership")
        guest.request("GET", "/api/install", status=404)
        require(sql("SELECT count(*) FROM _sqlx_migrations WHERE version=2 AND success") == "1",
                "startup did not apply the pending migration")
        require(sql("SELECT count(*) FROM pg_indexes WHERE indexname IN "
                    "('media_trash_idx','audit_logs_actor_time_idx','audit_logs_action_time_idx')") == "3",
                "startup migration did not recreate the expected indexes")
        guest.login(password)
        scenario.admin, scenario.guest = guest, Client(guest.origin)
        scenario.assert_public_content()
        scenario.assert_comments()
        scenario.assert_pending_content()
        scenario.assert_theme_recovery()
        body, _ = guest.request("GET", media["url"])
        require(body == PNG, "media object did not survive replacement")
        require(sql("SELECT value->>'id' FROM settings WHERE key='installation'") == installation_id,
                "installation was unexpectedly repeated")
        # Exercise the Docker healthcheck itself, not just a request from the host.
        require(healthy("blog"), "Docker readiness check did not pass")
        stage("complete backup and encrypted repository round trip")
        # A configured application secret is preserved; unrelated backup credentials are not.
        compose("exec", "-T", "blog", "blog", "oauth", "add-github", "--client-id", "recovery-test", "--secret-ref", "GH_SECRET")
        identity_file = root / "test-age-key"
        identity_file.write_text(compose("run", "--rm", "--no-deps", "--entrypoint", "age-keygen", "ops") + "\n")
        identity_file.chmod(0o600)
        public = compose("run", "--rm", "--no-deps", "-v", f"{identity_file}:/test-key:ro",
                         "--entrypoint", "age-keygen", "ops", "-y", "/test-key")
        compose("--profile", "ops", "run", "--rm", "--no-deps", "--entrypoint", "python3", "ops", "-c",
                "import toml; p='/var/lib/blog/config/config.toml'; c=toml.load(p); c['database']['max_lifetime_secs']=777; open(p,'w').write(toml.dumps(c))")
        # The local repository exercises encryption/upload/fetch without external credentials.
        # Production selects S3 through the same restic interface.
        with env_file.open("a") as stream:
            stream.write(f"\nBLOG_BACKUP_RECIPIENT={public}\nRESTIC_REPOSITORY=/backups/test-repository\nRESTIC_PASSWORD={secrets.token_hex(32)}\nBLOG_BACKUP_KEEP=2\nBLOG_BACKUP_REMOTE_KEEP=1\n")
            stream.write("AWS_ACCESS_KEY_ID=unused-backup-access\nAWS_SECRET_ACCESS_KEY=unused-backup-secret\n")
        operation("remote-init")
        operation("backup")
        operation("backup")
        backups = sorted((root / "backups").glob("blog-*.tar.gz.age"))
        require(len(backups) == 2, "two complete backup archives expected")
        archive = backups[-1]
        require(archive.stat().st_mode & 0o777 == 0o600, "backup archive must be private")
        operation("verify", str(archive), str(identity_file))
        require(not (root / "backups/.context").exists(), "plaintext deployment context was left on disk")
        require(not list((root / "backups").glob(".backup-*")), "plaintext backup staging was left on disk")
        compose("run", "--rm", "--no-deps", "-T", "-v", f"{identity_file}:/run/secrets/backup-identity:ro",
                "--entrypoint", "python3", "ops", "-c", """
import json, sys, tomllib
sys.path.insert(0, '/opt/blog/scripts')
from compose_recovery import unpack
expected = json.load(sys.stdin)
with unpack(sys.argv[1]) as (_, _, deployment, _):
    assert not (deployment / 'source.env').exists()
    env = json.loads((deployment / 'environment.json').read_text())
    assert env['GH_SECRET']
    assert all(env.get(key) == value for key, value in expected.items())
    assert not any(k.startswith(('RESTIC_', 'AWS_')) or k in ('DATABASE_URL', 'BLOG_POSTGRES_PASSWORD', 'BLOG_OWNER_PASSWORD') for k in env)
    config = tomllib.loads((deployment / 'config.toml').read_text())
    assert 'url' not in config['database'] and 'maintenance' not in config
""", "/backups/" + archive.name, data=json.dumps(smtp_environment))
        report["smtp_archive_preserved"] = True
        guest = client()
        wait_for(lambda: guest.request("GET", "/readyz"), "backup did not restart source")
        snapshots = json.loads(operation("remote-list"))
        require(len(snapshots) == 1, "remote retention must group changing archive paths by site")
        saved = archive.with_suffix(".saved")
        # On virtualized macOS bind mounts, a host rename may remain stale inside
        # a new container. Remove the local copy from the same filesystem view as fetch.
        compose("run", "--rm", "--no-deps", "--entrypoint", "mv", "ops",
                "/backups/" + archive.name, "/backups/" + saved.name)
        compose("run", "--rm", "--no-deps", "--entrypoint", "mv", "ops",
                "/backups/" + archive.name + ".json", "/backups/" + saved.name + ".json")
        require(not archive.exists(), "backup rename did not remove original local file")
        require(any(path.endswith(archive.name) for path in snapshots[0]["paths"]),
                "remote snapshot does not contain the latest local archive")
        operation("fetch", snapshots[0]["id"])
        require(hashlib.sha256(archive.read_bytes()).digest() == hashlib.sha256(saved.read_bytes()).digest(),
                "encrypted fetch changed backup bytes")
        damaged_dir = root / "backups/damaged"
        damaged_dir.mkdir()
        damaged = damaged_dir / archive.name
        damaged.write_bytes(b"not a backup")
        shutil.copyfile(str(archive) + ".json", str(damaged) + ".json")
        operation("verify", str(damaged), str(identity_file), success=False)

        stage("backup failure restarts the source and preserves previous backups")
        media_path = "/var/lib/blog/media/" + sql(f"SELECT path FROM media WHERE id='{media['id']}'")
        compose("exec", "-T", "blog", "mv", media_path, media_path + ".saved")
        try:
            operation("backup", success=False)
        finally:
            compose("exec", "-T", "blog", "mv", media_path + ".saved", media_path)
        guest = client()  # Docker may reassign port 0 when a stopped container restarts.
        wait_for(lambda: guest.request("GET", "/readyz"), "failed backup left source stopped")
        require(archive.is_file(), "failed backup pruned the previous backup")
        require(json.loads((root / "backups/status.json").read_text())["status"] == "failed",
                "failed backup status missing")
        require((root / "backups/last-successful-backup.json").is_file(), "last successful backup was lost")

        stage("fresh deployment restore, isolation, login/media verification and release")
        operation("restore", str(archive), str(restored), str(identity_file))
        restored_model = json.loads(restored_compose("config", "--format", "json"))
        restored_database = urlsplit(restored_model["services"]["blog"]["environment"]["DATABASE_URL"]).path.lstrip("/")
        for key, value in (("BLOG_DB_MAX_CONNECTIONS", "9"), ("BLOG_DB_STATEMENT_TIMEOUT_MS", "20000")):
            require(restored_model["services"]["blog"]["environment"].get(key) == value,
                    "restored deployment lost database policy: " + key)
        for key in smtp_environment:
            require(restored_model["services"]["blog"]["environment"].get(key) == application_env[key],
                    "restored deployment lost mail configuration: " + key)
        report["smtp_restored_config_preserved"] = True
        require(restored_model["services"]["blog"]["environment"].get("GH_SECRET", "").replace("$$", "$") == special_secret,
                "restore changed a secret containing quotes, dollars, backslashes or newlines")
        require(restored_compose("ps", "--services", "--status", "running") == "db",
                "restore must not start a public application")
        def restored_sql(query):
            return restored_compose("exec", "-T", "db", "psql", "-XAt", "-v", "ON_ERROR_STOP=1",
                                    "-U", "postgres", "-d", restored_database, "-c", query)
        require(restored_sql("SELECT count(*) FROM sessions") == "0", "restored sessions were not revoked")
        require(restored_sql("SELECT value->>'id' FROM settings WHERE key='installation'") == installation_id,
                "restore lost the site identity")
        operation("release", directory=restored, success=False)
        operation("check", "acceptance-owner", "--password-stdin", directory=restored, data="wrong-password\n", success=False)
        checked = json.loads(operation("check", "acceptance-owner", "--password-stdin", directory=restored, data=password + "\n"))
        require(checked["mail_disabled"], "isolation check must verify mail is disabled")
        report["recovery_check"] = checked
        require(restored_sql("SELECT count(*) FROM sessions") == "0", "verification left a reusable session")
        require(restored_sql("SELECT count(*) FROM account_links") == "0", "restored account links were not revoked")
        operation("release", directory=restored)
        restored_compose("exec", "-T", "blog", "test", "!", "-e",
                         "/var/lib/blog/config/recovered/resources/deployment")
        target = Client("http://" + restored_compose("port", "blog", "8080"))
        target.request("GET", "/readyz")
        scenario.admin, scenario.guest = target, Client(target.origin)
        scenario.assert_public_content()
        scenario.assert_comments()
        body, _ = target.request("GET", media["url"])
        require(body == PNG, "restored media differs")
        require(restored_sql("SELECT count(*) FROM sessions") == "0", "release retained verification sessions")
        require(target.json("GET", "/auth/password/recovery")["enabled"], "release lost SMTP configuration")
        target.login(password)
        require(target.json("GET", "/version") == build, "restored binary identity changed")
        scenario.assert_pending_content()
        scenario.assert_theme_recovery()
        scenario.assert_media_references(target.json("GET", API + "/media/" + media["id"]))
        # A restored site must still be able to edit and publish its pending draft.
        pending_path, public_path = scenario.pending_content[0]
        pending = target.json("GET", pending_path)
        target.json("POST", pending_path + "/publish", {"expected_version": pending["version"]})
        require(b"Pending after recovery" in scenario.guest.request("GET", public_path)[0],
                "restored editing draft could not be published")
        require(urlsplit(restored_model["services"]["blog"]["environment"]["DATABASE_URL"]).username == "blog_owner",
                "default recovery changed the site's account mode")
        require(restored_sql("SELECT rolsuper OR rolcreatedb OR rolcreaterole FROM pg_roles WHERE rolname='blog_owner'") == "f",
                "restored site account must not be a cluster administrator")
        require(restored_sql("SELECT count(*) FROM pg_roles WHERE rolname IN ('blog_app','blog_maintenance')") == "0",
                "default recovery created unnecessary accounts")
        require(not json.loads(operation("maintenance", directory=restored))["dry_run"],
                "restored site could not run maintenance with the default connection")
        guest.request("GET", "/readyz")
        require(sql("SELECT value->>'id' FROM settings WHERE key='installation'") == installation_id,
                "restore modified the source deployment")
        restored_compose("--profile", "ops", "run", "--rm", "--no-deps", "--entrypoint", "python3", "ops", "-c",
                         "import toml; assert toml.load('/var/lib/blog/config/config.toml')['database']['max_lifetime_secs'] == 777")
        stage("authenticated SMTP invitation and recovery after release")
        report["restored_smtp"] = {}
        with smtp_sink(ops_image, restored_compose("ps", "-q", "blog"), project + "-restored-smtp",
                       root / "scripts", env, smtp_config, report["restored_smtp"],
                       lambda: restored_compose("logs", "--no-color", "blog")) as receive_token:
            report["restored_smtp"].update(scenario.account_emails(
                receive_token, username="acceptance-restored", email="restored@acceptance.invalid", link_origin=mail_origin))
        report["restored_smtp"]["configured_link_origin_preserved"] = True
        stage("optional restricted accounts survive a second recovery")
        app_password, maintenance_password = secrets.token_hex(32), secrets.token_hex(32)
        restored_sql(f"CREATE ROLE blog_app LOGIN PASSWORD '{app_password}'; "
                     f"CREATE ROLE blog_maintenance LOGIN PASSWORD '{maintenance_password}';")
        restored_compose("exec", "-T", "db", "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", "postgres", "-d", restored_database,
                         "-v", "app_role=blog_app", "-v", "maintenance_role=blog_maintenance",
                         data=(PROJECT / "scripts/database-roles.sql").read_text())
        restricted_url = f"postgres://blog_app:{app_password}@db:5432/{restored_database}"
        restored_env = restored / ".env"
        restored_env.write_text("\n".join(line for line in restored_env.read_text().splitlines()
                                         if not line.startswith("DATABASE_URL=")) + "\n" + dotenv({
            "DATABASE_URL": restricted_url,
            "BLOG_MAINTENANCE_DATABASE_URL": f"postgres://blog_maintenance:{maintenance_password}@db:5432/{restored_database}",
        }))
        restored_compose("run", "--rm", "--no-deps", "--entrypoint", "python3", "ops", "-c",
                         "import os,toml; p='/var/lib/blog/config/config.toml'; c=toml.load(p); "
                         "c['database']['url']=os.environ['DATABASE_URL']; open(p,'w').write(toml.dumps(c))")
        restored_compose("up", "-d", "--no-build", "--pull", "never", "--wait", "--wait-timeout", "90", "blog")
        operation("maintenance", directory=restored)
        operation("backup", directory=restored)
        recovered_archives = list((restored / "backups").glob("blog-*.tar.gz.age"))
        require(len(recovered_archives) == 1, "recovered deployment did not create a complete backup")
        operation("restore", str(recovered_archives[0]), str(restored_again), str(identity_file), directory=restored)
        again_env = (restored_again / ".env").read_text()
        require('DATABASE_URL="postgres://blog_app:' in again_env
                and 'BLOG_MAINTENANCE_DATABASE_URL="postgres://blog_maintenance:' in again_env,
                "separate database accounts were not preserved")
        operation("check", "acceptance-owner", "--password-stdin", directory=restored_again, data=password + "\n")
        report["steps"][-1].update(status="passed", seconds=round(time.monotonic() - last_stage, 3))
        print("Compose installation, SMTP, publishing, themes, persistence, backup and isolated recovery passed.", flush=True)
    finally:
        if (restored_again / ".env").is_file():
            subprocess.run(["docker", "compose", "--project-directory", str(restored_again), "down",
                            "--volumes", "--remove-orphans", "--timeout", "30"],
                           cwd=restored_again, env=env, capture_output=True, text=True, timeout=120, check=True)
        if (restored / ".env").is_file():
            restored_compose("down", "--volumes", "--remove-orphans", "--timeout", "30")
        # The test repository is intentionally local, so restore host ownership for cleanup.
        compose("run", "--rm", "--no-deps", "--entrypoint", "sh", "ops", "-c",
                f"chown -R {os.getuid()}:{os.getgid()} /backups")
        compose("down", "--volumes", "--remove-orphans", "--timeout", "30")
        report["cleanup"] = "passed"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", default="blog:local", help="already-built local image")
    parser.add_argument("--ops-image", default="blog-ops:local", help="matching already-built ops image")
    parser.add_argument("--smtp-security", choices=("tls", "starttls"), default="starttls")
    parser.add_argument("--report", type=Path, help="write a secret-free JSON verification report")
    args = parser.parse_args()
    started = time.monotonic()
    report = {"started_at": dt.datetime.now(dt.timezone.utc).isoformat(), "status": "running", "steps": [], "images": {}}
    report["scripts_sha256"] = {name: hashlib.sha256((PROJECT / "scripts" / name).read_bytes()).hexdigest()
                                for name in ("test_compose.py", "acceptance_support.py", "acceptance_smtp.py",
                                             "acceptance_smtp_tls_support.py", "compose_recovery.py")}
    try:
        for kind, name in (("application", args.image), ("ops", args.ops_image)):
            result = subprocess.run(["docker", "image", "inspect", name], capture_output=True, text=True, timeout=30)
            require(result.returncode == 0, "release image not available: " + kind)
            info = json.loads(result.stdout)[0]
            report["images"][kind] = {"name": name, "id": info["Id"], "architecture": info["Architecture"],
                                      "revision": info["Config"].get("Labels", {}).get("org.opencontainers.image.revision")}
        expected = os.environ.get("BLOG_EXPECT_REVISION")
        if expected:
            require(all(info["revision"] == expected for info in report["images"].values()),
                    "image label differs from expected release")
        with tempfile.TemporaryDirectory(prefix="blog-compose-test-") as directory:
            # Resolve tags once; every subsequent deployment uses immutable image IDs.
            exercise(report["images"]["application"]["id"], Path(directory), report["images"]["ops"]["id"], report, args.smtp_security)
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error) if isinstance(error, AcceptanceError) else "unexpected " + type(error).__name__
        if report["steps"] and report["steps"][-1]["status"] == "running":
            report["steps"][-1]["status"] = "failed"
        print("Compose verification failed: " + report["error"], file=sys.stderr)
        return 1
    finally:
        report["seconds"] = round(time.monotonic() - started, 3)
        if args.report:
            args.report.parent.mkdir(parents=True, exist_ok=True)
            args.report.write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
