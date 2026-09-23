import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import recovery


class RecoveryTests(unittest.TestCase):
    def bundle(self, root):
        data = root / "data"
        (data / "theme" / "templates").mkdir(parents=True, exist_ok=True)
        for name in ("base.html", "index.html", "post.html", "page.html"):
            (data / "theme" / "templates" / name).write_text(name)
        dump = data / "database.dump"
        dump.write_bytes(b"PGDMPfake")
        manifest = {
            "format": 1, "backup_id": "test", "secret_refs": [],
            "source_database": "blog", "schema_version": 3,
            "database_counts": {}, "files": recovery.file_records(data),
        }
        raw = json.dumps(manifest).encode()
        (root / "manifest.json").write_bytes(raw)
        (root / "COMPLETE").write_text(hashlib.sha256(raw).hexdigest())
        return dump

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

    def test_database_names_are_restricted(self):
        for url in ("sqlite:///blog", "postgres://localhost/bad-name", "postgres://localhost/"):
            with self.assertRaises(recovery.RecoveryError):
                recovery.db_config(url)
        self.assertEqual(recovery.db_config("postgres://user:pass@localhost/blog")["PGDATABASE"], "blog")


if __name__ == "__main__":
    unittest.main()
