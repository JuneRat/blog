"""Policy negative cases plus Cargo's actual renamed/target/dev metadata shape."""

import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from check_dependencies import check_metadata


def package(name, dependencies=()):
    return {
        "name": name,
        "id": name,
        "manifest_path": f"/workspace/crates/{name}/Cargo.toml",
        "dependencies": list(dependencies),
    }


def dependency(name, *, kind=None, target=None, local=False, rename=None, optional=False):
    return {
        "name": name, "kind": kind, "target": target, "rename": rename,
        "optional": optional, "path": f"/workspace/crates/{name}" if local else None,
    }


def workspace(*packages):
    return {"packages": list(packages), "workspace_members": [p["id"] for p in packages]}


class DependencyPolicyTests(unittest.TestCase):
    def test_current_layers_and_test_exception_are_allowed(self):
        metadata = workspace(
            package("domain", [dependency("uuid")]),
            package("application", [dependency("domain", local=True), dependency("tokio", kind="dev")]),
            package("infrastructure", [dependency("application", local=True), dependency("sqlx")]),
            package("interfaces", [dependency("application", local=True), dependency("axum")]),
            package("server", [dependency("interfaces", local=True), dependency("domain", local=True, kind="dev")]),
        )
        self.assertEqual(check_metadata(metadata), [])

    def test_outward_internal_edge_is_rejected_for_every_dependency_kind(self):
        for kind in (None, "build", "dev"):
            with self.subTest(kind=kind):
                errors = check_metadata(workspace(
                    package("domain", [dependency("infrastructure", kind=kind, local=True)]),
                    package("infrastructure"),
                ))
                self.assertEqual(len(errors), 1)
                self.assertIn("layer boundary", errors[0])

    def test_test_only_exception_never_allows_production_or_build(self):
        for kind in (None, "build"):
            with self.subTest(kind=kind):
                errors = check_metadata(workspace(
                    package("server", [dependency("domain", kind=kind, local=True)]),
                    package("domain"),
                ))
                self.assertEqual(len(errors), 1)

    def test_runtime_stays_outside_application_production(self):
        for kind in (None, "build"):
            with self.subTest(kind=kind):
                errors = check_metadata(workspace(package("application", [dependency("tokio", kind=kind)])))
                self.assertEqual(len(errors), 1)
                self.assertIn("third-party", errors[0])

    def test_inactive_target_optional_renamed_framework_cannot_bypass_check(self):
        errors = check_metadata(workspace(package("application", [dependency(
            "sqlx", target='cfg(target_os = "windows")', rename="storage", optional=True,
        )])))
        self.assertEqual(len(errors), 1)
        self.assertIn("windows", errors[0])
        self.assertIn("renamed to storage", errors[0])
        self.assertIn("optional", errors[0])

    def test_domain_framework_is_rejected_even_in_dev_dependencies(self):
        errors = check_metadata(workspace(package("domain", [dependency("axum", kind="dev")])))
        self.assertEqual(len(errors), 1)

    def test_interfaces_cannot_directly_use_adapter_subpackages(self):
        for name in ("sqlx", "sqlx-core", "minijinja"):
            with self.subTest(name=name):
                errors = check_metadata(workspace(package("interfaces", [dependency(name)])))
                self.assertEqual(len(errors), 1)
                self.assertIn("infrastructure", errors[0])

    def test_unknown_workspace_or_local_package_requires_policy(self):
        errors = check_metadata(workspace(package("unreviewed")))
        self.assertEqual(len(errors), 1)
        errors = check_metadata(workspace(package("application", [dependency("helper", local=True)])))
        self.assertEqual(len(errors), 1)
        self.assertIn("outside the reviewed workspace", errors[0])

    def test_registry_package_cannot_impersonate_workspace_member(self):
        errors = check_metadata(workspace(
            package("application", [dependency("domain")]), package("domain"),
        ))
        self.assertEqual(len(errors), 1)
        self.assertIn("outside its local member", errors[0])

    def test_nonmember_transitive_packages_are_not_direct_layer_edges(self):
        metadata = workspace(package("application", [dependency("serde")]))
        metadata["packages"].append(package("serde", [dependency("serde_derive")]))
        self.assertEqual(check_metadata(metadata), [])

    def test_real_cargo_metadata_preserves_target_rename_and_dev_kinds(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "Cargo.toml").write_text('[workspace]\nmembers = ["domain", "application", "server"]\nresolver = "3"\n')
            dependencies = {
                "domain": "",
                "application": '[target.\'cfg(target_os = "windows")\'.build-dependencies]\nouter = { package = "server", path = "../server" }\n',
                "server": '[dev-dependencies]\ndomain = { path = "../domain" }\n',
            }
            for name, declaration in dependencies.items():
                directory = root / name
                (directory / "src").mkdir(parents=True)
                (directory / "src/lib.rs").write_text("")
                (directory / "Cargo.toml").write_text(
                    f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n' + declaration
                )
            result = subprocess.run(
                ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path", str(root / "Cargo.toml")],
                capture_output=True, text=True, check=True,
            )
            errors = check_metadata(json.loads(result.stdout))
            self.assertEqual(len(errors), 1)
            self.assertIn("application -> server", errors[0])
            self.assertIn("build", errors[0])
            self.assertIn("windows", errors[0])
            self.assertIn("renamed to outer", errors[0])


if __name__ == "__main__":
    unittest.main()
