"""Container-independent checks for archive trust boundaries and retention failures."""
import io
import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import subprocess
import sys
import shutil
import unittest
from unittest.mock import patch

import compose_recovery as tool


def make_transport(path, content=b"age-encryption.org/v1\ntransport-only-test"):
    path.write_bytes(content)
    tool.transport_path(path).write_text(json.dumps({
        "format": 1, "archive": path.name, "site_id": "a" * 64,
        "size": len(content), "sha256": hashlib.sha256(content).hexdigest(),
    }))


class ArchiveTests(unittest.TestCase):
    def test_private_write_is_private_at_creation_and_replaces_existing_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.toml"
            original_replace = os.replace
            def inspect_replace(source, target):
                self.assertEqual(Path(source).stat().st_mode & 0o777, 0o600)
                self.assertEqual(Path(source).read_text(), "秘密")
                original_replace(source, target)
            old_umask = os.umask(0)
            try:
                with patch.object(tool.os, "replace", side_effect=inspect_replace):
                    tool.private_write(path, "秘密")
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                hardlink = Path(directory) / "old-copy"
                os.link(path, hardlink)
                path.chmod(0o644)
                tool.private_write(path, "new-secret")
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(hardlink.read_text(), "秘密")
                self.assertEqual(path.read_text(), "new-secret")
            finally:
                os.umask(old_umask)

    def test_private_write_rejects_symlinks_and_preserves_original_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.toml"
            path.write_text("original")
            link = Path(directory) / "symlink"
            link.symlink_to(path)
            with self.assertRaises(tool.RecoveryError):
                tool.private_write(link, "changed")
            for operation in ("fsync", "replace"):
                with patch.object(tool.os, operation, side_effect=OSError("disk failure")):
                    with self.assertRaises(OSError):
                        tool.private_write(path, "changed")
                self.assertEqual(path.read_text(), "original")
                self.assertEqual(sorted(p.name for p in Path(directory).iterdir()), ["config.toml", "symlink"])

    def test_maintenance_mode_uses_resolved_compose_settings_or_toml(self):
        model = {"services": {"maintenance": {"environment": {}}}}
        with patch.dict(os.environ, {"BLOG_MAINTENANCE_DATABASE_URL": "unused-ops-value"}):
            self.assertFalse(tool.has_dedicated_maintenance(model, {}))
        self.assertTrue(tool.has_dedicated_maintenance(model, {"maintenance": {"database_url": "file-value"}}))
        model["services"]["maintenance"]["environment"]["BLOG_MAINTENANCE_DATABASE_URL"] = "shell-value"
        self.assertTrue(tool.has_dedicated_maintenance(model, {}))

    def test_restore_accounts_preserve_shared_separate_and_legacy_modes(self):
        for restricted in (False, True):
            for dedicated in (False, True):
                with self.subTest(restricted=restricted, dedicated=dedicated):
                    values = tool.restore_credentials({"restricted_runtime": restricted,
                                                       "dedicated_maintenance": dedicated}, "blog_restore_test")
                    role = "blog_app" if restricted else "blog_owner"
                    password = values["BLOG_APP_PASSWORD" if restricted else "BLOG_OWNER_PASSWORD"]
                    self.assertEqual(values["DATABASE_URL"], f"postgres://{role}:{password}@db:5432/blog_restore_test")
                    self.assertEqual("BLOG_APP_PASSWORD" in values, restricted)
                    self.assertEqual("BLOG_MAINTENANCE_DATABASE_URL" in values, dedicated)
                    self.assertEqual("BLOG_MAINTENANCE_PASSWORD" in values, dedicated)
                    self.assertRegex(password, r"^[a-f0-9]{64}$")
        legacy = tool.restore_credentials({}, "blog_restore_old")
        self.assertIn("BLOG_APP_PASSWORD", legacy)
        self.assertIn("BLOG_MAINTENANCE_PASSWORD", legacy)
        self.assertNotEqual(legacy["BLOG_OWNER_PASSWORD"], tool.restore_credentials({}, "blog_restore_old")["BLOG_OWNER_PASSWORD"])
        for key in ("restricted_runtime", "dedicated_maintenance"):
            with self.assertRaises(tool.RecoveryError):
                tool.restore_credentials({key: "false"}, "blog_restore_bad")

    def test_missing_public_key_fails_before_reading_deployment_context(self):
        with patch.dict(os.environ, {"BLOG_BACKUP_RECIPIENT": ""}), patch.object(tool, "context") as context:
            with self.assertRaises(tool.RecoveryError):
                tool.preflight()
            context.assert_not_called()

    def test_recovery_material_excludes_unneeded_secrets(self):
        environment = {
            "RESTIC_PASSWORD": "remote-password", "AWS_SECRET_ACCESS_KEY": "aws-key",
            "DATABASE_URL": "postgres://old:secret@db/site", "BLOG_POSTGRES_PASSWORD": "admin-password",
            "GH_SECRET": "unused-secret", "CUSTOM_OAUTH_SECRET": "used-secret",
            "BLOG_DB_MAX_CONNECTIONS": "9", "TZ": "Asia/Shanghai", "UNRELATED_TOKEN": "unrelated",
        }
        self.assertEqual(tool.recovery_environment(environment, ["CUSTOM_OAUTH_SECRET"]), {
            "CUSTOM_OAUTH_SECRET": "used-secret", "BLOG_DB_MAX_CONNECTIONS": "9", "TZ": "Asia/Shanghai",
        })
        config = {"database": {"url": "old-secret", "max_connections": 9},
                  "maintenance": {"database_url": "other-secret"}, "server": {"public_base_url": "https://blog.test"}}
        safe = tool.recovery_config(config)
        self.assertNotIn("old-secret", json.dumps(safe))
        self.assertNotIn("other-secret", json.dumps(safe))
        self.assertEqual(safe["database"], {"max_connections": 9})
        self.assertEqual(config["database"]["url"], "old-secret")

    def test_reserved_or_missing_oauth_reference_is_rejected(self):
        for ref in ("RESTIC_PASSWORD", "AWS_SECRET_ACCESS_KEY", "DATABASE_URL", "BLOG_APP_PASSWORD", "PGPASSWORD", "PATH"):
            with self.subTest(ref=ref), self.assertRaises(tool.RecoveryError):
                tool.recovery_environment({ref: "secret"}, [ref])
        with self.assertRaises(tool.RecoveryError):
            tool.recovery_environment({}, ["CUSTOM_SECRET"])

    def test_transport_detects_damage_missing_marker_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "blog-20260928T000000Z-123456abcdef.tar.gz.age"
            make_transport(path)
            self.assertEqual(tool.verify_transport(path)["site_id"], "a" * 64)
            path.write_bytes(path.read_bytes()[:-1])
            with self.assertRaises(tool.RecoveryError):
                tool.verify_transport(path)
            make_transport(path)
            tool.transport_path(path).unlink()
            with self.assertRaises(tool.RecoveryError):
                tool.verify_transport(path)
            make_transport(path)
            path.unlink()
            path.symlink_to(tool.transport_path(path))
            with self.assertRaises(tool.RecoveryError):
                tool.verify_transport(path)

    def test_sync_uploads_the_pair_without_decryption(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "blog-20260928T000000Z-123456abcdef.tar.gz.age"
            make_transport(path)
            with patch.object(tool, "restic") as restic, patch.object(tool, "unpack", side_effect=AssertionError("needs key")):
                tool.sync(path)
            self.assertEqual(restic.call_args_list[0].args[0][-2:], [str(path), str(tool.transport_path(path))])

    def test_container_environment_distinguishes_unset_and_empty(self):
        self.assertEqual(tool.container_environment(["DATABASE_URL", "EMPTY=", "TOKEN=a=b"]),
                         {"EMPTY": "", "TOKEN": "a=b"})

    def test_prepare_preserves_log_zone_and_legacy_site_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            deployment, target = root / "deployment", root / "target"
            deployment.mkdir()
            target.mkdir()
            (deployment / "environment.json").write_text(json.dumps({
                "BLOG_TIME_ZONE": "Asia/Shanghai", "TZ": "Europe/London", "PATH": "/source/bin",
            }))
            with patch.object(tool, "unpack") as unpack, patch.object(tool, "host_owned"), \
                    patch.dict(os.environ, {"BLOG_BACKUP_TMPFS_SIZE": "4g"}), \
                    patch.object(tool, "Path", side_effect=lambda value: target if value == "/target" else Path(value)):
                unpack.return_value.__enter__.return_value = (
                    root, {"secret_refs": [], "backup_id": "test"}, deployment,
                    {"image_id": "blog:test", "ops_image": "blog-ops:test", "recipient": "age1example"},
                )
                tool.prepare("backup.tar.gz")
            env = (target / ".env").read_text()
            self.assertIn('BLOG_TIME_ZONE="Asia/Shanghai"', env)
            self.assertIn('TZ="Europe/London"', env)
            self.assertIn('BLOG_BACKUP_TMPFS_SIZE="4g"', env)
            self.assertNotIn("PATH=", env)

    def test_rejects_unsafe_tar_before_writing_any_member(self):
        for name, kind in (("backup/../escape", tarfile.REGTYPE),
                           ("/escape", tarfile.REGTYPE),
                           ("backup/link", tarfile.SYMTYPE),
                           ("backup/fifo", tarfile.FIFOTYPE),
                           ("backup/a", tarfile.REGTYPE)):
            with self.subTest(name=name, kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive = root / "bad.tar.gz"
                with tarfile.open(archive, "w:gz") as stream:
                    first = tarfile.TarInfo("backup/a")
                    first.size = 1
                    stream.addfile(first, io.BytesIO(b"a"))
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "../../escape"
                    stream.addfile(member)
                output = root / "out"
                output.mkdir()
                with self.assertRaises(tool.RecoveryError):
                    tool.safe_extract(archive, output)
                self.assertEqual(list(output.iterdir()), [])

    def test_extracts_files_without_inheriting_archive_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "valid.tar.gz"
            with tarfile.open(archive, "w:gz") as stream:
                member = tarfile.TarInfo("backup/data/file")
                member.mode = 0o777
                member.size = 5
                stream.addfile(member, io.BytesIO(b"hello"))
            output = root / "out"
            output.mkdir()
            tool.safe_extract(archive, output)
            result = output / "backup/data/file"
            self.assertEqual(result.read_bytes(), b"hello")
            self.assertEqual(result.stat().st_mode & 0o777, 0o600)

    def test_remote_failure_preserves_all_local_archives(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = [f"blog-20260928T00000{i}Z-123456abcdef.tar.gz.age" for i in range(3)]
            for name in names:
                make_transport(root / name)
            with patch.object(tool, "BACKUPS", root), patch.dict(os.environ, {
                "RESTIC_REPOSITORY": "/remote", "BLOG_BACKUP_KEEP": "1",
            }), patch.object(tool, "sync", side_effect=tool.RecoveryError("remote failed")):
                with self.assertRaises(tool.RecoveryError):
                    tool.finalize(names[-1])
            self.assertEqual(sorted(p.name for p in root.iterdir()), sorted(names + [name + ".json" for name in names]))

    def test_retention_keeps_unrelated_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "blog-20260928T000000Z-123456abcdef.tar.gz.age"
            new = root / "blog-20260928T000001Z-123456abcdef.tar.gz.age"
            for path in (old, new, root / "manual.tar.gz"):
                make_transport(path)
            link = root / "blog-20260928T000002Z-123456abcdef.tar.gz.age"
            link.symlink_to(root / "manual.tar.gz")
            with patch.object(tool, "BACKUPS", root), patch.dict(os.environ, {
                "RESTIC_REPOSITORY": "", "BLOG_BACKUP_KEEP": "1",
            }), patch("sys.stdout", new=io.StringIO()):
                tool.finalize(new.name)
            self.assertFalse(old.exists())
            self.assertFalse(tool.transport_path(old).exists())
            self.assertTrue(new.exists())
            self.assertTrue(link.is_symlink())
            self.assertTrue((root / "manual.tar.gz").exists())

    def test_orphan_ciphertext_and_legacy_backups_are_not_pruned(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "blog-20260928T000000Z-123456abcdef.tar.gz.age"
            new = root / "blog-20260928T000001Z-123456abcdef.tar.gz.age"
            old.write_bytes(b"interrupted-before-marker")
            legacy = root / "blog-20260928T000000Z-123456abcdef.tar.gz"
            legacy.write_bytes(b"legacy")
            make_transport(new)
            with patch.object(tool, "BACKUPS", root), patch.dict(os.environ, {"RESTIC_REPOSITORY": "", "BLOG_BACKUP_KEEP": "1"}):
                tool.finalize(new.name)
            self.assertTrue(old.exists())
            self.assertTrue(legacy.exists())

    def test_dotenv_validates_keys_and_preserves_secret_characters(self):
        result = tool.dotenv({"SECRET": "a'b\\c$VALUE\nnext"})
        self.assertEqual(json.loads(result.split("=", 1)[1]).replace("$$", "$"), "a'b\\c$VALUE\nnext")
        with self.assertRaises(tool.RecoveryError):
            tool.dotenv({"BAD\nKEY": "value"})


@unittest.skipUnless(shutil.which("age") and shutil.which("age-keygen"), "age tools required; also exercised in the ops image")
class EncryptionTests(unittest.TestCase):
    def test_real_encryption_decryption_and_authentication(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            deployment = source / "data/resources/deployment"
            deployment.mkdir(parents=True)
            (source / "sensitive.txt").write_text("a-private-post-and-oauth-secret")
            metadata = {"format": 2, "site_id": "a" * 64, "binary_sha256": "binary-test"}
            (deployment / "compose.json").write_text(json.dumps(metadata))
            key, wrong = root / "key", root / "wrong"
            for path in (key, wrong):
                subprocess.run(["age-keygen", "-o", str(path)], check=True, capture_output=True)
            public = subprocess.check_output(["age-keygen", "-y", str(key)], text=True).strip()
            with patch.dict(os.environ, {"BLOG_BACKUP_RECIPIENT": public}):
                self.assertEqual(tool.recipient(), public)
            with patch.dict(os.environ, {"BLOG_BACKUP_RECIPIENT": "age1invalid"}):
                with self.assertRaises(tool.RecoveryError):
                    tool.recipient()
            archive = root / "blog-20260928T000000Z-123456abcdef.tar.gz.age"
            tool.encrypt_archive(source, archive, public)
            encrypted = archive.read_bytes()
            self.assertNotIn(b"a-private-post-and-oauth-secret", encrypted)
            make_transport(archive, encrypted)
            with patch.object(tool, "IDENTITY", key), patch.object(tool, "SCRATCH", root), \
                    patch.object(tool, "binary_hash", return_value="binary-test"), \
                    patch.object(tool.recovery, "verify", return_value={}):
                with tool.unpack(archive) as (opened, _, _, _):
                    self.assertEqual((opened / "sensitive.txt").read_text(), "a-private-post-and-oauth-secret")
            for identity, content in ((wrong, encrypted), (key, encrypted[:-1]), (key, encrypted[:-1] + bytes([encrypted[-1] ^ 1]))):
                make_transport(archive, content)  # Recomputing the public checksum cannot bypass age authentication.
                with patch.object(tool, "IDENTITY", identity), patch.object(tool, "SCRATCH", root), \
                        patch.object(tool.recovery, "verify") as verify:
                    with self.assertRaises(tool.RecoveryError):
                        with tool.unpack(archive):
                            self.fail("invalid ciphertext accepted")
                    verify.assert_not_called()
            self.assertEqual(list(root.glob("blog-verify-*")), [])


class InterruptedBackupTests(unittest.TestCase):
    def test_interruption_stops_operation_before_restart_or_keeps_source_stopped(self):
        for stop_fails in (False, True):
            with self.subTest(stop_fails=stop_fails), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "scripts").mkdir()
                (root / "bin").mkdir()
                (root / ".env").write_text("BLOG_POSTGRES_PASSWORD=test\n")
                shutil.copyfile(Path(__file__).parent / "compose-backup.sh", root / "scripts/compose-backup.sh")
                fake = root / "bin/docker"
                fake.write_text(f"#!{sys.executable}\n" + '''
import json, os, pathlib, signal, sys
root = pathlib.Path(os.environ['FAKE_DOCKER_ROOT'])
args = sys.argv[1:]
active = root / 'active-operation'
log = root / 'events'
def event(value):
    with log.open('a') as stream: stream.write(value + '\\n')
if args[0] == 'inspect':
    if '--format' in args: print('{}'); sys.exit(0)
    sys.exit(0 if active.exists() else 1)
if args[0] == 'stop':
    event('stop-operation')
    if os.environ.get('STOP_FAILS') == '1': sys.exit(1)
    active.unlink(); sys.exit(0)
if args[0] == 'compose':
    args = args[3:]  # --project-directory PATH
    if 'ps' in args: print('test-blog'); sys.exit(0)
    if 'config' in args: print('{}'); sys.exit(0)
    if 'exec' in args: print('hash  /usr/local/bin/blog'); sys.exit(0)
    if args[0] == 'stop': event('stop-blog'); sys.exit(0)
    if args[0] == 'start':
        event('start-blog')
        sys.exit(99 if active.exists() else 0)
    if args[0] == 'run':
        if args[-1] == 'preflight': sys.exit(0)
        if args[-1] == 'backup':
            active.write_text('running')
            os.kill(os.getppid(), signal.SIGTERM)
            sys.exit(143)
sys.exit(2)
''')
                fake.chmod(0o700)
                result = subprocess.run(["sh", str(root / "scripts/compose-backup.sh"), "backup"],
                                        capture_output=True, timeout=15,
                                        env={**os.environ, "PATH": str(root / "bin") + ":" + os.environ["PATH"],
                                             "FAKE_DOCKER_ROOT": str(root), "STOP_FAILS": "1" if stop_fails else "0"})
                self.assertNotEqual(result.returncode, 0)
                events = (root / "events").read_text().splitlines()
                self.assertEqual(events, ["stop-blog", "stop-operation"] + ([] if stop_fails else ["start-blog"]))
                self.assertEqual((root / "backups/.operation-lock").exists(), stop_fails)
                self.assertEqual(json.loads((root / "backups/status.json").read_text())["status"], "failed")


if __name__ == "__main__":
    unittest.main()
