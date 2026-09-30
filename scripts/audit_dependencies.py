#!/usr/bin/env python3
"""Audit the full Cargo lockfile, retaining evidence for one unreachable advisory.

No advisory is passed to cargo-audit's --ignore option. The exception below is
bound to one package/version and fails as soon as RSA or MySQL becomes reachable
on any platform, or RustSec publishes a patched version. See docs/development.md.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
RSA_CHECKSUM = "b8573f03f5883dcaebdfcf4725caa1ecb9c15b2ef50c43a07b816e06799bb12d"


def graph_packages(tree: str) -> set[tuple[str, str]]:
    """Parse cargo tree's explicit flat package format, failing on unknown output."""
    packages = set()
    for line in tree.splitlines():
        # Cargo separates workspace roots with blank lines, even with --prefix none.
        if not line.strip():
            continue
        match = re.fullmatch(r"([A-Za-z0-9_-]+) v([^\s]+)(?: .*)?", line)
        if not match:
            raise ValueError(f"Unrecognized cargo tree line: {line!r}")
        packages.add((match[1], match[2]))
    if not packages:
        raise ValueError("The all-platform workspace dependency graph is empty")
    return packages


def findings(report: dict) -> list[dict]:
    """Validate the report before making any policy decision (fail closed)."""
    if report["settings"]["ignore"]:
        raise ValueError("cargo-audit must run without ignored advisories")
    vulnerabilities = report["vulnerabilities"]
    result = list(vulnerabilities["list"])
    if vulnerabilities["count"] != len(result) or vulnerabilities["found"] != bool(result):
        raise ValueError("Inconsistent cargo-audit vulnerability count")
    for warnings in report["warnings"].values():
        result.extend(warnings)
    return result


def explained_unreachable_rsa(finding: dict, packages: set[tuple[str, str]]) -> bool:
    package = finding["package"]
    return (
        finding["advisory"]["id"] == "RUSTSEC-2023-0071"
        and finding["advisory"]["package"] == "rsa"
        and package["name"] == "rsa"
        and package["version"] == "0.9.10"
        and package["source"] == CRATES_IO
        and package["checksum"] == RSA_CHECKSUM
        and not finding["versions"]["patched"]
        and not any(name in {"rsa", "sqlx-mysql"} for name, _ in packages)
    )


def evaluate(report: dict, tree: str) -> tuple[list[dict], list[dict]]:
    packages = graph_packages(tree)
    blocked, explained = [], []
    for finding in findings(report):
        if explained_unreachable_rsa(finding, packages):
            explained.append(finding)
        else:
            blocked.append(finding)
    return blocked, explained


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cargo-audit", default="cargo-audit", help="Path to cargo-audit 0.22.2")
    parser.add_argument("--advisory-db", type=Path, required=True, help="Already downloaded RustSec Git database")
    parser.add_argument("--report-dir", type=Path, help="Directory for the original report and graph evidence")
    parser.add_argument("--offline", action="store_true", help="Require every platform dependency to be cached")
    args = parser.parse_args()
    report_dir = args.report_dir or Path(tempfile.mkdtemp(prefix="blog-dependency-audit-"))
    report_dir.mkdir(parents=True, exist_ok=True)
    print(f"Dependency audit evidence: {report_dir}", flush=True)

    tree_command = [
        "cargo", "tree", "--locked", "--workspace", "--all-features", "--target", "all",
        "--edges", "normal,build,dev", "--prefix", "none", "--format", "{p} {f}",
    ]
    if args.offline:
        tree_command.append("--offline")
    tree = subprocess.run(tree_command, cwd=ROOT, check=True, text=True, capture_output=True)
    (report_dir / "cargo-tree-all.txt").write_text(tree.stdout, encoding="utf-8")
    # Parse before auditing, so missing/invalid graph evidence can never authorize
    # a lockfile exception. Target "all" includes the Linux production build.
    graph_packages(tree.stdout)
    commit = subprocess.run(
        ["git", "-C", str(args.advisory_db), "rev-parse", "HEAD"],
        check=True, text=True, capture_output=True,
    )
    (report_dir / "rustsec-commit.txt").write_text(commit.stdout, encoding="utf-8")
    audit = subprocess.run(
        [args.cargo_audit, "audit", "--db", str(args.advisory_db), "--no-fetch", "--no-yanked",
         "--deny", "warnings", "--json"],
        cwd=ROOT, text=True, capture_output=True,
    )
    (report_dir / "cargo-audit.json").write_text(audit.stdout, encoding="utf-8")
    (report_dir / "cargo-audit.stderr").write_text(audit.stderr, encoding="utf-8")
    if audit.returncode not in {0, 1}:
        raise ValueError(f"cargo-audit failed with exit code {audit.returncode}: {audit.stderr}")
    report = json.loads(audit.stdout)
    blocked, explained = evaluate(report, tree.stdout)
    if audit.returncode and not blocked and not explained:
        raise ValueError(f"cargo-audit failed without advisory findings: {audit.stderr}")
    for finding in explained:
        print("RUSTSEC-2023-0071: rsa 0.9.10 retained in Cargo.lock by optional SQLx MySQL; "
              "rsa and sqlx-mysql are absent from the all-platform/all-workspace-feature graph. "
              "RustSec reports no patched version. Full finding retained in cargo-audit.json.")
    for finding in blocked:
        print(f"Blocked: {finding['advisory']['id']} "
              f"({finding['package']['name']} {finding['package']['version']})", file=sys.stderr)
    print(f"Audited {report['lockfile']['dependency-count']} lockfile entries; "
          f"{len(blocked)} blocking findings, {len(explained)} explained unreachable finding(s).")
    return int(bool(blocked))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"Dependency audit failed: {error}", file=sys.stderr)
        sys.exit(1)
