"""Shared schema contract and reproducible SQL artifacts.

The SQL migration chain is authoritative. schema.json records immutable hashes
and explicit grants for every table; it is not a second DDL implementation.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
DIRECTORY = ROOT / "migrations/postgres"
IDENTIFIER = re.compile(r"^[a-z][a-z0-9_]*$")
MIGRATION = re.compile(r"^([0-9]+)_[a-z][a-z0-9_]*\.sql$")
PRIVILEGES = ("SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN")
COLUMN_PRIVILEGES = ("SELECT", "INSERT", "UPDATE", "REFERENCES")


class SchemaError(Exception):
    pass


def unique_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise SchemaError(f"duplicate schema key: {key}")
        result[key] = value
    return result


def parse_grant(value):
    if not isinstance(value, str):
        raise SchemaError("grant must be a string")
    match = re.fullmatch(r"([A-Z]+)(?:\(([a-z0-9_,]+)\))?", value)
    if not match or match[1] not in PRIVILEGES:
        raise SchemaError(f"invalid grant: {value}")
    columns = match[2].split(",") if match[2] else []
    if columns and (match[1] not in COLUMN_PRIVILEGES or len(set(columns)) != len(columns)
                    or any(not IDENTIFIER.fullmatch(name) for name in columns)):
        raise SchemaError(f"invalid column grant: {value}")
    return match[1], columns


def migration_inventory(directory):
    records = []
    for path in Path(directory).glob("*.sql"):
        match = MIGRATION.fullmatch(path.name)
        if not match or path.is_symlink() or not path.is_file():
            raise SchemaError(f"unsupported migration file: {path.name}; use forward-only NNNN_description.sql")
        records.append({"version": int(match[1]), "file": path.name,
                        "checksum": hashlib.sha384(path.read_bytes()).hexdigest()})
    records.sort(key=lambda item: item["version"])
    versions = [item["version"] for item in records]
    if not versions or versions[0] <= 0 or len(set(versions)) != len(versions):
        raise SchemaError("migration versions must be positive and unique")
    return records


def load_contract(directory=None, verify_files=True):
    directory = Path(directory or DIRECTORY)
    try:
        contract = json.loads((directory / "schema.json").read_text(), object_pairs_hook=unique_keys)
        if (not isinstance(contract, dict) or set(contract) != {"format", "id", "migrations", "tables"}
                or type(contract["format"]) is not int or contract["format"] != 1):
            raise SchemaError("unsupported schema contract format")
        if not isinstance(contract["id"], str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", contract["id"]):
            raise SchemaError("invalid schema identity")
        tables = contract["tables"]
        if not isinstance(tables, dict) or not tables:
            raise SchemaError("schema must declare tables and their privileges")
        for table, roles in tables.items():
            if (not IDENTIFIER.fullmatch(table) or table == "_sqlx_migrations"
                    or not isinstance(roles, dict) or set(roles) != {"app", "maintenance"}):
                raise SchemaError(f"invalid table or missing explicit role policy: {table}")
            for grants in roles.values():
                if not isinstance(grants, list) or any(not isinstance(item, str) for item in grants) or len(grants) != len(set(grants)):
                    raise SchemaError(f"invalid grants for {table}")
                for grant in grants:
                    parse_grant(grant)
        previous = 0
        if not isinstance(contract["migrations"], list) or not contract["migrations"]:
            raise SchemaError("missing migration history")
        for record in contract["migrations"]:
            if not isinstance(record, dict) or set(record) != {"version", "file", "checksum"}:
                raise SchemaError("invalid migration record")
            match = MIGRATION.fullmatch(record["file"])
            if (type(record["version"]) is not int or record["version"] <= previous
                    or not match or int(match[1]) != record["version"]
                    or not re.fullmatch(r"[0-9a-f]{96}", record["checksum"])):
                raise SchemaError("invalid migration version, filename or checksum")
            previous = record["version"]
        if verify_files and contract["migrations"] != migration_inventory(directory):
            raise SchemaError("migration files/checksums differ from schema.json; append new migrations and run schema_contract.py --write")
        return contract
    except (OSError, ValueError, TypeError, KeyError) as error:
        raise SchemaError("cannot read a valid schema.json contract") from error


def expected_migrations(directory=None):
    return [{"version": item["version"], "checksum": item["checksum"]}
            for item in load_contract(directory)["migrations"]]


def append_migrations(directory):
    contract = load_contract(directory, verify_files=False)
    actual = migration_inventory(directory)
    old = contract["migrations"]
    if actual[:len(old)] != old:
        raise SchemaError("existing migrations are immutable; add a new migration instead")
    contract["migrations"] = actual
    return contract


def verify_history(base_ref, root=ROOT):
    """Protect SQL already on the base branch, including before schema.json existed."""
    if not base_ref or set(base_ref) == {"0"}:
        return
    if not re.fullmatch(r"[0-9a-f]{40,64}", base_ref):
        raise SchemaError("base ref must be a full Git commit hash")
    listed = subprocess.run(["git", "ls-tree", "-r", "--name-only", base_ref, "--", "migrations/postgres"],
                            cwd=root, capture_output=True, check=True, text=True).stdout.splitlines()
    for name in listed:
        if not name.endswith(".sql"):
            continue
        original = subprocess.run(["git", "show", f"{base_ref}:{name}"], cwd=root,
                                  capture_output=True, check=True).stdout
        path = Path(root) / name
        if not path.is_file() or path.read_bytes() != original:
            raise SchemaError(f"migration changed or removed since base commit: {name}")


def roles_sql(contract, template):
    tables = sorted(contract["tables"])
    expected = ",".join(f"'{name}'" for name in tables)
    guard = f"""-- Refuse an incomplete contract before changing any privileges.
DO $$
BEGIN
  IF (SELECT array_agg(table_name::text ORDER BY table_name)
      FROM information_schema.tables
      WHERE table_schema='public' AND table_type='BASE TABLE'
        AND table_name<>'_sqlx_migrations') IS DISTINCT FROM ARRAY[{expected}]::text[] THEN
    RAISE EXCEPTION 'Database tables differ from schema.json; use the matching release';
  END IF;
END $$;"""
    grants = ["GRANT SELECT ON public._sqlx_migrations TO :\"app_role\";"]
    for table in tables:
        for role in ("app", "maintenance"):
            policy = contract["tables"][table][role]
            if policy:
                grants.append(f'GRANT {",".join(policy)} ON public."{table}" TO :"{role}_role";')
    return ("-- Generated by scripts/schema_contract.py --write; do not edit.\n"
            + template.replace("-- @schema-table-check", guard).replace("-- @schema-grants", "\n".join(grants)))


def generated_files(root, contract):
    root = Path(root)
    directory = root / "migrations/postgres"
    parts = ["-- Generated by scripts/schema_contract.py --write; do not edit.\n"
             "-- Forward migrations concatenated for empty-database reference only.\n"
             "-- Deploy using blog migrate; this file does not create SQLx history.\n\nBEGIN;\n"]
    for record in contract["migrations"]:
        parts.append(f'\n-- Migration: {record["file"]}\n' + (directory / record["file"]).read_text().rstrip() + "\n")
    parts.append("\nCOMMIT;\n")
    ddl = "".join(parts)
    return {root / "docs/sql/postgres-core.sql": ddl,
            root / "scripts/database-roles.sql": roles_sql(contract, (root / "scripts/sql/database-roles.sql.in").read_text())}


def verify_database(pg, contract, database=None, app_role=None, maintenance_role=None):
    """Compare real PostgreSQL tables and effective grants, including column grants."""
    actual = pg.query("SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_type='BASE TABLE' AND table_name<>'_sqlx_migrations' ORDER BY table_name", database).splitlines()
    if set(actual) != set(contract["tables"]):
        raise SchemaError("database table set differs from schema.json")
    columns = json.loads(pg.query("SELECT jsonb_object_agg(table_name,columns) FROM (SELECT table_name,jsonb_agg(column_name ORDER BY ordinal_position) AS columns FROM information_schema.columns WHERE table_schema='public' GROUP BY table_name) c", database))
    for role_kind, role in (("app", app_role), ("maintenance", maintenance_role)):
        if role is None:
            continue
        if not IDENTIFIER.fullmatch(role):
            raise SchemaError("invalid verification role")
        checks = []
        policies = {**contract["tables"], "_sqlx_migrations": {"app": ["SELECT"], "maintenance": []}}
        for table, policy in policies.items():
            allowed = set()
            allowed_columns = set()
            for grant in policy[role_kind]:
                privilege, names = parse_grant(grant)
                if names:
                    if not set(names).issubset(columns[table]):
                        raise SchemaError(f"grant references missing columns in {table}")
                    allowed_columns.update((name, privilege) for name in names)
                else:
                    allowed.add(privilege)
            for privilege in PRIVILEGES:
                expected = str(privilege in allowed).lower()
                checks.append(f"has_table_privilege('{role}','public.{table}','{privilege}') = {expected}")
            for column in columns[table]:
                literal = column.replace("'", "''")
                for privilege in COLUMN_PRIVILEGES:
                    expected = str(privilege in allowed or (column, privilege) in allowed_columns).lower()
                    checks.append(f"has_column_privilege('{role}','public.{table}','{literal}','{privilege}') = {expected}")
        # One boolean per role avoids dumping credentials or large grant inventories.
        if pg.query("SELECT " + " AND ".join(checks), database) != "t":
            raise SchemaError(f"effective {role_kind} privileges differ from schema.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="append new migration hashes and regenerate SQL references/grants")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base-ref", help="full Git commit hash whose migrations must remain unchanged")
    args = parser.parse_args()
    try:
        verify_history(args.base_ref, args.root)
        directory = args.root / "migrations/postgres"
        contract = append_migrations(directory) if args.write else load_contract(directory)
        outputs = generated_files(args.root, contract)
        if args.write:
            (directory / "schema.json").write_text(json.dumps(contract, indent=2) + "\n")
            for path, value in outputs.items():
                path.write_text(value)
        else:
            for path, value in outputs.items():
                if not path.is_file() or path.read_text() != value:
                    raise SchemaError(f"stale generated file: {path.relative_to(args.root)}; run schema_contract.py --write")
        print("Schema contract and generated SQL are consistent.")
    except (SchemaError, OSError, subprocess.SubprocessError) as error:
        print(f"schema: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
