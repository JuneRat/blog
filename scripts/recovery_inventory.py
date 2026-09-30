"""Schema and filesystem checks shared by isolated recovery and media maintenance."""
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path, PurePosixPath
import re
from urllib.parse import unquote
import uuid
from schema_contract import SchemaError as RecoveryError, expected_migrations, load_contract

ISOLATION_PREFIX = "blog:recovery-isolated:"


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def query_json(pg, sql, database=None):
    return json.loads(pg.query(sql, database))


def schema_snapshot(pg, database=None):
    contract = load_contract()
    tables = pg.query("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name<>'_sqlx_migrations' ORDER BY table_name", database).splitlines()
    if set(tables) != set(contract["tables"]):
        raise RecoveryError("database table set does not match schema.json; use the matching application revision")
    migrations = query_json(pg, "SELECT COALESCE(jsonb_agg(jsonb_build_object('version',version,'checksum',encode(checksum,'hex')) ORDER BY version),'[]') FROM _sqlx_migrations WHERE success", database)
    failed = int(pg.query("SELECT count(*) FROM _sqlx_migrations WHERE NOT success", database))
    if failed or migrations != expected_migrations():
        raise RecoveryError("migration history/checksums do not match this checkout; use the matching application revision")
    columns = query_json(pg, "SELECT jsonb_agg(jsonb_build_array(table_name,column_name,udt_name,is_nullable,column_default,character_maximum_length) ORDER BY table_name,ordinal_position) FROM information_schema.columns WHERE table_schema='public' AND table_name<>'_sqlx_migrations'", database)
    required = {("users", "auth_version"), ("sessions", "auth_version"), ("roles", "code"), ("comments", "root_id"), ("comments", "content_html"), ("media", "path"), ("posts", "comments_enabled")}
    if not required.issubset({(column[0],column[1]) for column in columns}):
        raise RecoveryError("legacy or incomplete schema detected")
    return {"id": contract["id"], "migrations": migrations, "columns": columns}


def database_counts(pg, database=None):
    # Aggregate rows rather than passing two arguments per table to a function
    # (PostgreSQL's function-argument limit would otherwise cap schema growth).
    parts = [f"SELECT '{table}' AS key, count(*) AS value FROM public.\"{table}\""
             for table in load_contract()["tables"]]
    for table in ("posts", "pages"):
        for state in ("draft", "scheduled", "published", "archived"):
            parts.append(f"SELECT '{table}_{state}', count(*) FROM public.{table} WHERE status='{state}'")
        parts.append(f"SELECT '{table}_trash', count(*) FROM public.{table} WHERE deleted_at IS NOT NULL")
    return query_json(pg, "SELECT jsonb_object_agg(key,value) FROM (" + " UNION ALL ".join(parts) + ") counts", database)


def media_inventory(pg, database=None):
    return query_json(pg, "SELECT COALESCE(jsonb_agg(jsonb_build_object('id',id,'path',path,'size',size,'sha256',checksum_sha256,'version',version,'deleted_at',to_char(deleted_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) ORDER BY id),'[]') FROM media", database)


def safe_file(root, key):
    if not isinstance(key,str) or not key or "\\" in key or "\x00" in key:
        raise RecoveryError("unsafe resource path")
    relative = PurePosixPath(key)
    if relative.is_absolute() or any(part in ("", ".", "..") for part in key.split("/")):
        raise RecoveryError("unsafe resource path")
    current = Path(root)
    if current.is_symlink():
        raise RecoveryError("resource root must not be a symbolic link")
    for part in relative.parts:
        current /= part
        if current.is_symlink():
            raise RecoveryError(f"symbolic link in resource path: {key}")
    return current


def validate_media(root, records):
    seen = set()
    for item in records:
        if item["path"] in seen:
            raise RecoveryError("duplicate media path")
        seen.add(item["path"])
        target = safe_file(root, item["path"])
        if not target.is_file() or target.stat().st_size != item["size"] or digest(target) != item["sha256"]:
            raise RecoveryError(f"media object missing or corrupt: {item['id']}")
    return len(seen)


_HYPHENATED_UUID = r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}"
_MEDIA_UUID = re.compile(r"(?:[0-9a-fA-F]{32}|" + _HYPHENATED_UUID
                         + r"|\{" + _HYPHENATED_UUID + r"\}|urn:uuid:" + _HYPHENATED_UUID + r")")


def media_url_id(source, pipeline_version=2):
    # Match media_refs.rs and Axum Path<String>: root-relative path, query and
    # fragment excluded, one strict UTF-8 percent decode, Rust's UUID forms.
    if pipeline_version not in (0, 1, 2):
        raise RecoveryError(f"unsupported content pipeline version: {pipeline_version}")
    path = re.split(r"[?#]", source, maxsplit=1)[0] if pipeline_version == 2 else source
    if not path.startswith("/media/"):
        return None
    try:
        value = path[len("/media/"):]
        if pipeline_version == 2:
            value = unquote(value, errors="strict")
        if not _MEDIA_UUID.fullmatch(value):
            return None
        return str(uuid.UUID(value))
    except (ValueError, UnicodeDecodeError):
        return None


class Images(HTMLParser):
    def __init__(self, pipeline_version=2):
        if pipeline_version not in (0, 1, 2):
            raise RecoveryError(f"unsupported content pipeline version: {pipeline_version}")
        super().__init__(convert_charrefs=True)
        self.pipeline_version = pipeline_version
        self.ids = set()

    def handle_starttag(self, tag, attrs):
        if tag != "img":
            return
        # Rust's tokenizer uses the first src; sanitize output never duplicates it.
        source = next((value for key,value in attrs if key == "src"), None)
        if source and (media_id := media_url_id(source, self.pipeline_version)) is not None:
            self.ids.add(media_id)

    handle_startendtag = handle_starttag


def validate_relations(pg, database=None):
    expected = set()
    for table, kind in (("posts","post"),("pages","page")):
        cover = "cover_media_id" if table == "posts" else "NULL"
        rows = query_json(pg, f"SELECT COALESCE(jsonb_agg(jsonb_build_object('id',id,'html',content_html,'cover',{cover},'pipeline_version',content_render_version)),'[]') FROM {table}", database)
        for row in rows:
            # A stored derived result must be checked against the pipeline that
            # created it. Legacy backups can be released, then explicitly rebuilt;
            # the native purge guard refuses new deletion until that upgrade ends.
            images = Images(row["pipeline_version"]); images.feed(row["html"]); images.close()
            for mid in images.ids | ({row["cover"]} if row["cover"] else set()):
                expected.add((mid,kind,row["id"]))
    rows = query_json(pg, "SELECT COALESCE(jsonb_agg(jsonb_build_array(mid,kind,sid)),'[]') FROM (SELECT avatar_media_id AS mid,'user' AS kind,id AS sid FROM users WHERE avatar_media_id IS NOT NULL UNION ALL SELECT cover_media_id,'series',id FROM series WHERE cover_media_id IS NOT NULL UNION ALL SELECT (value->>'logo_media_id')::uuid,'site','00000000-0000-0000-0000-000000000000'::uuid FROM settings WHERE key='site' AND value->>'logo_media_id' IS NOT NULL) refs", database)
    expected.update(tuple(row) for row in rows)
    actual = {tuple(row) for row in query_json(pg, "SELECT COALESCE(jsonb_agg(jsonb_build_array(media_id,source_type,source_id)),'[]') FROM media_refs", database)}
    if actual != expected:
        raise RecoveryError(f"media references disagree with stored HTML/covers: {len(expected-actual)} missing, {len(actual-expected)} unexpected")
    invalid = int(pg.query("WITH RECURSIVE reachable(id,root,post_id) AS (SELECT id,id,post_id FROM comments WHERE parent_id IS NULL AND root_id IS NULL UNION SELECT c.id,r.root,c.post_id FROM comments c JOIN reachable r ON c.parent_id=r.id AND c.post_id=r.post_id AND c.root_id=r.root) SELECT (SELECT count(*) FROM comments)-(SELECT count(*) FROM reachable)", database))
    if invalid:
        raise RecoveryError("comment tree contains cycles or inconsistent roots")
    invalid = int(pg.query("WITH RECURSIVE reachable(id) AS (SELECT id FROM categories WHERE parent_id IS NULL UNION SELECT c.id FROM categories c JOIN reachable r ON c.parent_id=r.id) SELECT (SELECT count(*) FROM categories)-(SELECT count(*) FROM reachable)", database))
    if invalid:
        raise RecoveryError("category tree contains a cycle")
    return len(actual)


def owner_count(pg, database=None):
    # Inspect legacy backups before migration as well as current admin-only data.
    return int(pg.query("SELECT count(DISTINCT u.id) FROM users u JOIN user_roles ur ON ur.user_id=u.id JOIN roles r ON r.id=ur.role_id WHERE r.code IN ('admin','owner') AND u.status='active' AND u.deleted_at IS NULL AND (u.password_hash IS NOT NULL OR EXISTS(SELECT 1 FROM oauth_accounts oa WHERE oa.user_id=u.id))", database))
