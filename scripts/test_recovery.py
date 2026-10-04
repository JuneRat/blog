import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import recovery
import recovery_inventory as inventory


class RecoveryTests(unittest.TestCase):
    def test_current_baseline_tables_and_all_lifecycle_states_are_counted(self):
        class Pg:
            def query(self, sql, database):
                for table in recovery.load_contract()["tables"]:
                    assert f'FROM public."{table}"' in sql
                assert "comment_settings" not in sql
                for state in ("draft", "scheduled", "published", "archived"):
                    assert f"status='{state}'" in sql
                return '{"comments":7}'
        counts = recovery.database_counts(Pg())
        self.assertEqual(counts["comments"], 7)
        self.assertTrue({"media", "media_refs", "post_series", "audit_logs", "sessions"}.issubset(recovery.load_contract()["tables"]))

    def bundle(self, root):
        data = root / "data"
        (data / "theme" / "templates").mkdir(parents=True, exist_ok=True)
        for name in ("base.html", "index.html", "post.html", "page.html"):
            (data / "theme" / "templates" / name).write_text(name)
        (data / "theme" / "theme.json").write_text('{"theme_api_version":1}')
        dump = data / "database.dump"
        dump.write_bytes(b"PGDMPfake")
        media = data / "resources" / "media" / "objects" / "test.png"
        media.parent.mkdir(parents=True, exist_ok=True)
        media.write_bytes(b"image bytes")
        manifest = {
            "format": 2, "backup_id": "test", "secret_refs": [],
            "source_database": "blog", "schema": {"id": recovery.load_contract()["id"], "migrations": recovery.expected_migrations()},
            "media": [{"id":"test-media","path":"objects/test.png","size":media.stat().st_size,"sha256":recovery.digest(media)}],
            "database_counts": {}, "files": recovery.file_records(data),
        }
        raw = json.dumps(manifest).encode()
        (root / "manifest.json").write_bytes(raw)
        (root / "COMPLETE").write_text(hashlib.sha256(raw).hexdigest())
        return dump

    def rewrite(self, root, transform):
        manifest = json.loads((root / "manifest.json").read_text())
        transform(manifest)
        raw = json.dumps(manifest).encode()
        (root / "manifest.json").write_bytes(raw)
        (root / "COMPLETE").write_text(hashlib.sha256(raw).hexdigest())

    def test_verify_rejects_legacy_format_or_baseline_checksum_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            self.bundle(root)
            self.rewrite(root,lambda m:m.update(format=1))
            with self.assertRaisesRegex(recovery.RecoveryError,"unsupported backup format"):
                recovery.verify(root)
            self.bundle(root)
            self.rewrite(root,lambda m:m["schema"].update(migrations=[{"version":1,"checksum":"old-schema"}]))
            with self.assertRaisesRegex(recovery.RecoveryError,"checksums"):
                recovery.verify(root)

    def test_media_registry_checksum_is_checked_independently_of_file_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); self.bundle(root)
            # Updating file records cannot hide a missing or changed registered object.
            media=root / "data/resources/media/objects/test.png"
            media.write_bytes(b"corrupt")
            self.rewrite(root,lambda m:m.update(files=recovery.file_records(root / "data")))
            with self.assertRaisesRegex(recovery.RecoveryError,"media object missing or corrupt"):
                recovery.verify(root)
            media.unlink()
            self.rewrite(root,lambda m:m.update(files=recovery.file_records(root / "data")))
            with self.assertRaisesRegex(recovery.RecoveryError,"media object missing or corrupt"):
                recovery.verify(root)

    def test_parent_symlinks_and_unlisted_symlink_directories_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); self.bundle(root)
            (root / "data/empty-link").symlink_to(root / "data/theme",target_is_directory=True)
            with self.assertRaises(recovery.RecoveryError): recovery.verify(root)
            (root / "data/empty-link").unlink()
            for path in ("/tmp/file", "../escape", "objects/../test", "objects//test", "objects/./test", "objects\\test"):
                with self.assertRaises(recovery.RecoveryError): inventory.safe_file(root,path)
            (root / "linked").symlink_to(root / "data",target_is_directory=True)
            with self.assertRaises(recovery.RecoveryError): inventory.safe_file(root,"linked/database.dump")

    def test_html_reference_parser_matches_only_local_media_images(self):
        media_id="00000000-0000-0000-0000-000000000001"
        parser=inventory.Images()
        parser.feed(f'<img src="/media/{media_id}"><a href="/media/{media_id}">link</a><img src="https://example.com/media/{media_id}"><img src="/media/00000000-0000-0000-0000-000000000002?x=1">')
        self.assertEqual(parser.ids,{media_id,"00000000-0000-0000-0000-000000000002"})

    def test_schema_rejects_old_migration_even_if_table_count_matches(self):
        class Pg:
            def query(self,sql,database):
                if "information_schema.tables" in sql: return "\n".join(recovery.load_contract()["tables"])
                if "WHERE NOT success" in sql: return "0"
                return '[{"version":1,"checksum":"legacy-baseline"}]'
        with self.assertRaisesRegex(recovery.RecoveryError,"migration history/checksums"):
            recovery.schema_snapshot(Pg())

    def test_verify_rejects_missing_completion_and_changed_dump(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dump = self.bundle(root)
            recovery.verify(root)
            dump.write_bytes(b"PGDMPchanged")
            with self.assertRaises(recovery.RecoveryError):
                recovery.verify(root)
            self.bundle(root)
            (root / "COMPLETE").unlink()
            with self.assertRaises(recovery.RecoveryError):
                recovery.verify(root)

    def test_verify_rejects_manifest_path_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.bundle(root)
            manifest = json.loads((root / "manifest.json").read_text())
            manifest["files"][0]["path"] = "../escape"
            raw = json.dumps(manifest).encode()
            (root / "manifest.json").write_bytes(raw)
            (root / "COMPLETE").write_text(hashlib.sha256(raw).hexdigest())
            with self.assertRaises(recovery.RecoveryError):
                recovery.verify(root)

    def test_resource_copy_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            (source / "linked").symlink_to(root)
            with self.assertRaises(recovery.RecoveryError):
                recovery.copy_resource(source, root / "copy")

    def test_secret_refs_must_be_present(self):
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(recovery.RecoveryError):
                recovery.assert_secret_refs(["IDP_SECRET"])
        with patch.dict(os.environ, {"IDP_SECRET": "present"}):
            recovery.assert_secret_refs(["IDP_SECRET"])

    def test_null_or_empty_secret_reference_cannot_disappear_from_backup(self):
        class Pg:
            def __init__(self, data): self.data=data
            def query(self, sql, database): return self.data
        for refs in ('[null]', '[""]', '["PRESENT",null]'):
            with self.assertRaises(recovery.RecoveryError): recovery.secret_refs(Pg(refs))
        self.assertEqual(recovery.secret_refs(Pg('["SECRET","SECRET"]')),["SECRET"])

    def test_docker_container_ids_are_accepted_but_options_are_not(self):
        recovery.PgTools("postgres://blog@localhost/blog", "1" * 64)
        with self.assertRaises(recovery.RecoveryError):
            recovery.PgTools("postgres://blog@localhost/blog", "--privileged")

    def test_database_names_are_restricted(self):
        for url in ("sqlite:///blog", "postgres://localhost/bad-name", "postgres://localhost/"):
            with self.assertRaises(recovery.RecoveryError):
                recovery.db_config(url)
        self.assertEqual(recovery.db_config("postgres://user:pass@localhost/blog")["PGDATABASE"], "blog")

    def test_restore_invalidates_persisted_sessions(self):
        class FakePg:
            def __init__(self, url, container=None):
                self.config = {"PGDATABASE": "admin"}
                self.calls = []

            def query(self, sql, database=None):
                self.calls.append(("query", sql))
                if sql.startswith("COMMENT ON DATABASE"):
                    self.tag = sql.split("'")[1]
                if "shobj_description" in sql:
                    return self.tag
                return ""

            def run(self, tool, args, database=None, input_path=None, output_path=None):
                self.calls.append(("run", tool))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.bundle(root)
            fake = FakePg("")
            args = argparse.Namespace(
                backup=root,
                target_db="blog_restore_unit",
                isolation_confirmed=True,
                output=root / "restore-out",
                docker_container=None,
            )
            with (
                patch.object(recovery, "PgTools", return_value=fake),
                patch.object(recovery, "validate_restored", return_value={"ok": True}),
                contextlib.redirect_stdout(io.StringIO()),
            ):
                recovery.restore(args)

            # 持久会话不能因备份回退而复活：恢复流程必须显式清空 sessions。
            self.assertIn(("query", "DELETE FROM sessions"), fake.calls)
            self.assertIn(("query", "DELETE FROM account_links"), fake.calls)
            self.assertTrue((args.output / "ISOLATED").read_text().startswith(recovery.ISOLATION_PREFIX))
            self.assertTrue((args.output / "RESTORED").is_file())


if __name__ == "__main__":
    unittest.main()
