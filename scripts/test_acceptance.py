"""Resource ownership and secret handling for the real HTTP acceptance runner."""

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

import acceptance


ADMIN_URL = "postgres://blog:private-password@127.0.0.1:5432/postgres"


class AcceptanceSafetyTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.args = argparse.Namespace(admin_url=ADMIN_URL, binary=self.root / "blog",
                                       admin_dist=self.root / "admin")
        self.args.binary.touch()
        self.args.admin_dist.mkdir()
        (self.args.admin_dist / "index.html").touch()
        self.env = patch.dict(os.environ, {"BLOG_TEST_ADMIN_URL": ADMIN_URL}, clear=True)
        self.env.start()
        self.addCleanup(self.env.stop)
        self.suite = acceptance.Acceptance(self.args, self.root)
        self.pg = Mock()
        self.pg.query.return_value = ""
        self.suite.pg = self.pg

    def test_only_explicit_loopback_database_without_connection_overrides(self):
        for url in ("", "postgres://user:secret@example.test/postgres", "sqlite:///tmp/test.db",
                    ADMIN_URL + "?host=remote.test", ADMIN_URL + "#fragment",
                    "postgres://blog:secret@127.0.0.1/bad-name"):
            with self.subTest(url=url), self.assertRaises(acceptance.AcceptanceError):
                acceptance.admin_url(url)
        self.assertEqual(acceptance.admin_url(ADMIN_URL), ADMIN_URL)
        self.assertEqual(acceptance.database_url(ADMIN_URL, "blog_acceptance_test"),
                         "postgres://blog:private-password@127.0.0.1:5432/blog_acceptance_test")

    def test_inherited_site_and_postgres_configuration_cannot_escape_test_environment(self):
        with patch.dict(os.environ, {"PATH": "/safe/bin", "DATABASE_URL": "production",
                                    "BLOG_CONFIG_FILE": "original-config.toml", "BLOG_RECOVERY_MODE": "1",
                                    "BLOG_PUBLIC_BASE_URL": "https://production.test", "PGSERVICE": "production",
                                    "PGHOST": "production.test", "PGOPTIONS": "unsafe"}):
            env = self.suite.env()
        self.assertNotIn("DATABASE_URL", env)
        self.assertNotIn("PGSERVICE", env)
        self.assertNotIn("PGHOST", env)
        self.assertNotIn("PGOPTIONS", env)
        self.assertNotIn("BLOG_PUBLIC_BASE_URL", env)
        self.assertEqual(env["BLOG_RECOVERY_MODE"], "0")
        self.assertEqual(env["BLOG_CONFIG_FILE"], str(self.root / "config.toml"))
        self.assertEqual(env["PATH"], "/safe/bin")

    def test_database_collision_never_creates_or_drops_any_database(self):
        self.pg.query.return_value = "1"
        with self.assertRaisesRegex(acceptance.AcceptanceError, "refusing to reuse"):
            self.suite.prepare()
        self.suite.cleanup()
        self.pg.execute.assert_not_called()
        self.assertFalse(self.suite.owned)
        self.assertFalse(any("DROP" in call.args[0] for call in self.pg.query.call_args_list))

    def test_failed_create_does_not_claim_an_existing_database(self):
        self.pg.execute.side_effect = acceptance.AcceptanceError("createdb failed")
        with self.assertRaises(acceptance.AcceptanceError):
            self.suite.prepare()
        self.suite.cleanup()
        self.assertFalse(self.suite.owned)
        self.assertFalse(any("DROP" in call.args[0] for call in self.pg.query.call_args_list))

    def restore_marker(self):
        self.suite.restore_dir.mkdir()
        tag = acceptance.recovery.ISOLATION_PREFIX + "a" * 32
        (self.suite.restore_dir / "ISOLATED").write_text(tag)
        return tag

    def test_failed_restore_with_matching_guard_is_cleaned_alongside_source(self):
        tag = self.restore_marker()
        self.suite.owned.add(self.suite.source)
        self.pg.query.side_effect = lambda sql, database: tag if sql.startswith("SELECT") else ""
        self.suite.cleanup()
        dropped = [call.args[0] for call in self.pg.query.call_args_list if call.args[0].startswith("DROP")]
        self.assertEqual(set(dropped), {f'DROP DATABASE "{name}" WITH (FORCE)'
                                       for name in (self.suite.source, self.suite.target)})
        self.assertFalse(self.suite.owned)

    def test_unproven_restore_is_untouched_but_source_is_still_cleaned(self):
        self.restore_marker()
        self.suite.owned.add(self.suite.source)
        self.pg.query.side_effect = lambda sql, database: "missing guard" if sql.startswith("SELECT") else ""
        with self.assertRaisesRegex(acceptance.AcceptanceError, "ownership unproven"):
            self.suite.cleanup()
        dropped = [call.args[0] for call in self.pg.query.call_args_list if call.args[0].startswith("DROP")]
        self.assertEqual(dropped, [f'DROP DATABASE "{self.suite.source}" WITH (FORCE)'])

    def test_cleanup_attempts_other_owned_resources_after_a_drop_failure(self):
        self.suite.owned.update((self.suite.source, self.suite.target))
        self.pg.query.side_effect = [acceptance.AcceptanceError("unavailable"), ""]
        with self.assertRaisesRegex(acceptance.AcceptanceError, "could not clean"):
            self.suite.cleanup()
        self.assertEqual(self.pg.query.call_count, 2)
        self.assertEqual(len(self.suite.owned), 1)

    def test_unresponsive_writer_is_killed_and_reaped_before_cleanup(self):
        process = Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("blog", 10), 0]
        self.suite.process = process
        self.suite.owned.add(self.suite.source)
        self.pg.query.side_effect = lambda *_: self.assertIsNone(self.suite.process)
        self.suite.cleanup()
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(process.wait.call_count, 2)

    def test_subprocess_failures_and_timeouts_never_echo_credentials(self):
        failed = subprocess.CompletedProcess([], 1, stdout=ADMIN_URL, stderr="安装码：" + "f" * 64)
        with patch("acceptance.subprocess.run", return_value=failed):
            with self.assertRaisesRegex(acceptance.AcceptanceError, r"^backup: unexpected exit status 1$"):
                acceptance.run_command(["backup", ADMIN_URL], {"PASSWORD": "secret"}, "backup")
        with patch("acceptance.subprocess.run", side_effect=subprocess.TimeoutExpired([ADMIN_URL], 10)):
            with self.assertRaisesRegex(acceptance.AcceptanceError, r"^backup: timed out$"):
                acceptance.run_command(["backup", ADMIN_URL], {}, "backup")

    def test_http_failure_reports_only_status_and_machine_error_code(self):
        client = acceptance.Client("http://127.0.0.1:8080")
        payload = json.dumps({"code": "invalid_credentials", "error": ADMIN_URL}).encode()
        client.opener.open = Mock(side_effect=HTTPError(client.origin, 401, "failure", {}, io.BytesIO(payload)))
        with self.assertRaisesRegex(acceptance.AcceptanceError,
                                    r"^POST /auth/login/password: HTTP 401 \(invalid_credentials\), expected 200$"):
            client.json("POST", "/auth/login/password", {"password": "private-password"})

    def test_task_polling_does_not_hide_failure_as_a_timeout_or_accept_a_missing_record(self):
        self.suite.task_view = Mock(return_value={"runs": {"items": [{"id": "accepted", "status": "failed"}]}})
        with self.assertRaisesRegex(acceptance.AcceptanceError, "unexpected task result: failed"), \
                patch("acceptance_support.time.sleep") as sleep:
            self.suite.wait_task("accepted", "html_rebuild")
        sleep.assert_not_called()
        self.suite.task_view.return_value = {"runs": {"items": []}}
        with self.assertRaisesRegex(acceptance.AcceptanceError, "disappeared"):
            self.suite.wait_task("accepted", "html_rebuild")

    def test_task_restart_hook_runs_after_writer_stop_and_before_activation(self):
        order = []
        self.suite.stop = lambda: order.append("stopped")
        self.suite.start = lambda: order.append("started")
        self.suite.origin = "http://127.0.0.1:8080"
        def tasks(restart):
            restart(lambda: order.append("due fixture"))
            return {"restarted_queue_id": "accepted"}
        self.suite.tasks = tasks
        with patch("acceptance.Client") as client:
            self.suite.task_lifecycle()
        self.assertEqual(order, ["stopped", "due fixture", "started"])
        client.return_value.login.assert_called_once_with(self.suite.password)
        self.assertEqual(self.suite.evidence["task_lifecycle"]["restarted_queue_id"], "accepted")

    def test_failure_report_is_sanitized_and_cleanup_runs(self):
        report = self.root / "report.json"
        with patch("sys.argv", ["acceptance.py", "--report", str(report)]), \
                patch.object(acceptance.Acceptance, "run", side_effect=RuntimeError(ADMIN_URL)), \
                patch.object(acceptance.Acceptance, "cleanup") as cleanup, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as stderr:
            result = acceptance.main()
        self.assertEqual(result, 1)
        cleanup.assert_called_once()
        saved = json.loads(report.read_text())
        self.assertEqual(saved["error"], "unexpected RuntimeError")
        self.assertEqual(saved["steps"][0]["name"], "cleanup")
        for secret in (ADMIN_URL, "private-password"):
            self.assertNotIn(secret, report.read_text() + stderr.getvalue())

    def test_report_never_overwrites_existing_file_or_starts_work(self):
        report = self.root / "report.json"
        report.write_text("existing evidence")
        with patch("sys.argv", ["acceptance.py", "--report", str(report)]), \
                patch("acceptance.Acceptance") as suite, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(acceptance.main(), 1)
        suite.assert_not_called()
        self.assertEqual(report.read_text(), "existing evidence")


if __name__ == "__main__":
    unittest.main()
