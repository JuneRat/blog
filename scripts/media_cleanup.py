#!/usr/bin/env python3
"""Explicit, maintenance-window deletion of selected trashed media.

Plan first, then review outside links before apply. Never sweeps zero-reference or
unregistered files. A committed audit receipt allows retry after file failures.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import sys
import uuid

from recovery import PgTools
from deployment_config import resource_paths
from recovery_inventory import RecoveryError, ISOLATION_PREFIX, digest, safe_file

FORMAT = 1
MAX_ITEMS = 1000


def literal(value):
    return "E'" + str(value).replace("\\", "\\\\").replace("'", "''") + "'"


def fingerprint(pg):
    identity = json.loads(pg.query("SELECT jsonb_build_object('name',datname,'oid',oid::text,'guard',shobj_description(oid,'pg_database')) FROM pg_database WHERE datname=current_database()"))
    if (identity.pop("guard") or "").startswith(ISOLATION_PREFIX):
        raise RecoveryError("media purge is disabled while the database is recovery-isolated")
    identity["endpoint"] = ({"container": pg.container} if pg.container else
                            {"host": pg.config["PGHOST"], "port": pg.config["PGPORT"]})
    return identity


def media_row(alias):
    return (f"jsonb_build_object('id',{alias}.id,'path',{alias}.path,'size',{alias}.size,"
            f"'sha256',{alias}.checksum_sha256,'version',{alias}.version,"
            f"'deleted_at',to_char({alias}.deleted_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'))")


def inventory(pg, ids):
    selected = ",".join(literal(mid) + "::uuid" for mid in ids)
    return json.loads(pg.query(f"SELECT COALESCE(jsonb_agg({media_row('m')} ORDER BY id),'[]') FROM media m WHERE id IN ({selected})"))


def referenced_sql(media_id):
    # media_refs is authoritative for body images, including private/trash content.
    # Explicit columns and the singleton logo are checked even if their ref is lost.
    return (f"EXISTS(SELECT 1 FROM media_refs WHERE media_id={media_id}) OR "
            f"EXISTS(SELECT 1 FROM users WHERE avatar_media_id={media_id}) OR "
            f"EXISTS(SELECT 1 FROM posts WHERE cover_media_id={media_id}) OR "
            f"EXISTS(SELECT 1 FROM series WHERE cover_media_id={media_id}) OR "
            f"EXISTS(SELECT 1 FROM settings WHERE key='site' AND (value->>'logo_media_id')::uuid={media_id})")


def validate_file(root, item, missing_ok=False):
    path = safe_file(root, item["path"])
    if not path.exists() and missing_ok:
        return None
    if not path.is_file() or path.stat().st_size != item["size"] or digest(path) != item["sha256"]:
        raise RecoveryError(f"media file missing or changed: {item['id']}")
    return path


def plan_digest(plan):
    data = json.dumps(plan, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
    return hashlib.sha256(data).hexdigest()


def read_plan(path):
    document = json.loads(Path(path).read_text())
    plan = document["plan"]
    if document["sha256"] != plan_digest(plan) or plan["format"] != FORMAT:
        raise RecoveryError("unsupported or damaged media cleanup plan")
    uuid.UUID(plan["operation_id"])
    items = plan["items"]
    if not 1 <= len(items) <= MAX_ITEMS or len({i["id"] for i in items}) != len(items):
        raise RecoveryError("plan must contain 1–1000 distinct media IDs")
    if len({i["path"] for i in items}) != len(items):
        raise RecoveryError("plan contains duplicate storage paths")
    for item in items:
        if str(uuid.UUID(item["id"])) != item["id"] or not item["deleted_at"] or item["version"] < 1:
            raise RecoveryError("invalid media snapshot in plan")
        safe_file(plan["media_root"], item["path"])
    if not Path(plan["media_root"]).is_absolute():
        raise RecoveryError("plan media root must be absolute")
    return plan, document["sha256"]


def create_plan(args, pg):
    ids = sorted({str(uuid.UUID(value)) for value in args.id})
    if not 1 <= len(ids) <= MAX_ITEMS:
        raise RecoveryError("select 1–1000 media IDs explicitly")
    identity = fingerprint(pg)
    root = Path(args.media_dir).absolute()
    if root.is_symlink() or not root.is_dir():
        raise RecoveryError("media root must be an existing directory, not a symbolic link")
    root = root.resolve(strict=True)
    items = inventory(pg, ids)
    if len(items) != len(ids):
        raise RecoveryError("one or more selected media records do not exist")
    for item in items:
        if not item["deleted_at"]:
            raise RecoveryError(f"move media to trash before planning purge: {item['id']}")
        if pg.query("SELECT " + referenced_sql(literal(item["id"]) + "::uuid")) != "f":
            raise RecoveryError(f"media still has a known reference: {item['id']}")
        validate_file(root, item)
    plan = {"format": FORMAT, "operation_id": str(uuid.uuid4()), "database": identity,
            "media_root": str(root), "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "items": items}
    document = {"plan": plan, "sha256": plan_digest(plan)}
    # Exclusive creation prevents accidental replacement of a partly applied plan.
    descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        json.dump(document, stream, ensure_ascii=False, indent=2)
        stream.write("\n"); stream.flush(); os.fsync(stream.fileno())
    return {"plan": str(Path(args.output).absolute()), "items": len(items),
            "bytes": sum(i["size"] for i in items), "operation_id": plan["operation_id"],
            "notice": "Review the selected IDs and outside links. Apply permanently breaks their URLs."}


def receipt_id(plan, item):
    return str(uuid.uuid5(uuid.UUID(plan["operation_id"]), item["id"]))


def receipt_matches_sql(plan, checksum, item):
    expected = {"operation_id": plan["operation_id"], "plan_sha256": checksum,
                "path": item["path"], "sha256": item["sha256"], "size": item["size"],
                "version": item["version"]}
    return (f"EXISTS(SELECT 1 FROM audit_logs WHERE id={literal(receipt_id(plan,item))}::uuid "
            f"AND action='media.purge' AND target_type='media' AND target_id={literal(item['id'])} "
            f"AND metadata @> {literal(json.dumps(expected))}::jsonb)")


def apply_plan(args, pg):
    if not args.maintenance_confirmed or not args.break_links_confirmed:
        raise RecoveryError("stop every writer, review outside links, then pass --maintenance-confirmed --break-links-confirmed")
    plan, checksum = read_plan(args.plan)
    if fingerprint(pg) != plan["database"]:
        raise RecoveryError("plan belongs to a different database/endpoint")
    root = Path(plan["media_root"])
    # Before modifying any row, every file must match. A missing file is accepted
    # only when this exact plan already committed that object's purge receipt.
    for item in plan["items"]:
        committed = pg.query("SELECT " + receipt_matches_sql(plan, checksum, item)) == "t"
        validate_file(root, item, missing_ok=committed)
    statements = ["BEGIN; SET LOCAL lock_timeout='10s'; SET LOCAL standard_conforming_strings=on;"]
    for item in sorted(plan["items"], key=lambda i: i["id"]):
        mid = literal(item["id"]) + "::uuid"
        expected = literal(json.dumps(item)) + "::jsonb"
        receipt = receipt_matches_sql(plan, checksum, item)
        metadata = {"operation_id": plan["operation_id"], "plan_sha256": checksum,
                    "path": item["path"], "sha256": item["sha256"], "size": item["size"],
                    "version": item["version"]}
        # A random dollar quote delimiter cannot collide with user-supplied paths.
        delimiter = "$purge_" + uuid.uuid4().hex + "$"
        statements.append(f"""DO {delimiter}
DECLARE current_snapshot jsonb;
BEGIN
  SELECT {media_row('m')} INTO current_snapshot FROM media m WHERE id={mid} FOR UPDATE;
  IF FOUND THEN
    IF {receipt} THEN RAISE EXCEPTION 'media record reappeared after purge: %', {mid}; END IF;
    IF current_snapshot IS DISTINCT FROM {expected} THEN
      RAISE EXCEPTION 'stale media plan: %', {mid};
    END IF;
    IF {referenced_sql(mid)} THEN RAISE EXCEPTION 'media is referenced: %', {mid}; END IF;
    DELETE FROM media WHERE id={mid};
    INSERT INTO audit_logs(id,action,target_type,target_id,metadata)
      VALUES({literal(receipt_id(plan,item))}::uuid,'media.purge','media',{literal(item['id'])},
        {literal(json.dumps(metadata))}::jsonb || jsonb_build_object('database_role',current_user));
  ELSIF NOT ({receipt}) THEN
    RAISE EXCEPTION 'missing media without this plan receipt: %', {mid};
  END IF;
END {delimiter};""")
    statements.append("COMMIT;")
    # On an uncertain commit, exit without touching files. Retrying the same plan
    # checks durable receipts, not a possibly stale local success flag.
    pg.query("\n".join(statements))
    result = {"operation_id": plan["operation_id"], "records_purged": len(plan["items"]),
              "files_deleted": 0, "files_already_absent": 0, "failures": []}
    for item in plan["items"]:
        try:
            # Recheck committed evidence before each file, including on retries.
            proof = pg.query("SELECT (" + receipt_matches_sql(plan, checksum, item) + ") AND NOT EXISTS(SELECT 1 FROM media WHERE id=" + literal(item["id"]) + "::uuid OR path=" + literal(item["path"]) + ")")
            if proof != "t":
                raise RecoveryError("purge receipt missing or storage path is registered again")
            path = validate_file(root, item, missing_ok=True)
            if path is None:
                result["files_already_absent"] += 1
            else:
                path.unlink()
                result["files_deleted"] += 1
        except (RecoveryError, OSError) as error:
            result["failures"].append({"id": item["id"], "error": str(error)})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    plan = sub.add_parser("plan")
    plan.add_argument("--id", action="append", required=True, help="repeat for each selected media UUID")
    plan.add_argument("--media-dir")
    plan.add_argument("--config", help="deployment TOML for resource paths")
    plan.add_argument("--blog-bin", help="blog executable used to resolve TOML configuration")
    plan.add_argument("--output", required=True, help="new JSON plan file")
    apply = sub.add_parser("apply")
    apply.add_argument("plan")
    apply.add_argument("--maintenance-confirmed", action="store_true")
    apply.add_argument("--break-links-confirmed", action="store_true")
    for command in (plan, apply):
        command.add_argument("--docker-container")
    args = parser.parse_args()
    try:
        if args.action == "plan":
            paths = resource_paths(args.config, args.blog_bin, {"media_dir": args.media_dir})
            args.media_dir = paths["media_dir"]
        pg = PgTools(os.environ.get("DATABASE_URL", ""), args.docker_container)
        result = create_plan(args, pg) if args.action == "plan" else apply_plan(args, pg)
        print(json.dumps(result, ensure_ascii=False))
        return 1 if result.get("failures") else 0
    except (RecoveryError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"media cleanup: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
