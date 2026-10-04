"""Failure boundaries for browser recovery; real database round trips are in test_browser_compose.py."""
import datetime as dt
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import browser_recovery as recovery


class BrowserRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.request = {"state_dir": str(self.root / "state"), "action": "initialize"}
        self.store = recovery.Store(self.request)
        self.store.execute()

    def tearDown(self):
        self.temporary.cleanup()

    def test_public_status_never_exposes_authentication_or_remote_secrets(self):
        self.store.settings.update(credential_hash="private-authentication-hash", remote={
            "endpoint": "https://storage.example", "region": "region", "bucket": "bucket", "prefix": "site",
            "access_key": "private-access-key", "secret_key": "private-secret-key"})
        payload = json.dumps(self.store.status())
        self.assertNotIn("private-", payload)
        self.assertIn("storage.example", payload)

    def test_restart_marks_interrupted_jobs_and_preserves_recovery_gate(self):
        self.store.begin("restore")
        recovery.write_json(self.store.root / "RECOVERY_REQUIRED", {"job": self.store.job["id"]})
        result = recovery.Store(self.request).execute()
        self.assertTrue(result["recovery_required"])
        self.assertEqual(result["jobs"][0]["status"], "interrupted")

    def test_recovery_archive_is_protected_from_automatic_retention(self):
        keep = ["blog-20260101T000000Z-aaaaaaaaaaaa.tar.gz.age", "blog-20260102T000000Z-bbbbbbbbbbbb.tar.gz.age"]
        records = [{"name": name, "protected": i == 0} for i, name in enumerate(keep)]
        self.store.settings["keep"] = 1
        with patch.object(self.store, "status", return_value={"backups": records}), patch.object(self.store, "delete") as delete:
            self.store.prune()
            delete.assert_not_called()

    def test_delete_refused_until_restore_finishes(self):
        (self.store.root / "RECOVERY_REQUIRED").touch()
        with self.assertRaises(recovery.RecoveryError):
            self.store.delete("blog-20260101T000000Z-aaaaaaaaaaaa.tar.gz.age")

    def test_archive_paths_cannot_escape_store(self):
        for name in ("../config.json", "/etc/passwd", "blog-20260101T000000Z-aaaaaaaaaaaa.tar.gz.age/../../secret", "x.age"):
            with self.subTest(name=name), self.assertRaises(recovery.RecoveryError):
                recovery.safe_name(name)

    def test_exclusive_operation_lock(self):
        other = recovery.Store(self.request)
        with self.store.locked(), self.assertRaises(recovery.RecoveryError), other.locked():
            pass

    def archive(self, entries):
        path = self.root / "input.tar.gz"
        with tarfile.open(path, "w:gz") as archive:
            for name, value, kind in entries:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind == tarfile.REGTYPE:
                    info.size = len(value)
                    archive.addfile(info, io.BytesIO(value))
                else:
                    info.linkname = value.decode()
                    archive.addfile(info)
        return path

    def test_tar_rejects_links_traversal_and_duplicates(self):
        for entries in [
            [("../escape", b"data", tarfile.REGTYPE)],
            [("backup/../escape", b"data", tarfile.REGTYPE)],
            [("backup/link", b"/etc/passwd", tarfile.SYMTYPE)],
            [("backup/link", b"/etc/passwd", tarfile.LNKTYPE)],
            [("backup/file", b"x", tarfile.REGTYPE), ("backup/file", b"y", tarfile.REGTYPE)],
        ]:
            with self.subTest(entries=entries), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(recovery.RecoveryError):
                    recovery.safe_extract(self.archive(entries), Path(directory))

    def test_tar_expansion_limit_counts_all_files(self):
        path = self.archive([("backup/a", b"a" * 10, tarfile.REGTYPE), ("backup/b", b"b" * 10, tarfile.REGTYPE)])
        with self.assertRaises(recovery.RecoveryError):
            recovery.safe_extract(path, self.root / "output", maximum=15)
        self.assertFalse((self.root / "output/backup/b").exists())

    def test_tar_extracts_normal_private_files(self):
        path = self.archive([("backup/a", b"payload", tarfile.REGTYPE)])
        recovery.safe_extract(path, self.root / "output")
        file = self.root / "output/backup/a"
        self.assertEqual(file.read_bytes(), b"payload")
        self.assertEqual(file.stat().st_mode & 0o777, 0o600)

    def test_schedule_uses_next_utc_slot_and_never_busy_retries(self):
        now = dt.datetime(2026, 10, 4, 18, 0, tzinfo=dt.timezone.utc)
        settings = {"schedule": "daily", "hour_utc": 18, "weekday": 0}
        self.assertEqual(recovery.next_run(settings, now), (now + dt.timedelta(days=1)).timestamp())
        settings["schedule"] = "weekly"
        self.assertEqual(recovery.next_run(settings, now), (now + dt.timedelta(days=1)).timestamp())
        settings["schedule"] = "off"
        self.assertEqual(recovery.next_run(settings, now), 0)

    def test_schedule_requires_saved_key_and_bounded_retention(self):
        values = {"schedule": "daily", "hour_utc": 18, "weekday": 0, "keep": 7, "remote_keep": 30}
        self.store.request["settings"] = values
        with self.assertRaises(recovery.RecoveryError): self.store.save_schedule()
        self.store.settings["key_confirmed"] = True
        self.store.save_schedule()
        for key, value in (("keep", 0), ("remote_keep", 366), ("hour_utc", 24), ("weekday", -1)):
            previous = values[key]; values[key] = value
            with self.subTest(key=key), self.assertRaises(recovery.RecoveryError): self.store.save_schedule()
            values[key] = previous

    def test_key_is_not_enabled_until_download_is_confirmed(self):
        self.store.settings.update(recipient="public", credential_hash=hashlib.sha256(b"secret").hexdigest())
        self.store.request["key"] = json.dumps({"identity": "private", "emergency_token": "secret"})
        self.assertFalse(self.store.status()["initialized"])
        with patch.object(recovery, "public_key", return_value="public"):
            self.store.confirm_key()
        self.assertTrue(self.store.status()["initialized"])
        self.assertNotIn("secret", json.dumps(self.store.status()))

    def test_wrong_key_cannot_enable_backups(self):
        self.store.settings.update(recipient="public", credential_hash=hashlib.sha256(b"secret").hexdigest())
        self.store.request["key"] = json.dumps({"identity": "private", "emergency_token": "different"})
        with patch.object(recovery, "public_key", return_value="public"), self.assertRaises(recovery.RecoveryError):
            self.store.confirm_key()

    def test_restore_failure_before_database_write_keeps_current_site(self):
        self.store.request.update(action="restore", job_id="a" * 32)
        with patch.object(self.store, "restore", side_effect=recovery.RecoveryError("无效备份")):
            with self.assertRaises(recovery.RecoveryError): self.store.execute()
        self.assertFalse((self.store.root / "RECOVERY_REQUIRED").exists())
        self.assertEqual(self.store.jobs()[0]["status"], "failed")

    def test_remote_failure_preserves_previous_backups(self):
        self.store.settings.update(recipient="public", key_confirmed=True, remote={"endpoint":"private"})
        self.store.begin("backup")
        self.store.request["name"] = "blog-20260101T000000Z-aaaaaaaaaaaa.tar.gz.age"
        with patch.object(self.store, "remote_upload", side_effect=RuntimeError()), patch.object(self.store, "prune") as prune:
            result = self.store.finalize_backup()
            self.assertIsNotNone(result["warning"])
            prune.assert_not_called()

    def test_upload_cleanup_preserves_only_the_incomplete_restore(self):
        import os
        name = "upload-" + "a" * 32 + ".age"
        path = self.store.root / "uploads" / name
        path.write_bytes(b"ciphertext")
        os.utime(path, (1, 1))
        self.store.request["name"] = name
        self.store.begin("restore")
        self.store.phase("failed", status="failed")
        recovery.write_json(self.store.root / "RECOVERY_REQUIRED", {"job":self.store.job["id"]})
        self.store.clean_uploads()
        self.assertTrue(path.exists())
        with self.assertRaises(recovery.RecoveryError): self.store.discard_upload()
        (self.store.root / "RECOVERY_REQUIRED").unlink()
        self.store.clean_uploads()
        self.assertFalse(path.exists())

    def test_legacy_database_errors_never_expose_localized_secret_values(self):
        output = io.StringIO()
        with patch.object(recovery.sys, "stdin", io.StringIO(json.dumps(self.request))), patch.object(recovery.sys, "stdout", output), patch.object(recovery.Store, "execute", side_effect=recovery.RecoveryError("psql failed: 密码 private-database-secret")):
            self.assertEqual(recovery.main(), 1)
        self.assertNotIn("private-database-secret", output.getvalue())
        self.assertNotIn("psql", output.getvalue())

    def test_mount_root_replacement_is_retryable(self):
        source = self.root / "source"; source.mkdir(); (source / "new").write_text("new content")
        target = self.root / "target"; target.mkdir(); (target / "old").write_text("old content")
        unfinished = target / ".browser-recovery-stage"; unfinished.mkdir(); (unfinished / "partial").touch()
        self.store.replace_files(source, target)
        self.store.replace_files(source, target)
        self.assertEqual([p.name for p in target.iterdir()], ["new"])
        self.assertEqual((target / "new").read_text(), "new content")


if __name__ == "__main__":
    unittest.main()
