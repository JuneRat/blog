#!/usr/bin/env python3
"""Maintenance-window backup and isolated restore for the current single-site blog.

The operator stops every writer before backup. This tool cannot prove that an external
server or worker has stopped, so it requires an explicit maintenance assertion.
Restore only creates a fresh blog_restore_* database and never starts a server.
"""

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import uuid
from urllib.parse import unquote, urlsplit
from deployment_config import resource_paths

from recovery_inventory import (
    RecoveryError, SCHEMA_ID, SCHEMA_TABLES, ISOLATION_PREFIX, digest, safe_file,
    expected_migrations, schema_snapshot, database_counts, media_inventory,
    validate_media, validate_relations, owner_count,
)

FORMAT = 2
SAFE_NAME = re.compile(r"^[A-Za-z][A-Za-z0-9_]{0,62}$")
THEME_SLUG = re.compile(r"^[a-z0-9-]+$")
RESTORE_NAME = re.compile(r"^blog_restore_[A-Za-z0-9_]{1,48}$")


def is_pg_archive(path):
    with open(path, "rb") as stream:
        return stream.read(5) == b"PGDMP"


def db_config(url):
    parsed = urlsplit(url)
    if parsed.scheme not in ("postgres", "postgresql") or not parsed.hostname:
        raise RecoveryError("DATABASE_URL must be a PostgreSQL URL")
    database = unquote(parsed.path.lstrip("/"))
    if not SAFE_NAME.fullmatch(database):
        raise RecoveryError("database name must be a simple PostgreSQL identifier")
    return {
        "PGHOST": parsed.hostname,
        "PGPORT": str(parsed.port or 5432),
        "PGUSER": unquote(parsed.username or ""),
        "PGPASSWORD": unquote(parsed.password or ""),
        "PGDATABASE": database,
    }


class PgTools:
    def __init__(self, url, container=None):
        self.config = db_config(url)
        self.container = container
        if container and not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", container):
            raise RecoveryError("invalid Docker container name")

    def command(self, tool, args, database=None):
        selected = dict(self.config)
        if database:
            selected["PGDATABASE"] = database
        if self.container:
            # Docker mode runs the tools beside the DB. Host and port from the URL
            # are intentionally ignored; the named container is the chosen instance.
            return ["docker", "exec", "-i", self.container, "env",
                    f"PGUSER={selected['PGUSER']}", f"PGDATABASE={selected['PGDATABASE']}",
                    tool, *args], os.environ.copy()
        env = os.environ.copy()
        env.update(selected)
        return [tool, *args], env

    def run(self, tool, args, database=None, input_path=None, output_path=None):
        cmd, env = self.command(tool, args, database)
        with (open(input_path, "rb") if input_path else open(os.devnull, "rb")) as source:
            with (open(output_path, "wb") if output_path else open(os.devnull, "wb")) as sink:
                result = subprocess.run(cmd, stdin=source, stdout=sink, stderr=subprocess.PIPE,
                                        env=env, check=False)
        if result.returncode:
            raise RecoveryError(f"{tool} failed: {result.stderr.decode(errors='replace').strip()}")

    def query(self, sql, database=None):
        cmd, env = self.command("psql", ["-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql], database)
        result = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env,
                                check=False)
        if result.returncode:
            raise RecoveryError(f"psql failed: {result.stderr.decode(errors='replace').strip()}")
        return result.stdout.decode().strip()


def copy_resource(source, destination):
    if Path(source).is_symlink():
        raise RecoveryError("resource root must not be a symbolic link")
    source = Path(source).resolve(strict=True)
    if not source.is_dir():
        raise RecoveryError(f"resource is not a directory: {source}")
    destination.mkdir(parents=True)
    for root, dirs, files in os.walk(source, followlinks=False):
        base = Path(root)
        for name in dirs + files:
            path = base / name
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode) or not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
                raise RecoveryError(f"resource contains an unsupported entry: {path}")
        relative = base.relative_to(source)
        (destination / relative).mkdir(parents=True, exist_ok=True)
        for name in files:
            shutil.copy2(base / name, destination / relative / name, follow_symlinks=False)


def file_records(root):
    records = []
    if root.is_symlink():
        raise RecoveryError("resource root must not be a symbolic link")
    for base, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            path = Path(base) / name
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode) or not (stat.S_ISDIR(mode) or stat.S_ISREG(mode)):
                raise RecoveryError(f"resource contains an unsupported entry: {path}")
        for name in files:
            path = Path(base) / name
            records.append({"path": path.relative_to(root).as_posix(), "size": path.stat().st_size,
                            "sha256": digest(path)})
    records.sort(key=lambda record: record["path"])
    return records


def assert_secret_refs(refs):
    missing = [ref for ref in refs if not ref or ref not in os.environ or not os.environ[ref]]
    if missing:
        raise RecoveryError("secret recovery environment is missing references: " + ", ".join(missing))


def secret_refs(pg, database=None):
    refs = json.loads(pg.query("SELECT COALESCE(jsonb_agg(p->>'secret_ref'),'[]') FROM settings, "
                              "jsonb_array_elements(value->'providers') AS p "
                              "WHERE key='oauth'", database))
    if any(not isinstance(ref, str) or not ref for ref in refs):
        raise RecoveryError("OAuth provider has a missing secret reference")
    return sorted(set(refs))


def backup(args):
    if not args.maintenance_confirmed:
        raise RecoveryError("stop all writers and pass --maintenance-confirmed")
    output = Path(args.output).absolute()
    if output.exists():
        raise RecoveryError("backup destination already exists")
    pg = PgTools(os.environ.get("DATABASE_URL", ""), args.docker_container)
    stage = output.with_name(output.name + ".partial-" + uuid.uuid4().hex[:8])
    stage.mkdir(mode=0o700, parents=True)
    try:
        schema = schema_snapshot(pg)
        media = media_inventory(pg)
        validate_relations(pg)
        if owner_count(pg) < 1:
            raise RecoveryError("no active loginable Owner in backup source")
        data = stage / "data"
        data.mkdir()
        pg.run("pg_dump", ["--format=custom", "--no-owner", "--no-acl"],
               output_path=data / "database.dump")
        if not is_pg_archive(data / "database.dump"):
            raise RecoveryError("pg_dump did not produce a PostgreSQL custom-format archive")
        pg.run("pg_restore", ["--list"], input_path=data / "database.dump")
        copy_resource(args.theme_dir, data / "theme")
        if not (data / "theme" / "theme.json").is_file():
            raise RecoveryError("theme manifest is missing")
        for template in ("base.html", "index.html", "post.html", "page.html"):
            if not (data / "theme" / "templates" / template).is_file():
                raise RecoveryError(f"theme is missing required template: {template}")
        # The admin can select any validated sibling theme. Preserve the complete installed set,
        # otherwise a database restored with a non-default active slug could not render.
        installed = data / "resources" / "installed-themes"
        installed.mkdir(parents=True)
        for candidate in Path(args.theme_dir).parent.iterdir():
            if not candidate.is_symlink() and candidate.is_dir() and (candidate / "theme.json").is_file():
                copy_resource(candidate, installed / candidate.name)
        active_slug = pg.query("SELECT COALESCE((SELECT value->>'slug' FROM settings WHERE key='theme'), '')")
        if active_slug and not THEME_SLUG.fullmatch(active_slug):
            raise RecoveryError("active database theme slug is unsafe")
        if active_slug and not (data / "resources" / "installed-themes" / active_slug / "theme.json").is_file():
            raise RecoveryError("active database theme is not installed in the backup")
        media_root = Path(args.media_dir)
        destination = data / "resources" / "media"
        if media_root.exists():
            copy_resource(media_root, destination)
        elif not media:
            destination.mkdir(parents=True)
        else:
            raise RecoveryError("media directory is missing")
        validate_media(destination, media)
        for spec in args.resource:
            if "=" not in spec:
                raise RecoveryError("--resource must be name=directory")
            name, source = spec.split("=", 1)
            if not SAFE_NAME.fullmatch(name) or name in ("theme", "database", "installed-themes", "media"):
                raise RecoveryError("invalid or duplicate resource name")
            if (data / "resources" / name).exists():
                raise RecoveryError("duplicate resource name")
            copy_resource(source, data / "resources" / name)
        refs = secret_refs(pg)
        assert_secret_refs(refs)
        counts = database_counts(pg)
        commit = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True,
                                check=False).stdout.strip() or "unknown"
        manifest = {
            "format": FORMAT, "backup_id": str(uuid.uuid4()),
            "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "source_database": pg.config["PGDATABASE"], "schema": schema, "media": media,
            "git_commit": commit, "secret_refs": refs, "database_counts": counts,
            "files": file_records(data),
        }
        raw = (json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode()
        (stage / "manifest.json").write_bytes(raw)
        (stage / "COMPLETE").write_text(hashlib.sha256(raw).hexdigest() + "\n")
        stage.rename(output)
        print(json.dumps({"backup": str(output), "backup_id": manifest["backup_id"],
                          "files": len(manifest["files"]), "bytes": sum(x["size"] for x in manifest["files"])},
                         ensure_ascii=False))
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def verify(path):
    root = Path(path).absolute()
    marker = root / "COMPLETE"
    manifest_path = root / "manifest.json"
    if not marker.is_file() or not manifest_path.is_file():
        raise RecoveryError("backup is incomplete")
    raw = manifest_path.read_bytes()
    if marker.read_text().strip() != hashlib.sha256(raw).hexdigest():
        raise RecoveryError("manifest checksum mismatch")
    manifest = json.loads(raw)
    if manifest.get("format") != FORMAT:
        raise RecoveryError("unsupported backup format; use the matching old recovery tool for old backups")
    schema = manifest.get("schema", {})
    if schema.get("id") != SCHEMA_ID or schema.get("migrations") != expected_migrations():
        raise RecoveryError("backup schema/checksums do not match this application revision")
    records = manifest.get("files", [])
    if not records or not any(item.get("path") == "database.dump" for item in records):
        raise RecoveryError("database dump missing from manifest")
    for template in ("base.html", "index.html", "post.html", "page.html"):
        if not any(item.get("path") == f"theme/templates/{template}" for item in records):
            raise RecoveryError(f"theme template is missing from manifest: {template}")
    if not any(item.get("path") == "theme/theme.json" for item in records):
        raise RecoveryError("theme manifest is missing from backup")
    if not is_pg_archive(safe_file(root / "data", "database.dump")):
        raise RecoveryError("database dump is not a PostgreSQL custom-format archive")
    seen = set()
    for item in records:
        rel = Path(item["path"])
        if rel.is_absolute() or ".." in rel.parts or rel.as_posix() in seen:
            raise RecoveryError("unsafe or duplicate manifest path")
        seen.add(rel.as_posix())
        target = safe_file(root / "data", item["path"])
        if not target.is_file() or target.is_symlink() or target.stat().st_size != item["size"] or digest(target) != item["sha256"]:
            raise RecoveryError(f"backup file failed verification: {rel}")
    actual = {item["path"] for item in file_records(root / "data")}
    if actual != seen:
        raise RecoveryError("backup contains unlisted files")
    validate_media(root / "data" / "resources" / "media", manifest["media"])
    return manifest


def restore(args):
    manifest = verify(args.backup)
    if not RESTORE_NAME.fullmatch(args.target_db):
        raise RecoveryError("target database must be a fresh blog_restore_* name")
    if args.target_db == manifest["source_database"]:
        raise RecoveryError("target cannot be the source database")
    if not args.isolation_confirmed:
        raise RecoveryError("isolate the target from public traffic and pass --isolation-confirmed")
    assert_secret_refs(manifest["secret_refs"])
    pg = PgTools(os.environ.get("DATABASE_URL", ""), args.docker_container)
    if pg.config["PGDATABASE"] == args.target_db:
        raise RecoveryError("DATABASE_URL must refer to an existing administrative database")
    target = Path(args.output).absolute()
    if target.exists():
        raise RecoveryError("restore output already exists")
    exists = pg.query("SELECT 1 FROM pg_database WHERE datname = '" + args.target_db + "'", database="postgres")
    if exists:
        raise RecoveryError("target database already exists")
    target.mkdir(mode=0o700, parents=True)
    tag = ISOLATION_PREFIX + uuid.uuid4().hex
    (target / "ISOLATED").write_text(tag + "\n")
    try:
        pg.run("createdb", ["--template=template0", args.target_db], database="postgres")
        pg.query(f"COMMENT ON DATABASE \"{args.target_db}\" IS '{tag}'", database=args.target_db)
        pg.run("pg_restore", ["--exit-on-error", "--no-owner", "--no-acl", "--dbname=" + args.target_db],
               input_path=Path(args.backup) / "data" / "database.dump")
        if pg.query("SELECT shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()", args.target_db) != tag:
            raise RecoveryError("restored database isolation guard was lost")
        # 会话是运行态：恢复后一律作废，防止备份回退让旧 Cookie 重新有效（ADR-0010）。
        # 失败限流与 OAuth 尝试本就在进程内存，不随备份回来。
        pg.query("DELETE FROM sessions", database=args.target_db)
        source_data = Path(args.backup) / "data"
        shutil.copytree(source_data / "theme", target / "theme")
        if (source_data / "resources").is_dir():
            shutil.copytree(source_data / "resources", target / "resources")
        result = validate_restored(pg, args.target_db, manifest, target)
        result["isolation_tag"] = tag
        (target / "RESTORED").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False))
    except Exception:
        (target / "FAILED").write_text("Recovery failed; target remains isolated for inspection.\n")
        raise


def validate_restored(pg, database, manifest, output):
    schema = schema_snapshot(pg, database)
    if schema != manifest["schema"]:
        raise RecoveryError("restored schema differs from backup")
    owners = owner_count(pg, database)
    if owners < 1:
        raise RecoveryError("no active loginable Owner in restored database")
    counts = database_counts(pg, database)
    expected = {**manifest["database_counts"], "sessions": 0}
    if counts != expected:
        raise RecoveryError("restored data counts differ from backup (sessions must be empty)")
    media = media_inventory(pg, database)
    if media != manifest["media"]:
        raise RecoveryError("restored media registry differs from backup")
    verified = validate_media(Path(output) / "resources" / "media", media)
    references = validate_relations(pg, database)
    return {"target_database": database, "backup_id": manifest["backup_id"],
            "schema": schema, "loginable_owners": owners, "counts": counts,
            "verified_media": verified, "verified_references": references,
            "isolation": "database guard active; no server or worker started"}


def release(args):
    if not args.verification_confirmed:
        raise RecoveryError("complete isolated verification and pass --verification-confirmed")
    target = Path(args.output)
    if (target / "FAILED").exists():
        raise RecoveryError("a failed restore cannot be released")
    result = json.loads((target / "RESTORED").read_text())
    database = result["target_database"]
    if not RESTORE_NAME.fullmatch(database):
        raise RecoveryError("invalid restored database name")
    pg = PgTools(os.environ.get("DATABASE_URL", ""), args.docker_container)
    tag = pg.query("SELECT shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()", database)
    if tag != result["isolation_tag"] or not tag.startswith(ISOLATION_PREFIX):
        raise RecoveryError("database isolation guard does not match this restore")
    if schema_snapshot(pg, database) != result["schema"] or owner_count(pg, database) < 1:
        raise RecoveryError("schema or Owner validation failed")
    validate_media(target / "resources" / "media", media_inventory(pg, database))
    validate_relations(pg, database)
    assert_secret_refs(secret_refs(pg, database))
    # Revoke even sessions created during verification; opening requires a new login.
    pg.query(f'BEGIN; DELETE FROM sessions; COMMENT ON DATABASE "{database}" IS NULL; COMMIT', database)
    (target / "RELEASED").write_text(dt.datetime.now(dt.timezone.utc).isoformat() + "\n")
    (target / "ISOLATED").unlink()
    print(json.dumps({"released": database, "sessions_revoked": True}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    b = sub.add_parser("backup")
    b.add_argument("--output", required=True)
    b.add_argument("--theme-dir")
    b.add_argument("--media-dir")
    b.add_argument("--config", help="deployment TOML for resource paths; database credentials still use DATABASE_URL")
    b.add_argument("--blog-bin", help="blog executable used to resolve TOML configuration")
    b.add_argument("--resource", action="append", default=[], help="additional directory as name=path")
    b.add_argument("--maintenance-confirmed", action="store_true")
    v = sub.add_parser("verify")
    v.add_argument("backup")
    r = sub.add_parser("restore")
    r.add_argument("backup")
    r.add_argument("--target-db", required=True)
    r.add_argument("--output", required=True, help="new directory for restored theme and isolation markers")
    r.add_argument("--isolation-confirmed", action="store_true")
    release_parser = sub.add_parser("release", help="remove the database isolation guard after verification")
    release_parser.add_argument("--output", required=True, help="successful restore output directory")
    release_parser.add_argument("--verification-confirmed", action="store_true")
    for command in (b, r, release_parser):
        command.add_argument("--docker-container", help="run PostgreSQL tools inside this DB container")
    args = parser.parse_args()
    try:
        if args.action == "backup":
            paths = resource_paths(args.config, args.blog_bin,
                                   {"media_dir": args.media_dir, "theme_dir": args.theme_dir})
            args.media_dir = paths["media_dir"]
            args.theme_dir = paths["theme_dir"]
            backup(args)
        elif args.action == "verify":
            manifest = verify(args.backup)
            print(json.dumps({"backup_id": manifest["backup_id"], "files": len(manifest["files"])}))
        elif args.action == "restore":
            restore(args)
        else:
            release(args)
    except (RecoveryError, OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"recovery: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
