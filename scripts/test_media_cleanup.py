import argparse
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid

import media_cleanup as cleanup
from recovery_inventory import RecoveryError


class FakePg:
    config = {"PGHOST": "localhost", "PGPORT": "5432"}
    container = None

    def __init__(self):
        self.committed = False
        self.commits = 0
        self.on_commit = lambda: None

    def query(self, sql):
        if "FROM pg_database" in sql:
            return '{"name":"test","oid":"123","guard":null}'
        if sql.startswith("BEGIN;"):
            self.on_commit()
            self.committed = True
            self.commits += 1
            return "COMMIT"
        return "t" if self.committed else "f"


class MediaCleanupTests(unittest.TestCase):
    def fixture(self, root):
        data = b"media bytes"
        file = root / "objects" / "image.png"
        file.parent.mkdir()
        file.write_bytes(data)
        item = {"id":str(uuid.uuid4()),"path":"objects/image.png","size":len(data),
                "sha256":hashlib.sha256(data).hexdigest(),"version":2,"deleted_at":"2026-01-01T00:00:00.000000Z"}
        pg = FakePg()
        plan = {"format":1,"operation_id":str(uuid.uuid4()),"database":cleanup.fingerprint(pg),
                "media_root":str(root),"items":[item]}
        path = root / "plan.json"
        self.write_plan(path,plan)
        args = argparse.Namespace(plan=path,maintenance_confirmed=True,break_links_confirmed=True)
        return pg,args,plan,file

    def write_plan(self, path, plan):
        path.write_text(json.dumps({"plan":plan,"sha256":cleanup.plan_digest(plan)}))

    def test_missing_or_changed_object_prevents_any_database_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            pg,args,_,file = self.fixture(Path(directory))
            file.write_bytes(b"changed")
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            file.unlink()
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            self.assertEqual(pg.commits,0)

    def test_failed_unlink_can_retry_only_with_the_same_committed_plan(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); pg,args,plan,file=self.fixture(root)
            extra=root / "objects/keep.png"; extra.write_bytes(b"unregistered")
            with patch.object(Path,"unlink",side_effect=PermissionError("disk is read only")):
                failed=cleanup.apply_plan(args,pg)
            self.assertEqual(len(failed["failures"]),1)
            self.assertTrue(file.exists()); self.assertTrue(pg.committed)
            result=cleanup.apply_plan(args,pg)
            self.assertEqual(result["files_deleted"],1)
            self.assertFalse(file.exists()); self.assertTrue(extra.exists())
            self.assertEqual(cleanup.apply_plan(args,pg)["files_already_absent"],1)
            pg.committed=False
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)

    def test_file_replaced_after_database_commit_is_not_unlinked(self):
        with tempfile.TemporaryDirectory() as directory:
            pg,args,_,file=self.fixture(Path(directory))
            pg.on_commit=lambda:file.write_bytes(b"new data")
            result=cleanup.apply_plan(args,pg)
            self.assertEqual(len(result["failures"]),1)
            self.assertEqual(file.read_bytes(),b"new data")

    def test_database_failure_preserves_formal_file(self):
        with tempfile.TemporaryDirectory() as directory:
            pg,args,_,file=self.fixture(Path(directory))
            def fail(): raise RecoveryError("transaction failed")
            pg.on_commit=fail
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            self.assertTrue(file.exists())

    def test_changed_plan_path_escape_and_symlinks_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory); pg,args,plan,file=self.fixture(root)
            document=json.loads(args.plan.read_text()); document["plan"]["items"][0]["version"]=3
            args.plan.write_text(json.dumps(document))
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            self.write_plan(args.plan,plan)
            file.unlink(); file.symlink_to(args.plan)
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            plan["items"][0]["path"]="../outside"
            self.write_plan(args.plan,plan)
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            self.assertEqual(pg.commits,0)

    def test_confirmations_and_database_identity_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            pg,args,plan,_=self.fixture(Path(directory))
            args.break_links_confirmed=False
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            args.break_links_confirmed=True
            plan["database"]["oid"]="different"
            self.write_plan(args.plan,plan)
            with self.assertRaises(RecoveryError): cleanup.apply_plan(args,pg)
            self.assertEqual(pg.commits,0)


if __name__ == "__main__":
    unittest.main()
