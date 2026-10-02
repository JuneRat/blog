"""Opt-in PostgreSQL drill: fresh databases only, removed in tearDown.

Build target/debug/blog first, then set BLOG_RECOVERY_TEST=1 and
BLOG_TEST_ADMIN_URL (loopback). Set BLOG_TEST_PG_CONTAINER when PostgreSQL tools
are in Docker instead of PATH. No existing application database is touched.
"""
import argparse
import base64
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import Request, urlopen
import uuid

import recovery
import schema_contract
from test_schema_contract import release_fixture

PROJECT = Path(__file__).resolve().parent.parent
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=")


def theme_release(path):
    """Mirror the documented release framing for a real restore configuration fixture."""
    manifest = json.loads((path / "theme.json").read_text())
    declaration = json.loads((path / "settings.schema.json").read_text()) if (path / "settings.schema.json").exists() else {"config_schema_version": 1, "fields": []}
    fields = []
    for raw in declaration["fields"]:
        field = {"key": raw["key"], "type": raw["type"], "label": raw["label"], "description": raw.get("description", ""), "group": raw.get("group", ""), "default": raw["default"], "min_length": raw.get("min_length"), "max_length": raw.get("max_length"), "min": raw.get("min"), "max": raw.get("max"), "options": raw.get("options", [])}
        fields.append(field)
    schema = json.dumps({"config_schema_version": declaration["config_schema_version"], "fields": fields}, ensure_ascii=False, separators=(",", ":")).encode()
    digest = hashlib.sha256()
    digest.update(manifest["theme_api_version"].to_bytes(4, "big"))
    digest.update(manifest["slug"].encode())
    digest.update(len(schema).to_bytes(8, "big")); digest.update(schema)
    for kind in ("templates", "assets"):
        digest.update(kind.encode())
        for item in sorted((path / kind).rglob("*")):
            if item.is_file():
                name = item.relative_to(path / kind).as_posix().encode(); body = item.read_bytes()
                digest.update(len(name).to_bytes(8, "big")); digest.update(name)
                digest.update(len(body).to_bytes(8, "big")); digest.update(body)
    return digest.hexdigest()


@unittest.skipUnless(os.environ.get("BLOG_RECOVERY_TEST") == "1", "opt-in isolated PostgreSQL drill")
class PostgresRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.admin_url = os.environ["BLOG_TEST_ADMIN_URL"]
        if urlsplit(self.admin_url).hostname not in ("127.0.0.1", "localhost", "::1"):
            self.fail("drill requires a loopback database")
        self.container = os.environ.get("BLOG_TEST_PG_CONTAINER")
        self.pg = recovery.PgTools(self.admin_url, self.container)
        suffix = uuid.uuid4().hex[:12]
        self.source = "blog_drill_" + suffix
        self.target = "blog_restore_" + suffix
        self.app_role = "blog_app_" + suffix
        self.maint_role = "blog_maint_" + suffix
        self.temp = tempfile.TemporaryDirectory(prefix="blog-recovery-drill-")
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(self.cleanup_database)
        self.pg.run("createdb", ["--template=template0", self.source], database="postgres")
        self.media_dir = self.root / "media"
        self.theme_dir = self.root / "themes"
        shutil.copytree(PROJECT / "themes/default", self.theme_dir / "default")
        shutil.copytree(PROJECT / "theme-packages/paper", self.theme_dir / "paper")
        shutil.copytree(PROJECT / "themes/default", self.theme_dir / ".theme-previous-default")
        (self.theme_dir / ".theme-previous-default/templates/index.html").write_text("previous release")
        (self.media_dir / "objects").mkdir(parents=True)
        self.ids = {key: str(uuid.uuid4()) for key in ("media", "unused", "deleted", "root", "reply", "nested", "series1", "series2", "post_revision", "page_revision")}
        self.cli(["migrate"])
        self.cli(["user", "create", "recovery-owner"])
        self.cli(["user", "passwd", "--user", "recovery-owner", "--password-stdin"], password="Recovery drill password 2026!\n")
        self.cli(["role", "assign", "--user", "recovery-owner", "--role", "admin"])
        self.seed()

    def cleanup_database(self):
        for database in (self.source, self.target):
            self.pg.query(f'DROP DATABASE IF EXISTS "{database}" WITH (FORCE)', "postgres")
        for role in (self.app_role, self.maint_role):
            self.pg.query(f'DROP ROLE IF EXISTS "{role}"', "postgres")

    def url(self, database, role=None):
        url = urlsplit(self.admin_url)
        host = f"[{url.hostname}]" if ":" in url.hostname else url.hostname
        netloc = f"{role}:drill-password@{host}:{url.port or 5432}" if role else url.netloc
        return urlunsplit((url.scheme, netloc, "/" + database, "", ""))

    def env(self, database=None, role=None, **extra):
        env = dict(os.environ, DATABASE_URL=self.url(database or self.source, role),
                   BLOG_CONFIG_FILE=str(self.root / "config.toml"),
                   BLOG_MIGRATIONS_DIR=str(PROJECT / "migrations/postgres"),
                   BLOG_THEME_DIR=str(self.theme_dir / "default"),
                   BLOG_MEDIA_DIR=str(self.media_dir), BLOG_RECOVERY_MODE="0",
                   BLOG_PUBLIC_BASE_URL="http://127.0.0.1:8080", BLOG_SECURE_COOKIES="0")
        env.update(extra)
        return env

    def cli(self, args, database=None, role=None, password=None, success=True, **extra):
        result = subprocess.run([str(PROJECT / "target/debug/blog"), *args], cwd=PROJECT,
                                env=self.env(database, role, **extra), input=password,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def query(self, sql, database=None):
        return self.pg.query(sql, database or self.source)

    def seed(self):
        owner = self.query("SELECT id FROM users WHERE username='recovery-owner'")
        for key in ("media", "unused", "deleted"):
            mid = self.ids[key]
            path = "objects/" + mid + ".png"
            target = self.media_dir / path
            target.write_bytes(PNG)
            self.query(f"INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256,deleted_at) VALUES('{mid}','{path}','test.png','image/png',{len(PNG)},1,1,'{recovery.digest(target)}',{'now()' if key == 'deleted' else 'NULL'})")
        (self.media_dir / "objects/unregistered.png").write_bytes(PNG)
        for state in ("draft", "scheduled", "published", "archived", "private", "trash"):
            actual = "published" if state in ("private", "trash") else state
            visibility = "private" if state == "private" else "public"
            mid = self.ids["deleted" if state == "trash" else "media"]
            self.query(f"INSERT INTO posts(id,author_id,title,slug,content,content_html,content_render_version,status,visibility,published_at,deleted_at) VALUES(gen_random_uuid(),'{owner}','{state}','drill-{state}','![image](/media/{mid})','<p><img src=\"/media/{mid}\" alt=\"image\"></p>',1,'{actual}','{visibility}',now()-interval '1 day',{'now()' if state == 'trash' else 'NULL'})")
            self.query(f"INSERT INTO media_refs SELECT '{mid}','post',id FROM posts WHERE slug='drill-{state}'")
        post = self.query("SELECT id FROM posts WHERE slug='drill-published'")
        mid = self.ids["media"]
        self.query(f"UPDATE users SET avatar_media_id='{mid}' WHERE id='{owner}'; INSERT INTO media_refs VALUES('{mid}','user','{owner}')")
        self.query(f"INSERT INTO settings(key,value) VALUES('site','{{\"logo_media_id\":\"{mid}\"}}'),('theme','{{\"slug\":\"paper\"}}'); INSERT INTO media_refs VALUES('{mid}','site','00000000-0000-0000-0000-000000000000')")
        theme_id = str(uuid.uuid4())
        release = theme_release(self.theme_dir / "default")
        self.query(f"INSERT INTO themes(id,slug,config,config_schema_version,release,media_fields,created_at,updated_at) VALUES('{theme_id}','default','{{\"header_image\":\"{mid}\",\"footer_note\":\"restore theme fixture\"}}',1,'{release}',ARRAY['header_image'],now(),now()); INSERT INTO media_refs VALUES('{mid}','theme','{theme_id}')")
        self.query("INSERT INTO plugin_runtime(schema_version,render_revision,version,updated_at) VALUES(1,0,6,now()); INSERT INTO plugins(id,enabled,config,version,created_at,updated_at) VALUES('markdown-enhance',false,'{\"math\":true,\"mermaid\":false}',3,now(),now())")
        self.query(f"INSERT INTO pages(id,title,slug,content,content_html,content_render_version,status,published_at) VALUES(gen_random_uuid(),'Page','drill-page','![image](/media/{mid})','<p><img src=\"/media/{mid}\"></p>',1,'scheduled',now()-interval '1 day'); INSERT INTO media_refs SELECT '{mid}','page',id FROM pages")
        for key in ("series1", "series2"):
            sid = self.ids[key]
            self.query(f"INSERT INTO series(id,name,slug,cover_media_id) VALUES('{sid}','{key}','{key}','{mid}'); INSERT INTO media_refs VALUES('{mid}','series','{sid}'); INSERT INTO post_series(post_id,series_id,position) VALUES('{post}','{sid}',2)")
        for key, parent in (("root", None), ("reply", "root"), ("nested", "reply")):
            parent_sql = "NULL" if parent is None else f"'{self.ids[parent]}'"
            root_sql = "NULL" if parent is None else f"'{self.ids['root']}'"
            self.query(f"INSERT INTO comments(id,post_id,parent_id,root_id,author_name,content,content_html,content_render_version,status,ip_address,created_at) VALUES('{self.ids[key]}','{post}',{parent_sql},{root_sql},'Guest','body','<p>body</p>',1,'{'trash' if key == 'root' else 'approved'}','192.0.2.1',now()-interval '200 days')")
        self.query(f"INSERT INTO sessions(token_hash,user_id,csrf_token,auth_version,expires_at) SELECT repeat('1',64),id,repeat('2',64),auth_version,now()+interval '1 day' FROM users WHERE id='{owner}'")
        self.query(f"INSERT INTO account_links(user_id,token_hash,email,auth_version,issued_at,expires_at) SELECT id,repeat('a',43),'owner@example.com',auth_version,now(),now()+interval '30 minutes' FROM users WHERE id='{owner}'")
        # Keep pending editor copies separate from the live source across a full backup/restore.
        for kind, slug, key in (("post", "drill-published", "post_revision"), ("page", "drill-page", "page_revision")):
            rid = self.ids[key]
            historical_image = self.ids["unused"]
            data = json.dumps({"slug": slug, "title": "Pending revision", "content": f"![history](/media/{historical_image})", "visibility": "private", "excerpt": None, "tag_ids": [], "category_id": None, "series": [], "cover_media_id": None})
            self.query(f"INSERT INTO content_revisions(id,{kind}_id,version,data,media_ids,actor_id,created_at) SELECT '{rid}',id,2,'{data}',ARRAY['{historical_image}']::uuid[],'{owner}',now() FROM {kind}s WHERE slug='{slug}'; INSERT INTO media_refs VALUES('{historical_image}','revision','{rid}'); UPDATE {kind}s SET version=2,draft_revision_id='{rid}' WHERE slug='{slug}'")
        self.query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) VALUES(gen_random_uuid(),'fixture','system','old',now()-interval '200 days')")

    def test_roundtrip_roles_isolation_and_media_integrity(self):
        # Use the shipped grant script, then exercise actual LOGIN identities.
        for role in (self.app_role, self.maint_role):
            self.query(f"CREATE ROLE {role} LOGIN PASSWORD 'drill-password'")
        self.pg.run("psql", ["-X", "-v", "ON_ERROR_STOP=1", "-v", f"app_role={self.app_role}",
                            "-v", f"maintenance_role={self.maint_role}"], database=self.source,
                    input_path=PROJECT / "scripts/database-roles.sql")
        schema_contract.verify_database(self.pg, schema_contract.load_contract(), self.source,
                                        self.app_role, self.maint_role)
        self.cli(["migrate"], role=self.app_role)
        checksum=self.query("SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=1")
        self.query("UPDATE _sqlx_migrations SET checksum=decode('00','hex') WHERE version=1")
        mismatch=self.cli(["migrate"],role=self.app_role,success=False)
        self.assertIn("校验和不匹配",mismatch.stderr)
        self.query(f"UPDATE _sqlx_migrations SET checksum=decode('{checksum}','hex') WHERE version=1")
        app = recovery.PgTools(self.url(self.source, self.app_role), self.container)
        for sql in ("DELETE FROM audit_logs", "UPDATE audit_logs SET action='changed'", "TRUNCATE audit_logs"):
            with self.assertRaises(recovery.RecoveryError): app.query(sql)
        maint = recovery.PgTools(self.url(self.source, self.maint_role), self.container)
        for sql in ("UPDATE comments SET content='changed'", "SELECT author_email FROM comments", "UPDATE settings SET version=version+1", "UPDATE audit_logs SET action='changed'"):
            with self.assertRaises(recovery.RecoveryError): maint.query(sql)
        cleaned = self.cli(["maintenance"], BLOG_MAINTENANCE_DATABASE_URL=self.url(self.source,self.maint_role), BLOG_MIGRATIONS_DIR="unavailable", BLOG_PUBLIC_BASE_URL="unavailable")
        result=json.loads(cleaned.stdout.strip())
        self.assertEqual((result["comment_ips"],result["audit_logs"]),(3,1))

        backup = self.root / "backup"
        args = argparse.Namespace(output=backup, theme_dir=self.theme_dir / "default", media_dir=self.media_dir,
                                  resource=[], maintenance_confirmed=True, docker_container=self.container)
        with patch.dict(os.environ, {"DATABASE_URL":self.url(self.source)}), contextlib.redirect_stdout(io.StringIO()):
            pending = self.theme_dir / (".theme-op-" + str(uuid.uuid4()))
            pending.mkdir()
            with self.assertRaisesRegex(recovery.RecoveryError, "unfinished theme operation"):
                recovery.backup(args)
            self.assertFalse(backup.exists())
            pending.rmdir()
            # A missing formal object prevents completion, including a soft-deleted one.
            missing=self.media_dir / ("objects/"+self.ids["deleted"]+".png")
            missing.unlink()
            with self.assertRaises(recovery.RecoveryError): recovery.backup(args)
            self.assertFalse(backup.exists())
            missing.write_bytes(PNG)
            recovery.backup(args)
            manifest=recovery.verify(backup)
            self.assertEqual(len(manifest["media"]),3)
            self.assertEqual(manifest["database_counts"]["sessions"],1)
            self.assertEqual(manifest["database_counts"]["plugins"],1)
            self.assertEqual(manifest["database_counts"]["plugin_runtime"],1)
            output=self.root / "restore"
            recovery.restore(argparse.Namespace(backup=backup,target_db=self.target,output=output,isolation_confirmed=True,docker_container=self.container))
        self.assertEqual(self.query("SELECT count(*) FROM sessions",self.target),"0")
        self.assertEqual(self.query("SELECT count(*) FROM account_links",self.target),"0")
        self.assertEqual(self.query("SELECT count(*) FROM content_revisions",self.target),"2")
        for kind, key in (("post", "post_revision"), ("page", "page_revision")):
            self.assertEqual(self.query(f"SELECT draft_revision_id FROM {kind}s WHERE draft_revision_id IS NOT NULL",self.target),self.ids[key])
            self.assertEqual(self.query(f"SELECT data->>'title' FROM content_revisions WHERE id='{self.ids[key]}'",self.target),"Pending revision")
        self.assertEqual(self.query("SELECT count(*) FROM media_refs WHERE source_type='revision'",self.target),"2")
        self.assertEqual(self.query("SELECT config->>'footer_note' FROM themes WHERE slug='default'",self.target), "restore theme fixture")
        self.assertEqual(self.query("SELECT enabled||':'||config::text||':'||version FROM plugins WHERE id='markdown-enhance'",self.target), self.query("SELECT enabled||':'||config::text||':'||version FROM plugins WHERE id='markdown-enhance'"))
        self.assertEqual(self.query("SELECT schema_version||':'||render_revision||':'||version FROM plugin_runtime",self.target), "1:0:6")
        self.assertTrue((output / "resources/media/objects/unregistered.png").is_file())
        self.cli(["serve","--addr","127.0.0.1:0"],database=self.target,success=False)
        self.cli(["publish-due"],database=self.target,success=False)
        blocked=self.cli(["maintenance"],BLOG_MAINTENANCE_DATABASE_URL=self.url(self.target,self.maint_role),success=False)
        self.assertIn("恢复隔离期间禁止",blocked.stderr)
        self.cli(["serve","--addr","0.0.0.0:0"],database=self.target,success=False,BLOG_RECOVERY_MODE="1")
        self.check_isolated_server(output)
        self.assertEqual(self.query("SELECT status FROM posts WHERE slug='drill-scheduled'",self.target),"scheduled")
        # A lost reference prevents release; session revocation is done only at successful release.
        self.query("DELETE FROM media_refs WHERE source_type IN ('site','theme')",self.target)
        release_args=argparse.Namespace(output=output,verification_confirmed=True,docker_container=self.container)
        with patch.dict(os.environ, {"DATABASE_URL":self.admin_url}), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(recovery.RecoveryError): recovery.release(release_args)
            self.assertTrue((output / "ISOLATED").exists())
            self.query(f"INSERT INTO media_refs VALUES('{self.ids['media']}','site','00000000-0000-0000-0000-000000000000')",self.target)
            with self.assertRaisesRegex(recovery.RecoveryError, "1 missing"):
                recovery.release(release_args)
            self.assertTrue((output / "ISOLATED").exists())
            self.query(f"INSERT INTO media_refs SELECT '{self.ids['media']}','theme',id FROM themes WHERE slug='default'",self.target)
            recovery.release(release_args)
        self.assertEqual(self.query("SELECT count(*) FROM sessions",self.target),"0")
        self.assertEqual(self.query("SELECT count(*) FROM account_links",self.target),"0")
        self.assertEqual(self.query("SELECT config->>'footer_note' FROM themes WHERE slug='default'",self.target), "restore theme fixture")
        self.assertTrue((output / "RELEASED").is_file())
        self.cli(["publish-due"],database=self.target)
        self.assertEqual(self.query("SELECT status FROM posts WHERE slug='drill-scheduled'",self.target),"published")

    def test_next_release_upgrade_grants_and_versioned_restore(self):
        (self.root / "config.toml").write_text("")
        (self.root / "config.toml").chmod(0o600)
        old = self.root / "old-release"
        new = self.root / "new-release"
        baseline = release_fixture(old)
        upgraded = release_fixture(new, extension=True)
        migrations = str(new / "migrations/postgres")
        old_checksum = self.query("SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=1")
        posts = self.query("SELECT count(*) FROM posts")

        def grants(root):
            self.pg.run("psql", ["-X", "-v", "ON_ERROR_STOP=1", "-v", f"app_role={self.app_role}",
                                "-v", f"maintenance_role={self.maint_role}"], database=self.source,
                        input_path=root / "scripts/database-roles.sql")

        def tool(root, args, success=True):
            result = subprocess.run(["python3", "-B", str(root / "scripts/recovery.py"), *map(str, args)],
                                    env=self.env(), capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
            return result

        docker = ["--docker-container", self.container] if self.container else []
        def backup(root, destination):
            tool(root, ["backup", "--output", destination, "--theme-dir", self.theme_dir / "default",
                        "--media-dir", self.media_dir, "--blog-bin", PROJECT / "target/debug/blog",
                        "--maintenance-confirmed", *docker])

        for role in (self.app_role, self.maint_role):
            self.query(f"CREATE ROLE {role} LOGIN PASSWORD 'drill-password'")
        grants(old)
        schema_contract.verify_database(self.pg, baseline, self.source, self.app_role, self.maint_role)
        old_backup = self.root / "old-backup"
        backup(old, old_backup)

        # Upgrade an existing database with real content, preserving 0001 and its history.
        self.cli(["migrate"], BLOG_MIGRATIONS_DIR=migrations)
        self.assertEqual(self.query("SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=1"), old_checksum)
        self.assertEqual(self.query("SELECT count(*) FROM posts"), posts)
        self.assertEqual(int(self.query("SELECT count(*) FROM _sqlx_migrations")), len(upgraded["migrations"]))
        with self.assertRaisesRegex(recovery.RecoveryError, "tables differ"):
            grants(old)
        with self.assertRaisesRegex(schema_contract.SchemaError, "privileges differ"):
            schema_contract.verify_database(self.pg, upgraded, self.source, self.app_role, self.maint_role)
        # Policies can also revoke previously granted columns across releases.
        self.query(f"GRANT SELECT(author_email) ON comments TO {self.maint_role}")
        grants(new)
        schema_contract.verify_database(self.pg, upgraded, self.source, self.app_role, self.maint_role)
        self.cli(["migrate"], role=self.app_role, BLOG_MIGRATIONS_DIR=migrations)
        app = recovery.PgTools(self.url(self.source, self.app_role), self.container)
        app.query("INSERT INTO schema_drill VALUES(gen_random_uuid(),'new release data')")
        maint = recovery.PgTools(self.url(self.source, self.maint_role), self.container)
        with self.assertRaises(recovery.RecoveryError):
            maint.query("SELECT * FROM schema_drill")

        # Strict rejection is intentional: restore old backups with their matching release.
        mismatch = tool(new, ["verify", old_backup], success=False)
        self.assertIn("do not match this application revision", mismatch.stderr)
        output = self.root / "old-restored"
        tool(old, ["restore", old_backup, "--target-db", self.target, "--output", output,
                   "--isolation-confirmed", *docker])
        self.assertEqual(self.query("SELECT count(*) FROM sessions", self.target), "0")
        tool(old, ["release", "--output", output, "--verification-confirmed", *docker])
        # Still offline: release removes the DB marker, never starts any writer or HTTP process.
        self.cli(["migrate"], database=self.target, BLOG_MIGRATIONS_DIR=migrations)
        schema_contract.verify_database(self.pg, upgraded, self.target)
        self.assertEqual(self.query("SELECT count(*) FROM posts", self.target), posts)
        self.assertEqual(self.query("SELECT count(*) FROM schema_drill", self.target), "0")

        # A new-format database round-trips its extra table/data with the same backup format.
        new_backup = self.root / "new-backup"
        backup(new, new_backup)
        manifest = json.loads((new_backup / "manifest.json").read_text())
        self.assertEqual(manifest["format"], recovery.FORMAT)
        self.assertEqual(manifest["database_counts"]["schema_drill"], 1)
        tool(old, ["verify", new_backup], success=False)
        self.query(f'DROP DATABASE "{self.target}" WITH (FORCE)', "postgres")
        output = self.root / "new-restored"
        tool(new, ["restore", new_backup, "--target-db", self.target, "--output", output,
                   "--isolation-confirmed", *docker])
        self.assertEqual(self.query("SELECT note FROM schema_drill", self.target), "new release data")
        self.assertEqual(self.query("SELECT count(*) FROM posts", self.target), posts)
        self.assertEqual(self.query("SELECT count(*) FROM sessions", self.target), "0")
        tool(new, ["release", "--output", output, "--verification-confirmed", *docker])

    def check_isolated_server(self, output):
        self.assertEqual((output / "resources/installed-themes/.theme-previous-default/templates/index.html").read_text(), "previous release")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1",0)); port=sock.getsockname()[1]
        base=f"http://127.0.0.1:{port}"
        env=self.env(self.target,BLOG_RECOVERY_MODE="1",BLOG_PUBLIC_BASE_URL=base,
                     BLOG_MEDIA_DIR=str(output / "resources/media"),
                     BLOG_THEME_DIR=str(output / "resources/installed-themes/default"))
        with tempfile.TemporaryFile() as log:
            process=subprocess.Popen([str(PROJECT / "target/debug/blog"),"serve","--addr",f"127.0.0.1:{port}"],cwd=PROJECT,env=env,stdout=log,stderr=log)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        log.seek(0); self.fail(log.read().decode())
                    try:
                        with urlopen(base+"/",timeout=1) as response: self.assertEqual(response.status,200)
                        break
                    except (URLError,TimeoutError): time.sleep(0.1)
                else: self.fail("isolated server did not start")
                for key in ("media","unused","deleted"):
                    with urlopen(base+"/media/"+self.ids[key],timeout=2) as response: self.assertEqual(response.read(),PNG)
                with self.assertRaises(HTTPError) as hidden: urlopen(base+"/posts/drill-private",timeout=2)
                self.assertEqual(hidden.exception.code,404)
                login=Request(base+"/auth/login/password",data=json.dumps({"username":"recovery-owner","password":"Recovery drill password 2026!"}).encode(),headers={"Content-Type":"application/json"})
                with urlopen(login,timeout=10) as response:
                    self.assertEqual(response.status,200)
                    self.assertIn("blog_session=",response.headers.get("Set-Cookie",""))
            finally:
                process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)



if __name__ == "__main__":
    unittest.main()
