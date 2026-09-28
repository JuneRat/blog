#!/usr/bin/env python3
"""Compare pool sizes on a disposable installed site, never an existing site.

Uses BLOG_TEST_ADMIN_URL (loopback only) and optionally BLOG_TEST_PG_CONTAINER,
like acceptance.py. Creates and removes its own random database. Measurements
include client overhead and are local capacity evidence, not a production SLA.
"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import datetime as dt
import http.client
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit

from acceptance import API, Acceptance, AcceptanceError, Client, PROJECT, admin_url, require


class CapacitySite(Acceptance):
    pool_size = 5

    def env(self, *args, **kwargs):
        env = super().env(*args, **kwargs)
        env.update(BLOG_DB_MAX_CONNECTIONS=str(self.pool_size),
                   BLOG_DB_MIN_CONNECTIONS=str(self.pool_size),
                   BLOG_DB_STATEMENT_TIMEOUT_MS="30000", BLOG_DB_LOCK_TIMEOUT_MS="5000")
        return env


class Samples:
    """Bounded logarithmic histogram; duration runs do not retain every request."""

    def __init__(self):
        self.statuses = Counter()
        self.buckets = Counter()
        self.error_types = Counter()

    def add(self, status, elapsed, error_type=None):
        self.statuses[status] += 1
        if error_type:
            self.error_types[error_type] += 1
        bucket = math.ceil(math.log(max(elapsed, .000001), 1.02))
        self.buckets[bucket] += 1

    def merge(self, other):
        self.statuses.update(other.statuses)
        self.buckets.update(other.buckets)
        self.error_types.update(other.error_types)

    def summary(self):
        count = sum(self.statuses.values())

        def percentile(fraction):
            remaining = math.ceil(count * fraction)
            for bucket, size in sorted(self.buckets.items()):
                remaining -= size
                if remaining <= 0:
                    return round(1.02 ** bucket * 1000, 3)
            return None

        return {"requests": count, "errors": count - self.statuses[200] - self.statuses[409],
                "failed_operations": count - self.statuses[200],
                "conflicts": self.statuses[409],
                "error_types": dict(sorted(self.error_types.items())),
                "status_counts": {str(status) if status else "client_or_transport_error": amount
                                  for status, amount in sorted(self.statuses.items()) if amount},
                "p50_ms": percentile(.50), "p95_ms": percentile(.95), "p99_ms": percentile(.99)}


def summarize(samples):
    collected = Samples()
    for status, elapsed in samples:
        collected.add(status, elapsed)
    return collected.summary()


class MixedWriter:
    """One author; each operation includes reading its current CAS version."""

    def __init__(self, client, post_id, content, series_slug=None):
        self.client = client
        self.post_id = post_id
        self.content = content
        self.series_slug = series_slug
        self.sequence = 0

    def save(self):
        path = f"{API}/posts/{self.post_id}"
        current = self.client.json("GET", path)
        self.sequence += 1
        content = self.content + f"\n\nCapacity edit {self.sequence}."
        saved = self.client.json("PATCH", path, {
            "content": content, "expected_version": current["version"],
        })
        require(saved["id"] == self.post_id and saved["content"] == content
                and saved["version"] > current["version"], "save response does not match submitted edit")

    def reorder(self):
        path = f"{API}/series/{self.series_slug}"
        # The admin API exposes a catalog GET, not GET /series/{slug}.
        catalog = self.client.json("GET", API + "/series")
        series = next((item for item in catalog if item["slug"] == self.series_slug), None)
        require(series is not None, "capacity series missing from catalog")
        members = self.client.json("GET", path + "/members")
        order = [member["id"] for member in reversed(members)]
        result = self.client.json("POST", path + "/reorder", {
            "ordered_post_ids": order, "expected_series_version": series["version"],
        })
        require(result["ordered_post_ids"] == order, "reorder response does not match submitted order")


def measure_write(operation, samples):
    started = time.perf_counter()
    status = 200
    error_type = None
    try:
        operation()
    except AcceptanceError as error:
        match = re.search(r": HTTP ([1-5]\d{2})(?:[ ,]|$)", str(error))
        status = int(match.group(1)) if match else 0
        error_type = (f"http_{status}" if match else "transport" if str(error).endswith(": connection failed")
                      else "contract_validation")
    except (OSError, ValueError, KeyError, TypeError) as error:
        status = 0
        # Preserve the safe exception class, never its potentially private payload.
        error_type = type(error).__name__
    samples.add(status, time.perf_counter() - started, error_type)


def load(origin, concurrency, requests, *, duration=None, posts=200, writers=(), write_interval=.25):
    address = urlsplit(origin)
    timing = {}
    barrier = threading.Barrier(concurrency + 1 + len(writers),
                                action=lambda: timing.update(started=time.perf_counter()))
    stop = threading.Event()

    def expired():
        return duration is not None and time.perf_counter() - timing["started"] >= duration

    def path_for(index):
        # Keep the original index/post/page mix while cycling across all posts.
        if index % 3 == 0:
            return "/"
        if index % 3 == 2:
            return "/capacity-page"
        post = (index // 3) % posts
        return "/posts/" + (f"capacity-{post}" if post else "capacity-post")

    def fetch(connection, path):
        started = time.perf_counter()
        try:
            connection.request("GET", path)
            response = connection.getresponse()
            response.read()
            status = response.status
        except (OSError, http.client.HTTPException):
            connection.close()
            status = 0
        return status, time.perf_counter() - started

    def worker(index):
        connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
        samples = Samples()
        try:
            barrier.wait(timeout=30)
            n = index
            while not stop.is_set() and not expired() and (duration is not None or n < requests):
                samples.add(*fetch(connection, path_for(n)))
                n += concurrency
        finally:
            connection.close()
        return samples

    def probe():
        connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
        samples = Samples()
        try:
            barrier.wait(timeout=30)
            while True:
                samples.add(*fetch(connection, "/readyz"))
                if stop.wait(.05):
                    return samples
        finally:
            connection.close()

    def author(writer):
        saves, reorders = Samples(), Samples()
        barrier.wait(timeout=30)
        # Complete at least one cycle, including in small fixed-request smoke runs.
        while True:
            measure_write(writer.save, saves)
            if writer.series_slug:
                measure_write(writer.reorder, reorders)
            if expired() or stop.wait(write_interval):
                return saves, reorders

    with ThreadPoolExecutor(max_workers=concurrency + 1 + len(writers)) as executor:
        readiness = executor.submit(probe)
        writes = [executor.submit(author, writer) for writer in writers]
        jobs = [executor.submit(worker, i) for i in range(concurrency)]
        samples = Samples()
        try:
            for job in jobs:
                samples.merge(job.result())
        finally:
            stop.set()
        elapsed = time.perf_counter() - timing["started"]
        checks = readiness.result()
        saves, reorders = Samples(), Samples()
        for future in writes:
            saved, reordered = future.result()
            saves.merge(saved)
            reorders.merge(reordered)
    result = {"concurrency": concurrency, **samples.summary(),
              "requests_per_second": round(sum(samples.statuses.values()) / elapsed, 2),
              "seconds": round(elapsed, 3), "readyz": checks.summary()}
    if writers:
        result["writers"] = len(writers)
        result["writes"] = {"save": saves.summary(), "series_reorder": reorders.summary()}
    return result


def validate(args):
    require(all(1 <= n <= 100 for n in args.pool_sizes), "pool sizes must be 1..100")
    require(all(1 <= n <= 128 for n in args.concurrency), "concurrency must be 1..128")
    require(args.duration is not None or max(args.concurrency) <= args.requests <= 100000,
            "requests must be concurrency..100000")
    require(args.duration is None or math.isfinite(args.duration) and 1 <= args.duration <= 3600,
            "duration must be 1..3600 seconds per measurement")
    require(1 <= args.posts <= 100000, "posts must be 1..100000")
    require(1 <= args.writers <= min(args.posts, 32), "writers must be 1..32 and no more than posts")
    require(10 <= args.write_interval_ms <= 60000, "write interval must be 10..60000 ms")
    require(not args.series_reorder or args.mixed and 2 <= args.series_members <= args.posts,
            "series reorder requires --mixed and 2..posts series members")


def measurement_passed(row):
    return (row["requests"] > 0 and row["failed_operations"] == 0 and row["readyz"]["failed_operations"] == 0
            and all(value["errors"] == 0 for value in row.get("writes", {}).values())
            and ("writes" not in row or row["writes"]["save"]["requests"] > 0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/release/blog")
    parser.add_argument("--admin-dist", type=Path, default=PROJECT / "apps/admin/dist")
    parser.add_argument("--pool-sizes", type=int, nargs="+", default=[5, 10, 20])
    parser.add_argument("--concurrency", type=int, nargs="+", default=[1, 8, 32])
    parser.add_argument("--requests", type=int, default=1000)
    parser.add_argument("--duration", type=float, help="seconds per measurement, overrides request count")
    parser.add_argument("--posts", type=int, default=200)
    parser.add_argument("--mixed", action="store_true", help="save an author's published post during reads")
    parser.add_argument("--writers", type=int, default=1, help="independent editing clients on distinct posts; requires --mixed to run")
    parser.add_argument("--series-reorder", action="store_true", help="also reorder the shared series; requires --mixed")
    parser.add_argument("--series-members", type=int, default=2, help="number of shared series members")
    parser.add_argument("--write-interval-ms", type=int, default=250, help="pause after each author cycle")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    args.binary, args.admin_dist = args.binary.resolve(), args.admin_dist.resolve()
    report = {"format": 2, "status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat(),
              "platform": platform.platform(), "cpu_count": os.cpu_count(), "measurements": [],
              "fixture": {"posts": args.posts, "pages": 1, "theme": "default", "warmup_requests": 30},
              "workload": {"mode": "mixed" if args.mixed else "read",
                           "duration_seconds": args.duration if args.duration is None or math.isfinite(args.duration) else None,
                           "requests": None if args.duration is not None else args.requests,
                           "writers": args.writers if args.mixed else 0,
                           "write_interval_ms": args.write_interval_ms if args.mixed else None,
                           "series_reorder": args.series_reorder,
                           "series_members": args.series_members if args.series_reorder else 0},
              "pass_rule": "no read/readiness failures or non-409 write errors; assess write conflicts against deployment thresholds",
              "latency_method": "logarithmic histogram upper bounds, <=2% error (1us floor, rounded to 1us)"}
    try:
        require(not args.report.exists(), "report exists; choose a new path")
        validate(args)
        args.admin_url = admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", ""))
        with tempfile.TemporaryDirectory(prefix="blog-capacity-") as directory:
            site = CapacitySite(args, Path(directory))
            try:
                site.prepare()
                site.install()
                site.admin = Client(site.origin)
                site.admin.login(site.password)
                post = site.action("posts", site.create("posts", "capacity-post",
                                   content="Capacity body.\n\n" * 100), "publish")
                site.action("pages", site.create("pages", "capacity-page"), "publish")
                if args.posts > 1:
                    site.query("INSERT INTO posts (id,author_id,title,slug,content,content_html,content_render_version,status,visibility,published_at,version) "
                               "SELECT gen_random_uuid(),author_id,'Capacity '||n,'capacity-'||n,content,content_html,content_render_version,status,visibility,published_at,1 "
                               f"FROM posts CROSS JOIN generate_series(1,{args.posts - 1}) n WHERE id='{post['id']}'")
                series_slug = None
                writer_ids = site.query("SELECT id FROM posts ORDER BY (slug <> 'capacity-post'), slug "
                                        f"LIMIT {args.writers}").splitlines() if args.mixed else []
                if args.series_reorder:
                    series_slug = "capacity-series"
                    series = site.admin.json("POST", API + "/series", {"name": "Capacity", "slug": series_slug}, status=201)
                    # Bulk fixture construction only. Measured saves and reorders
                    # always use HTTP, authorization, CAS and normal transactions.
                    site.query("BEGIN; INSERT INTO post_series (post_id,series_id,position) "
                               f"SELECT id,'{series['id']}',(row_number() OVER (ORDER BY (slug <> 'capacity-post'),slug)-1)::integer "
                               f"FROM posts ORDER BY (slug <> 'capacity-post'),slug LIMIT {args.series_members}; "
                               "UPDATE posts SET version=version+1 WHERE id IN "
                               f"(SELECT post_id FROM post_series WHERE series_id='{series['id']}'); "
                               f"UPDATE series SET version=version+1 WHERE id='{series['id']}'; COMMIT;")
                writers = [MixedWriter(site.admin.clone(), post_id, post["content"], series_slug)
                           for post_id in writer_ids]
                site.stop()
                for size in args.pool_sizes:
                    site.pool_size = size
                    site.start()
                    load(site.origin, 1, 30, posts=args.posts)
                    for concurrency in args.concurrency:
                        measurement = {"pool_size": size, "min_connections": size,
                                       **load(site.origin, concurrency, args.requests, duration=args.duration,
                                              posts=args.posts, writers=writers,
                                              write_interval=args.write_interval_ms / 1000)}
                        report["measurements"].append(measurement)
                        print(json.dumps(measurement), flush=True)
                    site.stop()
                report["evidence"] = {**site.evidence, "benchmark_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
                require(all(measurement_passed(row) for row in report["measurements"]),
                        "HTTP, readiness or author operation errors under load")
                report["status"] = "passed"
            finally:
                site.cleanup()
    except (AcceptanceError, KeyboardInterrupt) as error:
        report.update(status="failed", error=str(error) if isinstance(error, AcceptanceError) else "interrupted")
    except Exception as error:
        report.update(status="failed", error="unexpected " + type(error).__name__)
    try:
        with args.report.open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
    except OSError:
        print("could not create report", file=sys.stderr)
        return 1
    if report["status"] != "passed":
        print(report.get("error", "benchmark failed"), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    def interrupt(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt)
    raise SystemExit(main())
