"""Task-load contract, local threshold checks and disposable resource ownership."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

import benchmark_tasks as benchmark


class TaskCapacityTests(unittest.TestCase):
    def args(self, **changes):
        values = dict(image="blog:test", expected_revision=None, pool_sizes=[5], concurrency=4,
                      posts=2000, writers=4, duration=300, write_interval_ms=250,
                      connection_lifetime_seconds=60,
                      task_interval=15, scheduled_per_cycle=16, read_p95_ms=500,
                      write_p95_ms=2000, publication_delay_seconds=45)
        values.update(changes)
        return argparse.Namespace(**values)

    def test_invalid_duration_scale_and_thresholds_are_rejected(self):
        for changes in ({"duration": float("nan")}, {"duration": 0}, {"duration": 3601},
                        {"posts": 1}, {"writers": 33}, {"pool_sizes": [0]},
                        {"concurrency": 129}, {"scheduled_per_cycle": 0},
                        {"task_interval": 0}, {"read_p95_ms": float("inf")}, {"image": "-unsafe"}):
            with self.subTest(changes=changes), self.assertRaises(benchmark.AcceptanceError):
                benchmark.validate(self.args(**changes))
        benchmark.validate(self.args())

    def test_metric_parser_selects_fixed_workload_series_and_discards_nonfinite_values(self):
        result = benchmark.metric_values(b"# comment\nblog_render_active{kind=\"content\"} 2\n"
                                        b"blog_task_due_wait_seconds{kind=\"html_rebuild\"} 1.5\n"
                                        b"blog_http_requests_total{route=\"private\"} 99\n"
                                        b"blog_render_active{kind=\"bad\"} NaN\n"
                                        b"blog_render_queue_duration_seconds_bucket{le=\"1\"} 7\n")
        self.assertEqual(result, {'blog_render_active{kind="content"}': 2,
                                  'blog_task_due_wait_seconds{kind="html_rebuild"}': 1.5})

    def test_docker_memory_units(self):
        self.assertEqual(benchmark.memory_bytes("1.5MiB "), 1572864)
        self.assertEqual(benchmark.memory_bytes("2GB"), 2000000000)

    def row(self):
        return {"requests": 10, "failed_operations": 0, "p95_ms": 10,
                "readyz": {"failed_operations": 0}, "monitor": {"errors": 0, "metrics": {"blog_task_scheduler_available": {"max": 1, "last": 1}}},
                "writes": {"save": {"requests": 4, "errors": 0, "conflicts": 1, "p95_ms": 30}},
                "background": {"publication": {"max_delay_seconds": 29}}}

    def test_all_conflicting_writes_are_not_a_pass_and_delay_threshold_is_enforced(self):
        row = self.row()
        self.assertTrue(benchmark.passed(row, self.args()))
        row["writes"]["save"]["conflicts"] = 4
        self.assertFalse(benchmark.passed(row, self.args()))
        row = self.row()
        row["background"]["publication"]["max_delay_seconds"] = 46
        self.assertFalse(benchmark.passed(row, self.args()))
        row = self.row()
        row["monitor"]["errors"] = 1
        self.assertFalse(benchmark.passed(row, self.args()))

    def test_failure_cleans_only_owned_site(self):
        site = Mock()
        site.prepare.side_effect = benchmark.AcceptanceError("synthetic readiness failure")
        with patch.object(benchmark, "ComposeSite", return_value=site):
            with self.assertRaises(benchmark.AcceptanceError):
                benchmark.measure(self.args(), Path("/unused"), 5, True)
        site.cleanup.assert_called_once()

    def test_interruption_stops_both_workers_before_join_and_still_joins_monitor(self):
        site = Mock()
        site.query.return_value = json.dumps("2026-10-01T00:00:00+00:00")
        monitor, background = Mock(stop=threading.Event()), Mock(stop=threading.Event())
        reader, worker = Mock(), Mock()
        reader.is_alive.return_value = worker.is_alive.return_value = False
        def interrupted_join(**_kwargs):
            self.assertTrue(monitor.stop.is_set())
            self.assertTrue(background.stop.is_set())
            raise KeyboardInterrupt
        worker.join.side_effect = interrupted_join
        with patch.object(benchmark, "ComposeSite", return_value=site), \
                patch.object(benchmark, "Monitor", return_value=monitor), \
                patch.object(benchmark, "Background", return_value=background), \
                patch.object(benchmark.threading, "Thread", side_effect=[reader, worker]), \
                patch.object(benchmark, "load", side_effect=[{}, KeyboardInterrupt]):
            with self.assertRaises(KeyboardInterrupt):
                benchmark.measure(self.args(), Path("/unused"), 5, True)
        reader.join.assert_called_once_with(timeout=30)
        site.cleanup.assert_called_once()

    def test_empty_container_list_never_invokes_global_docker_stats(self):
        site = Mock(project="owned-project")
        site.metrics.request.return_value = (b"blog_render_active 0", {})
        site.compose.return_value = ""
        with patch.object(benchmark.subprocess, "run") as command:
            with self.assertRaisesRegex(benchmark.AcceptanceError, "both owned"):
                benchmark.Monitor(site).sample()
        command.assert_not_called()

    def test_completed_partial_html_work_cannot_pass_with_pending_backlog(self):
        site = Mock()
        site.query.side_effect = ["0", "0", "", json.dumps([{
            "kind": "html_rebuild", "status": "completed",
            "report": {"html": {"failure": None, "rebuilt": {"posts": 10000}, "has_more": True}},
        }]), json.dumps({"expected": 2, "published": 2}), json.dumps({"remaining_comment_ips": 0})]
        site.admin.clone.return_value.json.return_value = {"pending_html": {"posts": 90000, "pages": 0, "comments": 0}}
        background = benchmark.Background(site)
        background.cycles = 1
        background.runs = ["00000000-0000-0000-0000-000000000001"]
        with self.assertRaisesRegex(benchmark.AcceptanceError, "unfinished rebuild backlog"):
            background.finish()
        self.assertEqual(background.result["pending_html"]["posts"], 90000)

    def test_active_rebuild_does_not_reinvalidate_processed_fixture_rows(self):
        site = Mock(post={"id": "post"}, content_marker=2, comment_marker=3)
        site.query.return_value = "1"
        site.args.scheduled_per_cycle = 2
        site.admin.clone.return_value.json.return_value = {"id": "same-run"}
        background = benchmark.Background(site)
        background.cycle()
        self.assertEqual(background.skipped_rebuild_fixtures, 1)
        self.assertFalse(any("UPDATE posts SET" in call.args[0] for call in site.query.call_args_list))
        self.assertEqual(background.cycles, 1)

    def test_pruned_task_history_cannot_hide_a_missing_execution(self):
        site = Mock()
        site.query.side_effect = ["0", "0", "", json.dumps([{
            "kind": "html_rebuild", "status": "completed",
            "report": {"html": {"failure": None, "rebuilt": {"posts": 2000}}},
        }]), json.dumps({"expected": 2, "published": 2}), json.dumps({"remaining_comment_ips": 0})]
        site.admin.clone.return_value.json.return_value = {"pending_html": {"posts": 0, "pages": 0, "comments": 0}}
        background = benchmark.Background(site)
        background.cycles = 1
        background.runs = ["00000000-0000-0000-0000-000000000001", "00000000-0000-0000-0000-000000000002"]
        with self.assertRaisesRegex(benchmark.AcceptanceError, "task history is incomplete"):
            background.finish()
        self.assertEqual(len(background.result["runs"]), 1)
        self.assertEqual(background.result["pending_html"]["posts"], 0)

    def test_scheduler_error_or_final_unavailable_cannot_hide_behind_an_earlier_success(self):
        row = self.row()
        row["monitor"]["metrics"]["blog_task_scheduler_available"]["last"] = 0
        self.assertFalse(benchmark.passed(row, self.args()))
        row = self.row()
        row["monitor"]["metrics"]['blog_task_scheduler_checks_total{result="error"}'] = {"first": 0, "last": 1}
        self.assertFalse(benchmark.passed(row, self.args()))

    def test_background_validation_failure_retains_foreground_and_partial_evidence(self):
        site = Mock(build={})
        site.query.return_value = json.dumps("2026-10-01T00:00:00+00:00")
        monitor = Mock(stop=threading.Event(), errors=0, metrics=self.row()["monitor"]["metrics"], resources={})
        background = Mock(stop=threading.Event(), result={"pending_html": {"posts": 2}})
        background.finish.side_effect = benchmark.AcceptanceError("unfinished rebuild backlog")
        thread = Mock()
        thread.is_alive.return_value = False
        with patch.object(benchmark, "ComposeSite", return_value=site), \
                patch.object(benchmark, "Monitor", return_value=monitor), \
                patch.object(benchmark, "Background", return_value=background), \
                patch.object(benchmark.threading, "Thread", return_value=thread), \
                patch.object(benchmark, "load", side_effect=[{}, self.row()]):
            row = benchmark.measure(self.args(), Path("/unused"), 5, True)
        self.assertFalse(row["passed"])
        self.assertEqual(row["requests"], 10)
        self.assertEqual(row["background"]["pending_html"]["posts"], 2)
        self.assertIn("background_validation", row["failed_checks"])
        site.cleanup.assert_called_once()

    def test_existing_report_is_never_overwritten_or_used_to_start_docker(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            report.write_text("existing evidence")
            with patch("sys.argv", ["benchmark_tasks.py", "--image", "blog:test", "--report", str(report)]), \
                    patch.object(benchmark.subprocess, "run") as command, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(benchmark.main(), 1)
            command.assert_not_called()
            self.assertEqual(report.read_text(), "existing evidence")

    def test_unexpected_failure_report_does_not_echo_credentials(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            with patch("sys.argv", ["benchmark_tasks.py", "--image", "blog:test", "--report", str(report)]), \
                    patch.object(benchmark.subprocess, "run", side_effect=RuntimeError("secret database password")), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(benchmark.main(), 1)
            saved = json.loads(report.read_text())
            self.assertEqual(saved["error"], "unexpected RuntimeError")
            self.assertNotIn("password", report.read_text())


if __name__ == "__main__":
    unittest.main()
