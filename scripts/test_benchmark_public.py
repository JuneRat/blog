"""Bounded load measurement, mixed-write failures and disposable-run safety."""

import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

import benchmark_public as benchmark
from benchmark_public import AcceptanceError


class BenchmarkTests(unittest.TestCase):
    def args(self, **changes):
        values = dict(pool_sizes=[5], concurrency=[2], requests=100, posts=200,
                      duration=None, mixed=False, series_reorder=False, write_interval_ms=250,
                      writers=1, series_members=2, connection_lifetime_seconds=60)
        values.update(changes)
        return argparse.Namespace(**values)

    def test_invalid_scale_and_duration_are_rejected_before_resources_are_created(self):
        for changes in ({"posts": 0}, {"posts": 100001}, {"duration": float("nan")},
                        {"duration": float("inf")}, {"duration": 0}, {"duration": 3601},
                        {"write_interval_ms": 0}, {"series_reorder": True},
                        {"mixed": True, "series_reorder": True, "posts": 1}, {"writers": 33},
                        {"writers": 3, "posts": 2}, {"mixed": True, "series_reorder": True, "series_members": 201},
                        {"connection_lifetime_seconds": 0}, {"connection_lifetime_seconds": -1},
                        {"connection_lifetime_seconds": float("nan")}, {"connection_lifetime_seconds": float("inf")}):
            with self.subTest(changes=changes), self.assertRaises(AcceptanceError):
                benchmark.validate(self.args(**changes))
        benchmark.validate(self.args(mixed=True, series_reorder=True, posts=2, duration=1))

    def test_histogram_is_bounded_and_separates_conflicts_from_system_errors(self):
        samples = benchmark.Samples()
        for n in range(100000):
            samples.add(409 if n % 2 else 200, .001)
        samples.add(0, .030)
        result = samples.summary()
        self.assertEqual(len(samples.buckets), 2)
        self.assertEqual(result["requests"], 100001)
        self.assertEqual(result["conflicts"], 50000)
        self.assertEqual(result["errors"], 1)
        self.assertEqual(result["failed_operations"], 50001)
        self.assertTrue(1 <= result["p95_ms"] <= 1.021)
        self.assertIsNone(benchmark.Samples().summary()["p95_ms"])

    def test_save_uses_fresh_version_and_retains_payload_without_secret_reporting(self):
        client = Mock()
        client.json.side_effect = [
            {"version": 8}, {"id": "post-id", "version": 9, "content": "Body\n\nCapacity edit 1."},
            {"version": 12}, {"id": "post-id", "version": 13, "content": "Body\n\nCapacity edit 2."},
        ]
        writer = benchmark.MixedWriter(client, "post-id", "Body")
        writer.save()
        writer.save()
        self.assertEqual(client.json.call_args_list[1].args[2]["expected_version"], 8)
        self.assertEqual(client.json.call_args_list[3].args[2]["expected_version"], 12)

    def test_reorder_reads_current_version_and_sends_complete_reversed_membership(self):
        client = Mock()

        def api(method, path, body=None):
            if (method, path) == ("GET", "/api/admin/v1/series"):
                return [{"slug": "unrelated", "version": 99}, {"slug": "series-slug", "version": 14}]
            if (method, path) == ("GET", "/api/admin/v1/series/series-slug/members"):
                return [{"id": "a"}, {"id": "b"}]
            if (method, path) == ("POST", "/api/admin/v1/series/series-slug/reorder"):
                return {"series_version": 15, "ordered_post_ids": ["b", "a"]}
            self.fail(f"Unsupported series API request: {method} {path}")

        client.json.side_effect = api
        benchmark.MixedWriter(client, "a", "Body", "series-slug").reorder()
        self.assertEqual(client.json.call_args.args[2], {
            "ordered_post_ids": ["b", "a"], "expected_series_version": 14,
        })

    def test_status_only_http_error_and_invalid_response_keep_safe_error_types(self):
        samples = benchmark.Samples()
        benchmark.measure_write(Mock(side_effect=AcceptanceError(
            "GET /api/admin/v1/series/a: HTTP 405, expected 200")), samples)
        benchmark.measure_write(Mock(side_effect=KeyError("private response field")), samples)
        result = samples.summary()
        self.assertEqual(result["status_counts"]["405"], 1)
        self.assertEqual(result["error_types"], {"KeyError": 1, "http_405": 1})
        self.assertNotIn("private", json.dumps(result))

    def test_write_conflicts_and_transport_errors_are_not_swallowed_as_success(self):
        samples = benchmark.Samples()
        for error in (AcceptanceError("PATCH /posts/a: HTTP 409 (version_conflict), expected 200"),
                      AcceptanceError("PATCH /posts/a: connection failed"), ValueError("private response")):
            benchmark.measure_write(Mock(side_effect=error), samples)
        result = samples.summary()
        self.assertEqual(result["errors"], 2)
        self.assertEqual(result["failed_operations"], 3)
        self.assertEqual(result["conflicts"], 1)
        self.assertNotIn("private", json.dumps(result))
        row = {"requests": 10, "failed_operations": 0,
               "readyz": {"failed_operations": 0}, "writes": {"save": result}}
        self.assertFalse(benchmark.measurement_passed(row))
        row["writes"]["save"] = benchmark.summarize([(409, .001)])
        self.assertTrue(benchmark.measurement_passed(row))

    def test_planned_rotation_is_before_request_and_transport_failure_is_not_retried(self):
        old, new, after_failure = Mock(), Mock(), Mock()
        old.getresponse.return_value = new.getresponse.return_value = after_failure.getresponse.return_value = Mock(status=200)
        now = [0.0]
        connection = benchmark.ReadConnection("127.0.0.1", 1234, 60)
        with patch.object(benchmark.time, "perf_counter", side_effect=lambda: now[0]), \
                patch.object(benchmark.http.client, "HTTPConnection", side_effect=[old,new,after_failure]) as factory:
            self.assertEqual(connection.fetch("/"), (200,0.0))
            now[0] = 59
            self.assertEqual(connection.fetch("/readyz"), (200,0.0))
            factory.assert_called_once()
            now[0] = 60
            self.assertEqual(connection.fetch("/"), (200,0.0))
            old.close.assert_called_once()
            self.assertEqual(connection.rotations,1)
            new.request.side_effect = OSError("private transport detail")
            now[0] = 61
            samples = benchmark.Samples()
            samples.add(*connection.fetch("/readyz"))
            self.assertEqual(factory.call_count,2, "failed request is not automatically retried")
            self.assertEqual(samples.summary()["failed_operations"],1)
            self.assertEqual(samples.summary()["status_counts"], {"client_or_transport_error":1})
            self.assertNotIn("private",json.dumps(samples.summary()))
            self.assertEqual(connection.rotations,1, "transport failures are not planned rotations")
            now[0] = 62
            self.assertEqual(connection.fetch("/readyz"), (200,0.0))
            self.assertEqual(factory.call_count,3)
            self.assertEqual(connection.rotations,1)
            connection.close()

    def test_direct_load_rejects_invalid_connection_lifetime_before_starting_threads(self):
        with patch.object(benchmark.http.client, "HTTPConnection") as factory:
            for lifetime in (0,-1,float("nan"),float("inf")):
                with self.subTest(lifetime=lifetime), self.assertRaises(AcceptanceError):
                    benchmark.load("http://127.0.0.1:1234",1,1,connection_lifetime=lifetime)
            factory.assert_not_called()

    def fake_load(self, *, duration=None, mixed=False, connection_lifetime=60):
        paths, connections = [], []
        lock = threading.Lock()

        class Connection:
            def __init__(self, *_args, **_kwargs):
                self.closed = False
                connections.append(self)

            def request(self, method, path):
                with lock:
                    paths.append(path)
                time.sleep(.001)

            def getresponse(self):
                return Mock(status=200, read=lambda: b"ok")

            def close(self):
                self.closed = True

        writers = [Mock(series_slug="capacity-series") for _ in range(3)] if mixed else []
        with patch.object(benchmark.http.client, "HTTPConnection", Connection):
            result = benchmark.load("http://127.0.0.1:1234", 3, 3000 if duration else 30, duration=duration,
                                    posts=5, writers=writers, write_interval=.001,
                                    connection_lifetime=connection_lifetime)
        self.assertTrue(all(connection.closed for connection in connections))
        self.assertTrue(benchmark.measurement_passed(result))
        if duration is None:
            self.assertIn("/posts/capacity-4", paths)
        return result, writers

    def test_fixed_request_load_keeps_exact_count_and_cycles_across_content(self):
        result, _ = self.fake_load()
        self.assertEqual(result["requests"], 30)
        self.assertNotIn("writes", result)

    def test_duration_load_overrides_count_and_reports_both_write_operations(self):
        result, writers = self.fake_load(duration=.1, mixed=True)
        self.assertLess(result["requests"], 3000)
        self.assertGreaterEqual(result["seconds"], .1)
        self.assertGreater(result["writes"]["save"]["requests"], 0)
        self.assertEqual(result["writers"], 3)
        self.assertTrue(all(writer.save.call_count > 0 for writer in writers))
        self.assertEqual(result["writes"]["save"]["requests"], sum(writer.save.call_count for writer in writers))
        self.assertEqual(result["writes"]["series_reorder"]["requests"], sum(writer.reorder.call_count for writer in writers))

    def test_read_and_readiness_workers_rotate_connections_and_keep_exact_operation_count(self):
        result, _ = self.fake_load(duration=.16, connection_lifetime=.03)
        self.assertEqual(result["connection_lifetime_seconds"],.03)
        self.assertGreater(result["connection_rotations"]["reads"],0)
        self.assertGreater(result["connection_rotations"]["readyz"],0)
        self.assertEqual(result["failed_operations"],0)
        self.assertEqual(result["readyz"]["failed_operations"],0)

    def test_load_keeps_read_and_readiness_transport_failures_without_retrying_operations(self):
        paths, failed, lock = [], set(), threading.Lock()
        class Connection:
            def __init__(self,*_args,**_kwargs):
                pass
            def request(self,method,path):
                with lock:
                    paths.append(path)
                    kind = "readyz" if path == "/readyz" else "read"
                    if kind not in failed:
                        failed.add(kind)
                        raise OSError("private transport detail")
                time.sleep(.001)
            def getresponse(self):
                return Mock(status=200,read=lambda: b"ok")
            def close(self):
                pass
        with patch.object(benchmark.http.client,"HTTPConnection",Connection):
            result = benchmark.load("http://127.0.0.1:1234",1,30,connection_lifetime=.005)
        self.assertEqual(result["requests"],30)
        self.assertEqual(sum(path != "/readyz" for path in paths),30, "each measured read has exactly one request attempt")
        self.assertEqual(result["failed_operations"],1)
        self.assertEqual(result["readyz"]["failed_operations"],1)
        self.assertEqual(result["status_counts"]["client_or_transport_error"],1)
        self.assertEqual(result["readyz"]["status_counts"]["client_or_transport_error"],1)
        self.assertFalse(benchmark.measurement_passed(result))
        self.assertNotIn("private",json.dumps(result))

    def test_existing_report_is_untouched_and_no_site_is_started(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "evidence.json"
            report.write_text("existing evidence")
            with patch("sys.argv", ["benchmark_public.py", "--report", str(report)]), \
                    patch.object(benchmark, "CapacitySite") as site, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(benchmark.main(), 1)
            site.assert_not_called()
            self.assertEqual(report.read_text(), "existing evidence")

    def test_failure_always_cleans_owned_resources_and_hides_credentials(self):
        secret = "postgres://blog:private-password@127.0.0.1:5432/postgres"
        site = Mock()
        site.prepare.side_effect = RuntimeError(secret)
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "evidence.json"
            with patch("sys.argv", ["benchmark_public.py", "--report", str(report), "--mixed"]), \
                    patch.dict(os.environ, {"BLOG_TEST_ADMIN_URL": secret}), \
                    patch.object(benchmark, "CapacitySite", return_value=site), \
                    contextlib.redirect_stderr(io.StringIO()) as stderr:
                self.assertEqual(benchmark.main(), 1)
            site.cleanup.assert_called_once()
            self.assertEqual(json.loads(report.read_text())["error"], "unexpected RuntimeError")
            self.assertNotIn(secret, report.read_text() + stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
