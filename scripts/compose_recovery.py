#!/usr/bin/env python3
"""Container-side Compose recovery. Invoke through compose-backup.sh."""
import argparse
from contextlib import contextmanager, redirect_stdout
import datetime as dt
import getpass
import hashlib
from http.cookiejar import CookieJar
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import traceback
from types import SimpleNamespace
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener

import recovery
from recovery_inventory import (RecoveryError, digest, media_inventory,
                                owner_count, schema_snapshot, validate_media, validate_relations)

BACKUPS = Path("/backups")
CONFIG = Path("/var/lib/blog/config")
MEDIA = Path("/var/lib/blog/media")
STATE = CONFIG / "recovered"
ARCHIVE = re.compile(r"blog-\d{8}T\d{6}Z-[a-f0-9]{12}\.tar\.gz")
DATABASE = os.environ.get("BLOG_RESTORE_DATABASE", "blog_restore_site")


def require(condition, message):
    if not condition:
        raise RecoveryError(message)


def now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def private_write(path, value):
    path = Path(path)
    require(not path.is_symlink(), "refusing to write a symbolic link")
    with path.open("w") as stream:
        os.chmod(path, 0o600)
        stream.write(value)


def host_owned(path):
    os.chown(path, int(os.environ.get("BLOG_HOST_UID", "0")), int(os.environ.get("BLOG_HOST_GID", "0")))


def app_owned(path):
    for base, dirs, files in os.walk(path):
        os.chown(base, 10001, 10001)
        os.chmod(base, 0o700)
        for name in files:
            os.chown(Path(base) / name, 10001, 10001)
            os.chmod(Path(base) / name, 0o600)


def quiet_call(function, args):
    with redirect_stdout(io.StringIO()):
        return function(args)


def run(command, *, env=None, data=None, label="command"):
    result = subprocess.run(command, input=data, capture_output=True, env=env, check=False)
    require(result.returncode == 0, f"{label} failed; credentials and subprocess output withheld")
    return result.stdout


def pg(database="postgres"):
    password = quote(os.environ["BLOG_POSTGRES_PASSWORD"], safe="")
    return recovery.PgTools(f"postgres://postgres:{password}@db:5432/{database}")


def sql_input(client, sql, database=None):
    command, env = client.command("psql", ["-X", "-v", "ON_ERROR_STOP=1"], database)
    return run(command, env=env, data=sql.encode(), label="database setup")


def binary_hash():
    return digest(Path("/usr/local/bin/blog"))


def container_environment(entries):
    # Docker preserves key-only entries for explicitly unset Compose variables.
    return dict(item.split("=", 1) for item in entries if "=" in item)


def read_json(path):
    try:
        return json.loads(path.read_text())
    except json.JSONDecodeError as error:
        raise RecoveryError(f"invalid JSON in {path.name}: {error.msg} at byte {error.pos}") from None


def context():
    import tomllib
    model = read_json(BACKUPS / ".context/compose.json")
    actual = read_json(BACKUPS / ".context/container.json")
    environment = container_environment(actual["Config"]["Env"])
    require(actual["Config"].get("Cmd") == ["serve"]
            and actual["Config"].get("Entrypoint") == ["/usr/local/bin/blog"],
            "Compose recovery requires the standard serve command; CLI overrides are unsupported")
    require(environment.get("BLOG_CONFIG_FILE") == str(CONFIG / "config.toml")
            and environment.get("BLOG_MEDIA_DIR") == str(MEDIA),
            "Compose recovery requires the standard config and media mount paths")
    require((BACKUPS / ".context/binary.sha256").read_text().split()[0] == binary_hash(),
            "application and ops images differ; build/load matching images")
    configured = tomllib.loads((CONFIG / "config.toml").read_text())
    url = environment.get("DATABASE_URL") or configured.get("database", {}).get("url", "")
    selected = recovery.db_config(url)
    require(selected["PGHOST"] == "db" and selected["PGPORT"] == "5432",
            "Compose backup only supports this project's db service")
    require(not list(CONFIG.glob("*.install-state.json")), "finish installation before backing up")
    refs = recovery.secret_refs(pg(selected["PGDATABASE"]))
    for ref in refs:
        require(bool(environment.get(ref)), f"application secret reference is missing: {ref}")
        os.environ[ref] = environment[ref]
    return model, actual, environment, selected, configured


def preflight():
    context()
    retention("BLOG_BACKUP_KEEP", 7)
    retention("BLOG_BACKUP_REMOTE_KEEP", 30)
    print("Backup preflight passed.")


def assert_quiet(client):
    require(client.query("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() "
                         "AND pid<>pg_backend_pid() AND backend_type='client backend'") == "0",
            "other database clients are connected; stop external writers/maintenance first")


def backup():
    model, actual, environment, selected, configured = context()
    client = pg(selected["PGDATABASE"])
    assert_quiet(client)
    require(not client.query("SELECT COALESCE(shobj_description(oid,'pg_database'),'') "
                             "FROM pg_database WHERE datname=current_database()").startswith(recovery.ISOLATION_PREFIX),
            "release the isolated restore before creating a new backup")
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = f"blog-{stamp}-{secrets.token_hex(6)}.tar.gz"
    with tempfile.TemporaryDirectory(prefix=".backup-", dir=BACKUPS) as temporary:
        work = Path(temporary)
        deployment = work / "deployment"
        deployment.mkdir()
        # Include effective container settings as well as the operator's source .env.
        # Shell overrides and custom OAuth secret names therefore survive host loss.
        private_write(deployment / "environment.json", json.dumps(environment))
        shutil.copyfile("/deployment/.env", deployment / "source.env")
        shutil.copyfile(CONFIG / "config.toml", deployment / "config.toml")
        site_id = client.query("SELECT value->>'id' FROM settings WHERE key='installation'")
        require(bool(re.fullmatch(r"[a-f0-9]{64}", site_id)), "installation identity is missing")
        metadata = {"format": 1, "binary_sha256": binary_hash(), "image_id": actual["Image"],
                    "image": model["services"]["blog"]["image"],
                    "ops_image": model["services"]["ops"]["image"], "site_id": site_id,
                    "project": model["name"], "created_at": now()}
        private_write(deployment / "compose.json", json.dumps(metadata, indent=2))
        os.environ["DATABASE_URL"] = "postgres://postgres:" + quote(os.environ["BLOG_POSTGRES_PASSWORD"], safe="") + "@db:5432/" + selected["PGDATABASE"]
        output = work / "backup"
        quiet_call(recovery.backup, SimpleNamespace(
            output=str(output), docker_container=None, maintenance_confirmed=True,
            theme_dir=environment.get("BLOG_THEME_DIR", "/opt/blog/themes/default"),
            media_dir=str(MEDIA), resource=[f"deployment={deployment}"]))
        assert_quiet(client)
        recovery.verify(output)
        partial = BACKUPS / (name + ".partial")
        try:
            with tarfile.open(partial, "w:gz") as archive:
                archive.add(output, arcname="backup")
            os.chmod(partial, 0o600)
            host_owned(partial)
            with partial.open("rb") as stream:
                os.fsync(stream.fileno())
            partial.rename(BACKUPS / name)
        finally:
            partial.unlink(missing_ok=True)
    print(name)


def safe_extract(archive, destination):
    """Reject traversal, links, special files and duplicate entries before writing anything."""
    seen = set()
    with tarfile.open(archive, "r:gz") as source:
        members = source.getmembers()
        for member in members:
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and path.parts and path.parts[0] == "backup"
                    and all(part not in ("", ".", "..") for part in member.name.split("/"))
                    and "\\" not in member.name and member.name not in seen
                    and (member.isdir() or member.isfile()), "unsafe backup archive entry")
            seen.add(member.name)
        for member in members:
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True, mode=0o700)
            else:
                target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                with source.extractfile(member) as src, target.open("xb") as dest:
                    shutil.copyfileobj(src, dest)
                os.chmod(target, 0o600)


@contextmanager
def unpack(path):
    require(Path(path).is_file() and not Path(path).is_symlink(), "backup must be a regular file")
    with tempfile.TemporaryDirectory(prefix=".verify-", dir=BACKUPS) as temporary:
        safe_extract(path, Path(temporary))
        root = Path(temporary) / "backup"
        manifest = recovery.verify(root)
        deployment = root / "data/resources/deployment"
        metadata = json.loads((deployment / "compose.json").read_text())
        require(metadata.get("format") == 1 and metadata["binary_sha256"] == binary_hash(),
                "backup requires its matching application and ops images")
        yield root, manifest, deployment, metadata


def dotenv(values):
    # Double quotes encode backslashes/newlines; $$ prevents Compose interpolation.
    # Single quotes cannot represent every backslash + quote combination reliably.
    lines = []
    for key, value in values.items():
        require(bool(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key)), "invalid environment key")
        value = str(value)
        require("\x00" not in value and "\r" not in value, "unsupported environment value")
        lines.append(key + "=" + json.dumps(value.replace("$", "$$"), ensure_ascii=False))
    return "\n".join(lines) + "\n"


def prepare(path):
    with unpack(path) as (_, manifest, deployment, metadata):
        values = json.loads((deployment / "environment.json").read_text())
        # Keep only settings explicitly wired into the application; image defaults
        # such as PATH/BIND are not deployment overrides.
        values = {key: value for key, value in values.items()
                  if key in manifest["secret_refs"] or key in (
                      "BLOG_PUBLIC_BASE_URL", "BLOG_TRUSTED_PROXIES", "BLOG_SECURE_COOKIES",
                      "BLOG_TIME_ZONE", "TZ",
                      "BLOG_LOG_FORMAT", "RUST_LOG", "IDP_SECRET", "GH_SECRET",
                      "BLOG_DB_MAX_CONNECTIONS",
                      "BLOG_DB_MIN_CONNECTIONS",
                      "BLOG_DB_ACQUIRE_TIMEOUT_MS",
                      "BLOG_DB_IDLE_TIMEOUT_SECS",
                      "BLOG_DB_MAX_LIFETIME_SECS",
                      "BLOG_DB_STATEMENT_TIMEOUT_MS",
                      "BLOG_DB_LOCK_TIMEOUT_MS",
                      "BLOG_DB_IDLE_IN_TRANSACTION_TIMEOUT_MS",
                      "BLOG_DB_CONNECT_RETRIES",
                      "BLOG_DB_CONNECT_RETRY_BACKOFF_MS",
                  )}
        app_password, maintenance_password = secrets.token_hex(32), secrets.token_hex(32)
        database = "blog_restore_" + secrets.token_hex(6)
        values.update({"BLOG_IMAGE": metadata["image_id"], "BLOG_OPS_IMAGE": metadata["ops_image"],
                       "COMPOSE_PROJECT_NAME": "blog-restore-" + secrets.token_hex(6),
                       "BLOG_POSTGRES_PASSWORD": secrets.token_hex(32),
                       "BLOG_OWNER_PASSWORD": secrets.token_hex(32),
                       "BLOG_APP_PASSWORD": app_password, "BLOG_MAINTENANCE_PASSWORD": maintenance_password,
                       "BLOG_RESTORE_DATABASE": database,
                       "DATABASE_URL": f"postgres://blog_app:{app_password}@db:5432/{database}",
                       "BLOG_MAINTENANCE_DATABASE_URL": f"postgres://blog_maintenance:{maintenance_password}@db:5432/{database}",
                       "BLOG_THEME_DIR": str(STATE / "resources/installed-themes/default"),
                       "BLOG_HTTP_HOST": "127.0.0.1", "BLOG_HTTP_PORT": "0", "BLOG_METRICS_PORT": "0"})
        target = Path("/target")
        require(not (target / ".env").exists(), "restore deployment already initialized")
        private_write(target / ".env", dotenv(values))
        private_write(target / ".restore.json", json.dumps({"backup_id": manifest["backup_id"], **metadata}))
        host_owned(target / ".env")
        host_owned(target / ".restore.json")


def restore(path):
    import tomllib
    require(not any(CONFIG.iterdir()) and not any(MEDIA.iterdir()), "restore requires empty config and media volumes")
    with unpack(path) as (root, manifest, deployment, _):
        client = pg()
        os.environ["DATABASE_URL"] = "postgres://postgres:" + quote(os.environ["BLOG_POSTGRES_PASSWORD"], safe="") + "@db:5432/postgres"
        quiet_call(recovery.restore, SimpleNamespace(
            backup=str(root), target_db=DATABASE, target_owner="blog_owner", output=str(STATE),
            isolation_confirmed=True, docker_container=None))
        for role, variable in (("blog_app", "BLOG_APP_PASSWORD"), ("blog_maintenance", "BLOG_MAINTENANCE_PASSWORD")):
            password = os.environ[variable]
            require(bool(re.fullmatch(r"[a-f0-9]{64}", password)), "restore role password must be generated")
            sql_input(client, f"CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '{password}';")
        command, env = client.command("psql", ["-X", "-v", "ON_ERROR_STOP=1", "-v", "app_role=blog_app",
                                               "-v", "maintenance_role=blog_maintenance",
                                               "-f", "/opt/blog/scripts/database-roles.sql"], DATABASE)
        run(command, env=env, label="restored database grants")
        shutil.copytree(STATE / "resources/media", MEDIA, dirs_exist_ok=True)
        # The HTTP process may read its config volume. Never leave the source's
        # cluster/admin secrets there; the original private archive retains them.
        shutil.rmtree(STATE / "resources/deployment")
        shutil.rmtree(STATE / "resources/media")
        # Use fresh DB credentials; preserve all other recognized deployment settings.
        import toml
        config = tomllib.loads((deployment / "config.toml").read_text())
        config.setdefault("database", {}).update({"url": f"postgres://blog_app:{os.environ['BLOG_APP_PASSWORD']}@db:5432/{DATABASE}",
                              "migrations_dir": "/opt/blog/migrations/postgres"})
        config.pop("maintenance", None)
        config["recovery"] = {"enabled": False}
        config.setdefault("paths", {})["theme_dir"] = str(STATE / "resources/installed-themes/default")
        # A custom default directory can have another name; the canonical copy is always present.
        if not (STATE / "resources/installed-themes/default/theme.json").is_file():
            shutil.copytree(STATE / "theme", STATE / "resources/installed-themes/default")
        private_write(CONFIG / "config.toml", toml.dumps(config))
        app_owned(CONFIG)
        app_owned(MEDIA)
        print(json.dumps({"backup_id": manifest["backup_id"], "database": DATABASE, "isolated": True}))


def check(username, password_stdin=False):
    require((STATE / "ISOLATED").is_file() and not (STATE / "FAILED").exists(), "no successful isolated restore")
    (STATE / "VERIFIED").unlink(missing_ok=True)
    password = sys.stdin.readline().rstrip("\r\n") if password_stdin else getpass.getpass("管理员密码: ")
    require(bool(password), "administrator password is empty")
    client = pg(DATABASE)
    result = json.loads((STATE / "RESTORED").read_text())
    require(result["target_database"] == DATABASE, "restore database configuration changed")
    require(schema_snapshot(client) == result["schema"] and owner_count(client) > 0, "restored schema/Owner differs")
    media = media_inventory(client)
    validate_media(MEDIA, media)
    validate_relations(client)
    # Supply only application settings to the non-root verification process.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(("PG", "RESTIC_", "AWS_")) and key not in (
               "BLOG_POSTGRES_PASSWORD", "BLOG_OWNER_PASSWORD", "BLOG_APP_PASSWORD", "BLOG_MAINTENANCE_PASSWORD",
               "BLOG_MAINTENANCE_DATABASE_URL")}
    env.update({"DATABASE_URL": f"postgres://blog_app:{os.environ['BLOG_APP_PASSWORD']}@db:5432/{DATABASE}",
                "BLOG_CONFIG_FILE": str(CONFIG / "config.toml"), "BLOG_MEDIA_DIR": str(MEDIA),
                "BLOG_THEME_DIR": str(STATE / "resources/installed-themes/default"),
                "BLOG_ADMIN_DIST": "/opt/blog/admin", "BLOG_MIGRATIONS_DIR": "/opt/blog/migrations/postgres",
                "BLOG_BIND": "127.0.0.1:18080", "BLOG_PUBLIC_BASE_URL": "http://127.0.0.1:18080",
                "BLOG_SECURE_COOKIES": "false", "BLOG_TRUSTED_PROXIES": "", "BLOG_RECOVERY_MODE": "true",
                "BLOG_METRICS_BIND": "127.0.0.1:19090"})
    origin = "http://127.0.0.1:18080"
    opener = build_opener(ProxyHandler({}), HTTPCookieProcessor(CookieJar()))

    def request(path, body=None):
        req = Request(origin + path, data=json.dumps(body).encode() if body is not None else None,
                      headers={"Origin": origin, "Content-Type": "application/json"})
        with opener.open(req, timeout=5) as response:
            require(response.status == 200, "HTTP verification failed")
            return response.read()

    process = subprocess.Popen(["blog", "serve"], env=env, user=10001, group=10001, extra_groups=[],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        deadline = time.monotonic() + 45
        while True:
            require(process.poll() is None, "isolated application failed to start")
            try:
                request("/readyz")
                break
            except (URLError, TimeoutError):
                require(time.monotonic() < deadline, "isolated application startup timed out")
                time.sleep(0.25)
        request("/auth/login/password", {"username": username, "password": password})
        me = json.loads(request("/api/admin/v1/me"))
        require(bool(me.get("csrf_token")) and "settings.manage" in me.get("permissions", []),
                "login must use an administrator with settings.manage")
        request("/admin/")
        request("/")
        pages = client.query("SELECT slug FROM pages WHERE status='published' AND deleted_at IS NULL ORDER BY id LIMIT 10").splitlines()
        for slug in pages:
            request("/" + quote(slug, safe=""))
        checked_media = 0
        for item in media:
            if item["deleted_at"] is None:
                require(hashlib.sha256(request("/media/" + item["id"])).hexdigest() == item["sha256"],
                        "HTTP media checksum differs")
                checked_media += 1
        verification = {"verified_at": now(), "isolation_tag": result["isolation_tag"],
                        "pages": len(pages), "media": checked_media}
    except (HTTPError, URLError, TimeoutError):
        raise RecoveryError("isolated HTTP/login verification failed") from None
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        client.query("DELETE FROM sessions")
    # Publish success only after the process is stopped and verification sessions
    # are revoked. A failed cleanup must never authorize a later release.
    private_write(STATE / "VERIFIED", json.dumps(verification))
    print(json.dumps({"login": True, "pages": verification["pages"],
                      "media": verification["media"], "isolated": True}))


def release():
    result = json.loads((STATE / "RESTORED").read_text())
    require(result["target_database"] == DATABASE, "restore database configuration changed")
    verified = json.loads((STATE / "VERIFIED").read_text())
    require(verified["isolation_tag"] == result["isolation_tag"], "verification belongs to another restore")
    assert_quiet(pg(DATABASE))
    os.environ["DATABASE_URL"] = "postgres://postgres:" + quote(os.environ["BLOG_POSTGRES_PASSWORD"], safe="") + "@db:5432/postgres"
    quiet_call(recovery.release, SimpleNamespace(output=str(STATE), media_dir=str(MEDIA),
                                                verification_confirmed=True, docker_container=None))
    print("Restore released; verification sessions revoked.")


def media_cleanup(action, name, ids, maintenance_confirmed, break_links_confirmed):
    """Deployment wrapper only; the Rust application owns plan, commit and file semantics."""
    import tomllib
    require(bool(name and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}\.json", name)),
            "use a simple plan filename ending in .json")
    configured = tomllib.loads((CONFIG / "config.toml").read_text())
    selected = recovery.db_config(os.environ.get("DATABASE_URL") or configured["database"]["url"])
    require(selected["PGHOST"] == "db" and selected["PGPORT"] == "5432", "media cleanup requires the Compose database")
    plans = BACKUPS / "plans"
    require(not plans.is_symlink(), "plan directory must not be a symbolic link")
    plans.mkdir(mode=0o700, exist_ok=True)
    path = plans / name
    require(not path.is_symlink(), "plan must not be a symbolic link")
    env = {"PATH": os.environ["PATH"], "BLOG_CONFIG_FILE": str(CONFIG / "config.toml"),
           "BLOG_MEDIA_DIR": str(MEDIA), "BLOG_MIGRATIONS_DIR": "/opt/blog/migrations/postgres",
           "BLOG_RECOVERY_MODE": os.environ.get("BLOG_RECOVERY_MODE", "false"),
           "DATABASE_URL": "postgres://blog_owner:" + quote(os.environ["BLOG_OWNER_PASSWORD"], safe="")
                           + "@db:5432/" + selected["PGDATABASE"]}
    command = ["blog", "media", "purge"]
    if action == "media-plan":
        require(bool(ids), "select media IDs explicitly")
        command += ["plan", "--output", str(path)]
        for mid in ids:
            command += ["--id", mid]
    else:
        require(not ids and maintenance_confirmed and break_links_confirmed, "confirm maintenance and permanent link removal")
        assert_quiet(pg(selected["PGDATABASE"]))
        command += ["apply", str(path), "--maintenance-confirmed", "--break-links-confirmed"]
    result = subprocess.run(command, env=env, capture_output=True, text=True, check=False)
    # stdout is the typed CLI result, stderr can contain configuration/storage details.
    if result.stdout.strip():
        print(result.stdout.strip())
    if path.is_file():
        host_owned(path)
    host_owned(plans)
    require(result.returncode == 0, "media cleanup failed; check the reviewed plan, references, object files and database permissions")


def retention(key, default):
    value = int(os.environ.get(key, str(default)))
    require(1 <= value <= 10000, f"{key} must be between 1 and 10000")
    return value


def restic(args):
    require(bool(os.environ.get("RESTIC_REPOSITORY")) and bool(os.environ.get("RESTIC_PASSWORD")),
            "configure RESTIC_REPOSITORY and RESTIC_PASSWORD in .env")
    return run(["restic", "--no-cache", *args], label="encrypted remote backup")


def sync(path):
    with unpack(path) as (_, _, _, metadata):
        tag = "blog-site:" + metadata["site_id"]
        restic(["backup", "--host", "blog-compose", "--tag", tag, str(path)])
        restic(["forget", "--host", "blog-compose", "--tag", tag, "--group-by", "host,tags", "--keep-last",
                str(retention("BLOG_BACKUP_REMOTE_KEEP", 30)), "--prune"])
    print("Encrypted remote copy saved.")


def finalize(name):
    require(bool(ARCHIVE.fullmatch(name)), "invalid backup name")
    if os.environ.get("RESTIC_REPOSITORY"):
        sync(BACKUPS / name)
    # Prune only our completed archive names, and only after remote success (if configured).
    keep = retention("BLOG_BACKUP_KEEP", 7)
    archives = sorted((p for p in BACKUPS.iterdir() if ARCHIVE.fullmatch(p.name)
                       and p.is_file() and not p.is_symlink()), key=lambda p: p.name, reverse=True)
    for path in archives[keep:]:
        if path.name != name:
            path.unlink()
    print(json.dumps({"archive": name, "remote": bool(os.environ.get("RESTIC_REPOSITORY")), "keep": keep}))


def fetch(snapshot):
    require(bool(re.fullmatch(r"[a-f0-9]{8,64}", snapshot)), "use an explicit restic snapshot ID")
    with tempfile.TemporaryDirectory(prefix=".fetch-", dir=BACKUPS) as temporary:
        restic(["restore", snapshot, "--target", temporary])
        files = [p for p in Path(temporary).rglob("*.tar.gz") if ARCHIVE.fullmatch(p.name)]
        require(len(files) == 1, "snapshot must contain exactly one blog backup archive")
        with unpack(files[0]):
            pass
        target = BACKUPS / files[0].name
        require(not target.exists(), "local backup already exists: " + target.name)
        files[0].rename(target)
        os.chmod(target, 0o600)
        host_owned(target)
        print(target.name)


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("preflight", "backup", "verify", "prepare", "restore", "check",
                                           "release", "finalize", "sync", "remote-init", "remote-list", "fetch", "media-plan", "media-apply"))
    parser.add_argument("argument", nargs="?")
    parser.add_argument("ids", nargs="*")
    parser.add_argument("--password-stdin", action="store_true")
    parser.add_argument("--maintenance-confirmed", action="store_true")
    parser.add_argument("--break-links-confirmed", action="store_true")
    args = parser.parse_args()
    try:
        if args.action in ("media-plan", "media-apply"):
            media_cleanup(args.action, args.argument, args.ids, args.maintenance_confirmed, args.break_links_confirmed)
        elif args.ids or args.maintenance_confirmed or args.break_links_confirmed:
            raise RecoveryError("unexpected media cleanup arguments")
        elif args.action == "preflight": preflight()
        elif args.action == "backup": backup()
        elif args.action == "verify":
            with unpack("/input/backup.tar.gz") as (_, manifest, _, _):
                print(json.dumps({"backup_id": manifest["backup_id"], "files": len(manifest["files"])}))
        elif args.action == "prepare": prepare("/input/backup.tar.gz")
        elif args.action == "restore": restore("/input/backup.tar.gz")
        elif args.action == "check": check(args.argument, args.password_stdin)
        elif args.action == "release": release()
        elif args.action == "finalize": finalize(args.argument)
        elif args.action == "sync":
            require(bool(ARCHIVE.fullmatch(args.argument)), "use a local backup filename from backups/")
            sync(BACKUPS / args.argument)
        elif args.action == "remote-init": restic(["init"]); print("Encrypted repository initialized.")
        elif args.action == "remote-list": print(restic(["snapshots", "--json"]).decode())
        elif args.action == "fetch": fetch(args.argument)
    except RecoveryError as error:
        print(f"Compose {args.action}: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, tarfile.TarError) as error:
        # These operations handle complete deployment secrets. Never echo arbitrary
        # subprocess, TOML, archive or URL error text into scheduler logs.
        frame = traceback.extract_tb(error.__traceback__)[-1]
        print(f"Compose {args.action} failed ({type(error).__name__} at {frame.name}:{frame.lineno}); "
              "check matching images, credentials, archive and isolation prerequisites.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
