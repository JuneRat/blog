#!/usr/bin/env python3
"""Check direct Cargo dependency boundaries using unfiltered Cargo metadata.

Normal and build dependencies obey production rules. Dev dependencies have their
own reviewed policy; cfg-target and optional dependencies are checked even when
inactive on this machine. Transitive dependencies and feature behavior still need
review; this check does not claim to prove full isolation.
"""

import json
from pathlib import Path
import subprocess
import sys


PRODUCTION_INTERNAL = {
    "domain": frozenset(),
    "application": frozenset({"domain"}),
    "infrastructure": frozenset({"application", "domain"}),
    "interfaces": frozenset({"application"}),
    "server": frozenset({"application", "infrastructure", "interfaces"}),
}

# Integration tests in server assemble fixtures with domain types. This exception
# must not make domain a normal/build dependency of the composition root.
TEST_INTERNAL = {
    **PRODUCTION_INTERNAL,
    "server": PRODUCTION_INTERNAL["server"] | {"domain"},
}

# Inner layers accept a small, reviewed set of value types, DTO and async-trait
# support. New libraries require a deliberate policy change alongside their use.
PRODUCTION_EXTERNAL = {
    "domain": frozenset({"uuid", "time", "thiserror"}),
    # Controlled theme declarations and their exact serialized size limits are
    # pure configuration contracts; JSON parsing adds no persistence/runtime adapter.
    "application": frozenset({"uuid", "time", "serde", "serde_json", "thiserror", "async-trait", "url"}),
}
TEST_EXTERNAL = {
    **PRODUCTION_EXTERNAL,
    # Application fake-port tests run async use cases; the runtime stays in tests.
    "application": PRODUCTION_EXTERNAL["application"] | {"tokio"},
}
INTERFACES_FORBIDDEN = frozenset({"sqlx", "minijinja"})


def dependency_context(dependency):
    kind = dependency.get("kind") or "normal"
    target = dependency.get("target") or "all targets"
    alias = dependency.get("rename")
    optional = ", optional" if dependency.get("optional") else ""
    renamed = f", renamed to {alias}" if alias else ""
    return f"{kind}, {target}{optional}{renamed}"


def check_metadata(metadata):
    """Return all violations without depending on the host's active cfg/features."""
    member_ids = set(metadata["workspace_members"])
    packages = [package for package in metadata["packages"] if package["id"] in member_ids]
    by_directory = {
        Path(package["manifest_path"]).resolve().parent: package["name"]
        for package in packages
    }
    member_names = {package["name"] for package in packages}
    violations = []
    for package in packages:
        owner = package["name"]
        if owner not in PRODUCTION_INTERNAL:
            violations.append(f"{owner}: workspace member has no reviewed dependency policy")
            continue
        for dependency in package["dependencies"]:
            name = dependency["name"]  # Cargo's canonical package name, not its rename.
            label = f"{owner} -> {name} [{dependency_context(dependency)}]"
            kind = dependency.get("kind") or "normal"
            if kind not in {"normal", "build", "dev"}:
                violations.append(f"{label}: unknown dependency kind")
                continue
            testing = kind == "dev"
            internal_policy = TEST_INTERNAL if testing else PRODUCTION_INTERNAL
            external_policy = TEST_EXTERNAL if testing else PRODUCTION_EXTERNAL
            path = dependency.get("path")
            if path is not None:
                target_name = by_directory.get(Path(path).resolve())
                if target_name is None:
                    violations.append(f"{label}: local package is outside the reviewed workspace")
                elif target_name not in internal_policy[owner]:
                    violations.append(f"{label}: project dependency violates the layer boundary")
                continue
            if name in member_names:
                violations.append(f"{label}: workspace package name resolves outside its local member")
            elif owner in external_policy and name not in external_policy[owner]:
                violations.append(f"{label}: third-party dependency is not approved for this layer")
            elif owner == "interfaces" and any(
                name == forbidden or name.startswith(forbidden + "-")
                for forbidden in INTERFACES_FORBIDDEN
            ):
                violations.append(f"{label}: persistence/template adapters belong in infrastructure")
    return violations


def load_metadata(manifest_path):
    result = subprocess.run(
        [
            "cargo", "metadata", "--format-version", "1", "--no-deps", "--all-features",
            "--locked", "--offline", "--manifest-path", str(manifest_path),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def main():
    manifest = Path(__file__).resolve().parents[1] / "Cargo.toml"
    try:
        violations = check_metadata(load_metadata(manifest))
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError) as error:
        detail = error.stderr.strip() if isinstance(error, subprocess.CalledProcessError) else str(error)
        print(f"Dependency check could not inspect Cargo metadata: {detail}", file=sys.stderr)
        return 1
    if violations:
        print("Cargo dependency boundaries failed:", file=sys.stderr)
        for violation in violations:
            print(f"- {violation}", file=sys.stderr)
        return 1
    print("Cargo dependency boundaries passed (normal/build/dev, all targets and optional dependencies).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
