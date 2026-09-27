"""Schema and filesystem checks shared by isolated recovery and media maintenance."""
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path, PurePosixPath
import uuid

SCHEMA_ID = "blog-19-v1"
SCHEMA_TABLES = (
    "users", "oauth_accounts", "sessions", "roles", "permissions", "user_roles",
    "role_permissions", "media", "media_refs", "posts", "pages", "categories",
    "series", "tags", "post_tags", "post_series", "comments", "settings", "audit_logs",
)
ISOLATION_PREFIX = "blog:recovery-isolated:"

class RecoveryError(Exception):
    pass


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def query_json(pg, sql, database=None):
    return json.loads(pg.query(sql, database))


def expected_migrations():
    directory = Path(__file__).resolve().parent.parent / "migrations" / "postgres"
    return [{"version": int(path.name.split("_", 1)[0]),
             "checksum": hashlib.sha384(path.read_bytes()).hexdigest()}
            for path in sorted(directory.glob("*.sql"))]


def schema_snapshot(pg, database=None):
    tables = pg.query("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name<>'_sqlx_migrations' ORDER BY table_name", database).splitlines()
    if set(tables) != set(SCHEMA_TABLES):
        raise RecoveryError("database does not match the supported 19-table baseline")
    migrations = query_json(pg, "SELECT COALESCE(jsonb_agg(jsonb_build_object('version',version,'checksum',encode(checksum,'hex')) ORDER BY version),'[]') FROM _sqlx_migrations WHERE success", database)
    failed = int(pg.query("SELECT count(*) FROM _sqlx_migrations WHERE NOT success", database))
    if failed or migrations != expected_migrations():
        raise RecoveryError("migration history/checksums do not match this checkout; use the matching application revision")
    columns = query_json(pg, "SELECT jsonb_agg(jsonb_build_array(table_name,column_name,udt_name,is_nullable,column_default,character_maximum_length) ORDER BY table_name,ordinal_position) FROM information_schema.columns WHERE table_schema='public' AND table_name<>'_sqlx_migrations'", database)
    required = {("users", "auth_version"), ("sessions", "auth_version"), ("roles", "code"), ("comments", "root_id"), ("comments", "content_html"), ("media", "path"), ("posts", "comments_enabled")}
    if not required.issubset({(column[0],column[1]) for column in columns}):
        raise RecoveryError("legacy or incomplete schema detected")
    return {"id": SCHEMA_ID, "migrations": migrations, "columns": columns}


def database_counts(pg, database=None):
    parts = [f"'{table}',(SELECT count(*) FROM {table})" for table in SCHEMA_TABLES]
    for table in ("posts", "pages"):
        for state in ("draft", "scheduled", "published", "archived"):
            parts.append(f"'{table}_{state}',(SELECT count(*) FROM {table} WHERE status='{state}')")
        parts.append(f"'{table}_trash',(SELECT count(*) FROM {table} WHERE deleted_at IS NOT NULL)")
    return query_json(pg, "SELECT jsonb_build_object(" + ",".join(parts) + ")", database)


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


class Images(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.ids = set()

    def handle_starttag(self, tag, attrs):
        if tag != "img":
            return
        # Rust's tokenizer uses the first src; sanitize output never duplicates it.
        source = next((value for key,value in attrs if key == "src"), None)
        if source and source.startswith("/media/"):
            try:
                self.ids.add(str(uuid.UUID(source[len("/media/"):])))
            except ValueError:
                pass

    handle_startendtag = handle_starttag


def validate_relations(pg, database=None):
    expected = set()
    for table, kind in (("posts","post"),("pages","page")):
        cover = "cover_media_id" if table == "posts" else "NULL"
        rows = query_json(pg, f"SELECT COALESCE(jsonb_agg(jsonb_build_object('id',id,'html',content_html,'cover',{cover})),'[]') FROM {table}", database)
        for row in rows:
            images = Images(); images.feed(row["html"]); images.close()
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
    return int(pg.query("SELECT count(DISTINCT u.id) FROM users u JOIN user_roles ur ON ur.user_id=u.id JOIN roles r ON r.id=ur.role_id WHERE r.code='owner' AND u.status='active' AND u.deleted_at IS NULL AND (u.password_hash IS NOT NULL OR EXISTS(SELECT 1 FROM oauth_accounts oa WHERE oa.user_id=u.id))", database))
