"""Contract checks and temporary next-release fixtures; never edit shipped SQL."""
import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import schema_contract as schema


def release_fixture(root, extension=False):
    """Snapshot the real tools/chain, optionally append a test-only table."""
    root = Path(root)
    shutil.copytree(schema.ROOT / "migrations", root / "migrations")
    shutil.copytree(schema.ROOT / "scripts", root / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
    (root / "docs/sql").mkdir(parents=True)
    directory = root / "migrations/postgres"
    contract = schema.load_contract(directory)
    if extension:
        version = contract["migrations"][-1]["version"] + 1
        (directory / f"{version:04d}_schema_drill.sql").write_text(
            "CREATE TABLE schema_drill (id uuid PRIMARY KEY, note text NOT NULL);\n")
        contract["id"] = "blog-schema-drill"
        contract["tables"]["schema_drill"] = {"app": ["SELECT", "INSERT", "UPDATE", "DELETE"], "maintenance": []}
        (directory / "schema.json").write_text(json.dumps(contract))
        contract = schema.append_migrations(directory)
        (directory / "schema.json").write_text(json.dumps(contract))
    for path, value in schema.generated_files(root, contract).items():
        path.write_text(value)
    return contract


class SchemaContractTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="blog-schema-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.contract = release_fixture(self.root)
        self.directory = self.root / "migrations/postgres"

    def write(self, contract):
        (self.directory / "schema.json").write_text(json.dumps(contract))

    def test_existing_migration_cannot_be_rewritten_or_deleted(self):
        path = self.directory / self.contract["migrations"][0]["file"]
        original = path.read_bytes()
        for replacement in (original + b"\n-- changed\n", None):
            if replacement is None:
                path.unlink()
            else:
                path.write_bytes(replacement)
            with self.assertRaises(schema.SchemaError):
                schema.append_migrations(self.directory)
            with self.assertRaises(schema.SchemaError):
                schema.load_contract(self.directory)

    def test_base_commit_catches_rewritten_sql_even_with_recomputed_hashes(self):
        def git(*args):
            return subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True, text=True).stdout.strip()
        git("init")
        git("add", "migrations")
        git("-c", "user.name=Schema test", "-c", "user.email=schema@example.test", "commit", "-m", "baseline")
        base = git("rev-parse", "HEAD")
        schema.verify_history(base, self.root)
        path = self.directory / self.contract["migrations"][0]["file"]
        path.write_bytes(path.read_bytes() + b"\n-- illegal edit\n")
        self.contract["migrations"] = schema.migration_inventory(self.directory)
        self.write(self.contract)
        schema.load_contract(self.directory)
        with self.assertRaisesRegex(schema.SchemaError, "changed or removed"):
            schema.verify_history(base, self.root)

    def test_new_table_requires_explicit_policies_for_both_roles(self):
        for policy in ({"app": ["SELECT"]}, {"app": ["ALL"], "maintenance": []},
                       {"app": ["SELECT(id); DROP TABLE users"], "maintenance": []}):
            contract = copy.deepcopy(self.contract)
            contract["tables"]["schema_drill"] = policy
            self.write(contract)
            with self.assertRaises(schema.SchemaError):
                schema.load_contract(self.directory)

    def test_duplicate_keys_and_bad_history_are_rejected(self):
        (self.directory / "schema.json").write_text('{"format":1,"format":1}')
        with self.assertRaisesRegex(schema.SchemaError, "duplicate"):
            schema.load_contract(self.directory)
        contract = copy.deepcopy(self.contract)
        contract["migrations"].append(contract["migrations"][0])
        self.write(contract)
        with self.assertRaises(schema.SchemaError):
            schema.load_contract(self.directory)

    def test_appended_migration_updates_all_generated_outputs(self):
        extended = self.root / "next"
        contract = release_fixture(extended, extension=True)
        self.assertEqual(contract["migrations"][:-1], self.contract["migrations"])
        self.assertEqual(len(contract["tables"]), len(self.contract["tables"]) + 1)
        result = subprocess.run(["python3", "-B", str(extended / "scripts/schema_contract.py")], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        grant = extended / "scripts/database-roles.sql"
        self.assertIn('ON public."schema_drill" TO :"app_role"', grant.read_text())
        grant.write_text(grant.read_text() + "-- hand edit\n")
        result = subprocess.run(["python3", "-B", str(extended / "scripts/schema_contract.py")], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stale generated file", result.stderr)


if __name__ == "__main__":
    unittest.main()
