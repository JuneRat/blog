"""Opt-in PostgreSQL drill: fresh databases only, removed in tearDown.

Build target/debug/blog first, then set BLOG_RECOVERY_TEST=1 and
BLOG_TEST_ADMIN_URL (loopback). Set BLOG_TEST_PG_CONTAINER when PostgreSQL tools
are in Docker instead of PATH. No existing application database is touched.
"""
import argparse
import base64
import contextlib
import io
import json
import os
from pathlib import Path
import socket
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
import media_cleanup

PROJECT = Path(__file__).resolve().parent.parent
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=")


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
        (self.media_dir / "objects").mkdir(parents=True)
        self.ids = {key: str(uuid.uuid4()) for key in ("media", "unused", "deleted", "root", "reply", "nested", "series1", "series2")}
        self.cli(["migrate"])
        self.cli(["user", "create", "recovery-owner"])
        self.cli(["user", "passwd", "--user", "recovery-owner", "--password-stdin"], password="Recovery drill password 2026!\n")
        self.cli(["role", "assign", "--user", "recovery-owner", "--role", "owner"])
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
                   BLOG_MIGRATIONS_DIR=str(PROJECT / "migrations/postgres"),
                   BLOG_THEME_DIR=str(PROJECT / "themes/default"),
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
        self.query(f"INSERT INTO pages(id,title,slug,content,content_html,content_render_version,status,published_at) VALUES(gen_random_uuid(),'Page','drill-page','![image](/media/{mid})','<p><img src=\"/media/{mid}\"></p>',1,'scheduled',now()-interval '1 day'); INSERT INTO media_refs SELECT '{mid}','page',id FROM pages")
        for key in ("series1", "series2"):
            sid = self.ids[key]
            self.query(f"INSERT INTO series(id,name,slug,cover_media_id) VALUES('{sid}','{key}','{key}','{mid}'); INSERT INTO media_refs VALUES('{mid}','series','{sid}'); INSERT INTO post_series(post_id,series_id,position) VALUES('{post}','{sid}',2)")
        for key, parent in (("root", None), ("reply", "root"), ("nested", "reply")):
            parent_sql = "NULL" if parent is None else f"'{self.ids[parent]}'"
            root_sql = "NULL" if parent is None else f"'{self.ids['root']}'"
            self.query(f"INSERT INTO comments(id,post_id,parent_id,root_id,author_name,content,content_html,content_render_version,status,ip_address,created_at) VALUES('{self.ids[key]}','{post}',{parent_sql},{root_sql},'Guest','body','<p>body</p>',1,'{'trash' if key == 'root' else 'approved'}','192.0.2.1',now()-interval '200 days')")
        self.query(f"INSERT INTO sessions(token_hash,user_id,csrf_token,auth_version,expires_at) SELECT repeat('1',64),id,repeat('2',64),auth_version,now()+interval '1 day' FROM users WHERE id='{owner}'")
        self.query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) VALUES(gen_random_uuid(),'fixture','system','old',now()-interval '200 days')")

    def test_roundtrip_roles_isolation_and_media_integrity(self):
        # Use the shipped grant script, then exercise actual LOGIN identities.
        for role in (self.app_role, self.maint_role):
            self.query(f"CREATE ROLE {role} LOGIN PASSWORD 'drill-password'")
        self.pg.run("psql", ["-X", "-v", "ON_ERROR_STOP=1", "-v", f"app_role={self.app_role}",
                            "-v", f"maintenance_role={self.maint_role}"], database=self.source,
                    input_path=PROJECT / "scripts/database-roles.sql")
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
        args = argparse.Namespace(output=backup, theme_dir=PROJECT / "themes/default", media_dir=self.media_dir,
                                  resource=[], maintenance_confirmed=True, docker_container=self.container)
        with patch.dict(os.environ, {"DATABASE_URL":self.url(self.source)}), contextlib.redirect_stdout(io.StringIO()):
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
            output=self.root / "restore"
            recovery.restore(argparse.Namespace(backup=backup,target_db=self.target,output=output,isolation_confirmed=True,docker_container=self.container))
        self.assertEqual(self.query("SELECT count(*) FROM sessions",self.target),"0")
        self.assertTrue((output / "resources/media/objects/unregistered.png").is_file())
        self.cli(["serve","--addr","127.0.0.1:0"],database=self.target,success=False)
        self.cli(["publish-due"],database=self.target,success=False)
        blocked=self.cli(["maintenance"],BLOG_MAINTENANCE_DATABASE_URL=self.url(self.target,self.maint_role),success=False)
        self.assertIn("恢复隔离期间禁止",blocked.stderr)
        self.cli(["serve","--addr","0.0.0.0:0"],database=self.target,success=False,BLOG_RECOVERY_MODE="1")
        self.check_isolated_server(output)
        self.assertEqual(self.query("SELECT status FROM posts WHERE slug='drill-scheduled'",self.target),"scheduled")
        # A lost reference prevents release; session revocation is done only at successful release.
        self.query("DELETE FROM media_refs WHERE source_type='site'",self.target)
        release_args=argparse.Namespace(output=output,verification_confirmed=True,docker_container=self.container)
        with patch.dict(os.environ, {"DATABASE_URL":self.admin_url}), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(recovery.RecoveryError): recovery.release(release_args)
            self.assertTrue((output / "ISOLATED").exists())
            self.query(f"INSERT INTO media_refs VALUES('{self.ids['media']}','site','00000000-0000-0000-0000-000000000000')",self.target)
            recovery.release(release_args)
        self.assertEqual(self.query("SELECT count(*) FROM sessions",self.target),"0")
        self.assertTrue((output / "RELEASED").is_file())
        self.cli(["publish-due"],database=self.target)
        self.assertEqual(self.query("SELECT status FROM posts WHERE slug='drill-scheduled'",self.target),"published")

    def check_isolated_server(self, output):
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

    def media_plan(self, ids=None):
        pg = recovery.PgTools(self.url(self.source), self.container)
        output = self.root / ("purge-" + uuid.uuid4().hex + ".json")
        media_cleanup.create_plan(argparse.Namespace(
            id=ids or [self.ids["unused"]], media_dir=self.media_dir, output=output), pg)
        return pg, argparse.Namespace(plan=output, maintenance_confirmed=True, break_links_confirmed=True)

    def trash_unused(self):
        self.query(f"UPDATE media SET deleted_at=now(),version=version+1 WHERE id='{self.ids['unused']}'")
        return self.media_dir / ("objects/" + self.ids["unused"] + ".png")

    def test_media_purge_rechecks_refs_versions_files_and_database_identity(self):
        # Active unreferenced media and historical trash references are both protected.
        for mid in (self.ids["unused"], self.ids["deleted"]):
            with self.assertRaises(recovery.RecoveryError): self.media_plan([mid])
        path = self.trash_unused()
        pg, args = self.media_plan()
        plan, _ = media_cleanup.read_plan(args.plan)
        self.assertEqual(plan["items"][0]["id"], self.ids["unused"])
        self.query(f"UPDATE media SET version=version+1 WHERE id='{self.ids['unused']}'")
        with self.assertRaisesRegex(recovery.RecoveryError, "stale media plan"):
            media_cleanup.apply_plan(args, pg)
        pg, args = self.media_plan()
        mid = self.ids["unused"]
        self.query(f"INSERT INTO media_refs SELECT '{mid}','post',id FROM posts WHERE slug='drill-private'")
        with self.assertRaisesRegex(recovery.RecoveryError, "media is referenced"):
            media_cleanup.apply_plan(args, pg)
        self.query(f"DELETE FROM media_refs WHERE media_id='{mid}'")
        # Explicit columns still protect a media row when its bookkeeping ref is lost.
        self.query(f"UPDATE users SET avatar_media_id='{mid}' WHERE username='recovery-owner'")
        with self.assertRaisesRegex(recovery.RecoveryError, "media is referenced"):
            media_cleanup.apply_plan(args, pg)
        self.query(f"UPDATE users SET avatar_media_id='{self.ids['media']}' WHERE username='recovery-owner'")
        path.write_bytes(b"changed")
        with self.assertRaisesRegex(recovery.RecoveryError, "file missing or changed"):
            media_cleanup.apply_plan(args, pg)
        path.write_bytes(PNG)
        other_pg = recovery.PgTools(self.admin_url, self.container)
        with self.assertRaisesRegex(recovery.RecoveryError, "different database"):
            media_cleanup.apply_plan(args, other_pg)
        self.query(f"COMMENT ON DATABASE {self.source} IS 'blog:recovery-isolated:test'")
        with self.assertRaisesRegex(recovery.RecoveryError, "recovery-isolated"):
            media_cleanup.apply_plan(args, pg)
        self.assertTrue(path.is_file())
        self.assertEqual(self.query("SELECT count(*) FROM audit_logs WHERE action='media.purge'"), "0")

    def test_media_purge_audit_rollback_and_partial_file_retry(self):
        path = self.trash_unused()
        # Exercise SQL literal escaping with a real stored path, plus a multi-row plan.
        second = str(uuid.uuid4())
        relative = "objects/有'引号.png"
        other = self.media_dir / relative
        other.write_bytes(PNG)
        self.query(f"INSERT INTO media(id,path,filename,mime_type,size,width,height,checksum_sha256,deleted_at) VALUES('{second}',{media_cleanup.literal(relative)},'test.png','image/png',{len(PNG)},1,1,'{recovery.digest(other)}',now())")
        pg, args = self.media_plan([self.ids["unused"], second])
        self.query("CREATE FUNCTION fail_media_purge() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'audit unavailable'; END $$; CREATE TRIGGER fail_media_purge BEFORE INSERT ON audit_logs FOR EACH ROW WHEN (NEW.action='media.purge') EXECUTE FUNCTION fail_media_purge()")
        with self.assertRaisesRegex(recovery.RecoveryError, "audit unavailable"):
            media_cleanup.apply_plan(args, pg)
        self.assertEqual(self.query(f"SELECT count(*) FROM media WHERE id IN ('{second}','{self.ids['unused']}')"), "2")
        self.assertTrue(path.is_file() and other.is_file())
        self.query("DROP TRIGGER fail_media_purge ON audit_logs; DROP FUNCTION fail_media_purge()")
        unlink = Path.unlink
        def fail_one(target, *args, **kwargs):
            if target == other.resolve(): raise PermissionError("temporary unlink failure")
            return unlink(target, *args, **kwargs)
        with patch.object(Path, "unlink", fail_one):
            result = media_cleanup.apply_plan(args, pg)
        self.assertEqual(result["files_deleted"], 1)
        self.assertEqual([f["id"] for f in result["failures"]], [second])
        self.assertEqual(self.query(f"SELECT count(*) FROM media WHERE id IN ('{second}','{self.ids['unused']}')"), "0")
        self.assertEqual(self.query("SELECT count(*) FROM audit_logs WHERE action='media.purge'"), "2")
        # A retry must not remove a substituted file even though DB deletion committed.
        other.write_bytes(b"replacement")
        with self.assertRaisesRegex(recovery.RecoveryError, "file missing or changed"):
            media_cleanup.apply_plan(args, pg)
        self.assertEqual(other.read_bytes(), b"replacement")
        other.write_bytes(PNG)
        result = media_cleanup.apply_plan(args, pg)
        self.assertEqual((result["files_deleted"], result["files_already_absent"], result["failures"]), (1, 1, []))
        result = media_cleanup.apply_plan(args, pg)
        self.assertEqual((result["files_deleted"], result["files_already_absent"]), (0, 2))
        self.assertEqual(self.query("SELECT count(*) FROM audit_logs WHERE action='media.purge'"), "2")
        self.assertTrue((self.media_dir / "objects/unregistered.png").is_file())
        self.assertTrue((self.media_dir / ("objects/" + self.ids["deleted"] + ".png")).is_file())

    def test_media_purge_uncertain_commit_and_missing_receipt_fail_closed(self):
        path = self.trash_unused()
        pg, args = self.media_plan()
        query = pg.query
        def lose_commit_reply(sql, *other, **kwargs):
            result = query(sql, *other, **kwargs)
            if sql.startswith("BEGIN;"): raise recovery.RecoveryError("connection lost after commit")
            return result
        with patch.object(pg, "query", lose_commit_reply):
            with self.assertRaisesRegex(recovery.RecoveryError, "connection lost after commit"):
                media_cleanup.apply_plan(args, pg)
        self.assertTrue(path.is_file())
        self.assertEqual(self.query(f"SELECT count(*) FROM media WHERE id='{self.ids['unused']}'"), "0")
        self.assertEqual(self.query("SELECT count(*) FROM audit_logs WHERE action='media.purge'"), "1")
        # An absent DB row alone is insufficient evidence to delete the physical object.
        receipt = self.query("SELECT to_jsonb(a) FROM audit_logs a WHERE action='media.purge'")
        self.query("DELETE FROM audit_logs WHERE action='media.purge'")
        with self.assertRaisesRegex(recovery.RecoveryError, "missing media without this plan receipt"):
            media_cleanup.apply_plan(args, pg)
        self.assertTrue(path.is_file())
        # Put back the captured fixture receipt, then the original plan safely resumes.
        self.query(f"INSERT INTO audit_logs SELECT * FROM jsonb_populate_record(NULL::audit_logs,{media_cleanup.literal(receipt)}::jsonb)")
        result = media_cleanup.apply_plan(args, pg)
        self.assertEqual((result["files_deleted"], result["failures"]), (1, []))

    def test_media_purge_waits_for_restore_and_rejects_the_stale_plan(self):
        path = self.trash_unused()
        pg, args = self.media_plan()
        mid = self.ids["unused"]
        # The restoring writer holds the row until commit; cleanup must inspect the
        # committed new version after its FOR UPDATE acquires that same row.
        sql = f"SET application_name='media_restore_lock'; BEGIN; UPDATE media SET deleted_at=NULL,version=version+1 WHERE id='{mid}'; SELECT pg_sleep(4); COMMIT;"
        command, env = pg.command("psql", ["-X", "-v", "ON_ERROR_STOP=1", "-c", sql])
        process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            for _ in range(50):
                if self.query("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name='media_restore_lock' AND wait_event='PgSleep')") == "t":
                    break
                time.sleep(0.05)
            else: self.fail("restoring writer did not acquire its row lock")
            with self.assertRaisesRegex(recovery.RecoveryError, "stale media plan"):
                media_cleanup.apply_plan(args, pg)
            out, err = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, out + err)
        finally:
            if process.poll() is None: process.kill()
            process.communicate()
        self.assertTrue(path.is_file())
        self.assertEqual(self.query(f"SELECT deleted_at IS NULL FROM media WHERE id='{mid}'"), "t")
        self.assertEqual(self.query("SELECT count(*) FROM audit_logs WHERE action='media.purge'"), "0")


if __name__ == "__main__":
    unittest.main()
