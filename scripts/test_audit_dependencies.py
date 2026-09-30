import copy
import unittest

from audit_dependencies import CRATES_IO, RSA_CHECKSUM, evaluate


class DependencyAuditPolicyTests(unittest.TestCase):
    def setUp(self):
        self.report = {
            "settings": {"ignore": []},
            "vulnerabilities": {
                "count": 1,
                "found": True,
                "list": [{
                    "advisory": {"id": "RUSTSEC-2023-0071", "package": "rsa"},
                    "package": {
                        "name": "rsa", "version": "0.9.10", "source": CRATES_IO,
                        "checksum": RSA_CHECKSUM,
                    },
                    "versions": {"patched": []},
                }],
            },
            "warnings": {},
        }
        self.tree = "server v0.1.0 (/repo/crates/server) \n\n \nsqlx v0.8.6 postgres\n"

    def test_only_exact_unreachable_finding_is_explained(self):
        blocked, explained = evaluate(self.report, self.tree)
        self.assertEqual(len(blocked), 0)
        self.assertEqual(len(explained), 1)

    def test_rsa_or_mysql_on_any_platform_blocks(self):
        for package in ("rsa v0.9.10", "rsa v0.10.0", "sqlx-mysql v0.8.6"):
            with self.subTest(package=package):
                blocked, explained = evaluate(self.report, self.tree + package + "\n")
                self.assertEqual(len(blocked), 1)
                self.assertFalse(explained)

    def test_changed_version_source_checksum_or_patch_blocks(self):
        changes = [
            ("version", "0.9.11"), ("source", "git+https://example.com/rsa"),
            ("checksum", "changed"),
        ]
        for key, value in changes:
            with self.subTest(key=key):
                report = copy.deepcopy(self.report)
                report["vulnerabilities"]["list"][0]["package"][key] = value
                self.assertEqual(len(evaluate(report, self.tree)[0]), 1)
        report = copy.deepcopy(self.report)
        report["vulnerabilities"]["list"][0]["versions"]["patched"] = [">=0.10.0"]
        self.assertEqual(len(evaluate(report, self.tree)[0]), 1)

    def test_any_other_advisory_or_warning_blocks_even_if_unreachable(self):
        finding = copy.deepcopy(self.report["vulnerabilities"]["list"][0])
        finding["advisory"]["id"] = "RUSTSEC-2099-0001"
        report = copy.deepcopy(self.report)
        report["vulnerabilities"]["list"].append(finding)
        report["vulnerabilities"]["count"] = 2
        self.assertEqual(len(evaluate(report, self.tree)[0]), 1)
        report = copy.deepcopy(self.report)
        report["warnings"] = {"unmaintained": [finding]}
        self.assertEqual(len(evaluate(report, self.tree)[0]), 1)

    def test_missing_graph_or_unrecognized_output_fails_closed(self):
        for tree in ("", "warning: missing graph\n", "rsa v0.9.10\nnot a package\n"):
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                evaluate(self.report, tree)

    def test_ignored_advisories_and_invalid_report_fail_closed(self):
        report = copy.deepcopy(self.report)
        report["settings"]["ignore"] = ["RUSTSEC-2023-0071"]
        with self.assertRaises(ValueError):
            evaluate(report, self.tree)
        report = copy.deepcopy(self.report)
        report["vulnerabilities"]["count"] = 0
        with self.assertRaises(ValueError):
            evaluate(report, self.tree)
        with self.assertRaises(KeyError):
            evaluate({}, self.tree)


if __name__ == "__main__":
    unittest.main()
