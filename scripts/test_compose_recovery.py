"""Container-independent checks for archive trust boundaries and retention failures."""
import io
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


class ArchiveTests(unittest.TestCase):
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
                    patch.object(tool, "Path", side_effect=lambda value: target if value == "/target" else Path(value)):
                unpack.return_value.__enter__.return_value = (
                    root, {"secret_refs": [], "backup_id": "test"}, deployment,
                    {"image_id": "blog:test", "ops_image": "blog-ops:test"},
                )
                tool.prepare("backup.tar.gz")
            env = (target / ".env").read_text()
            self.assertIn('BLOG_TIME_ZONE="Asia/Shanghai"', env)
            self.assertIn('TZ="Europe/London"', env)
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
            names = [f"blog-20260928T00000{i}Z-123456abcdef.tar.gz" for i in range(3)]
            for name in names:
                (root / name).write_bytes(b"backup")
            with patch.object(tool, "BACKUPS", root), patch.dict(os.environ, {
                "RESTIC_REPOSITORY": "/remote", "BLOG_BACKUP_KEEP": "1",
            }), patch.object(tool, "sync", side_effect=tool.RecoveryError("remote failed")):
                with self.assertRaises(tool.RecoveryError):
                    tool.finalize(names[-1])
            self.assertEqual(sorted(p.name for p in root.iterdir()), names)

    def test_retention_keeps_unrelated_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = root / "blog-20260928T000000Z-123456abcdef.tar.gz"
            new = root / "blog-20260928T000001Z-123456abcdef.tar.gz"
            for path in (old, new, root / "manual.tar.gz"):
                path.write_bytes(b"backup")
            link = root / "blog-20260928T000002Z-123456abcdef.tar.gz"
            link.symlink_to(root / "manual.tar.gz")
            with patch.object(tool, "BACKUPS", root), patch.dict(os.environ, {
                "RESTIC_REPOSITORY": "", "BLOG_BACKUP_KEEP": "1",
            }), patch("sys.stdout", new=io.StringIO()):
                tool.finalize(new.name)
            self.assertFalse(old.exists())
            self.assertTrue(new.exists())
            self.assertTrue(link.is_symlink())
            self.assertTrue((root / "manual.tar.gz").exists())

    def test_dotenv_validates_keys_and_preserves_secret_characters(self):
        result = tool.dotenv({"SECRET": "a'b\\c$VALUE\nnext"})
        self.assertEqual(json.loads(result.split("=", 1)[1]).replace("$$", "$"), "a'b\\c$VALUE\nnext")
        with self.assertRaises(tool.RecoveryError):
            tool.dotenv({"BAD\nKEY": "value"})


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
