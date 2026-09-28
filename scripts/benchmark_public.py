#!/usr/bin/env python3
"""Compare pool sizes on a disposable installed site, never an existing site.

Uses BLOG_TEST_ADMIN_URL (loopback only) and optionally BLOG_TEST_PG_CONTAINER,
like acceptance.py. Creates and removes its own random database. Measurements
include client overhead and are local capacity evidence, not a production SLA.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import datetime as dt
import http.client
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit

from acceptance import Acceptance, AcceptanceError, Client, PROJECT, admin_url, require


class CapacitySite(Acceptance):
    pool_size = 5

    def env(self, *args, **kwargs):
        env = super().env(*args, **kwargs)
        env.update(BLOG_DB_MAX_CONNECTIONS=str(self.pool_size),
                   BLOG_DB_MIN_CONNECTIONS=str(self.pool_size),
                   BLOG_DB_STATEMENT_TIMEOUT_MS="30000", BLOG_DB_LOCK_TIMEOUT_MS="5000")
        return env


def summarize(samples):
    times = sorted(elapsed for _, elapsed in samples)
    return {"requests": len(times), "errors": sum(status != 200 for status, _ in samples),
            "p50_ms": round(times[math.ceil(len(times) * .50) - 1] * 1000, 2),
            "p95_ms": round(times[math.ceil(len(times) * .95) - 1] * 1000, 2),
            "p99_ms": round(times[math.ceil(len(times) * .99) - 1] * 1000, 2)}


def load(origin, concurrency, requests):
    address = urlsplit(origin)
    paths = ("/", "/posts/capacity-post", "/capacity-page")
    barrier = threading.Barrier(concurrency + 1)
    stop = threading.Event()

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
        samples = []
        try:
            barrier.wait(timeout=30)
            for n in range(index, requests, concurrency):
                if stop.is_set():
                    break
                samples.append(fetch(connection, paths[n % len(paths)]))
        finally:
            connection.close()
        return samples

    def probe():
        connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
        samples = []
        try:
            barrier.wait(timeout=30)
            while True:
                samples.append(fetch(connection, "/readyz"))
                if stop.wait(.05):
                    return samples
        finally:
            connection.close()

    with ThreadPoolExecutor(max_workers=concurrency + 1) as executor:
        readiness = executor.submit(probe)
        started = time.perf_counter()
        jobs = [executor.submit(worker, i) for i in range(concurrency)]
        try:
            samples = [sample for job in jobs for sample in job.result()]
        finally:
            stop.set()
        elapsed = time.perf_counter() - started
        checks = readiness.result()
    return {"concurrency": concurrency, **summarize(samples),
            "requests_per_second": round(requests / elapsed, 2), "seconds": round(elapsed, 3),
            "readyz": summarize(checks)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/release/blog")
    parser.add_argument("--admin-dist", type=Path, default=PROJECT / "apps/admin/dist")
    parser.add_argument("--pool-sizes", type=int, nargs="+", default=[5, 10, 20])
    parser.add_argument("--concurrency", type=int, nargs="+", default=[1, 8, 32])
    parser.add_argument("--requests", type=int, default=1000)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    args.binary, args.admin_dist = args.binary.resolve(), args.admin_dist.resolve()
    report = {"format": 1, "status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat(),
              "platform": platform.platform(), "cpu_count": os.cpu_count(), "measurements": [],
              "fixture": "200 published posts, 1 page, default theme; mixed index/post/page, 30 warmup requests"}
    try:
        require(not args.report.exists(), "report exists; choose a new path")
        require(all(1 <= n <= 100 for n in args.pool_sizes), "pool sizes must be 1..100")
        require(all(1 <= n <= 128 for n in args.concurrency), "concurrency must be 1..128")
        require(max(args.concurrency) <= args.requests <= 100000, "requests must be concurrency..100000")
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
                site.query("INSERT INTO posts (id,author_id,title,slug,content,content_html,content_render_version,status,visibility,published_at,version) "
                           "SELECT gen_random_uuid(),author_id,'Capacity '||n,'capacity-'||n,content,content_html,content_render_version,status,visibility,published_at,1 "
                           f"FROM posts CROSS JOIN generate_series(1,199) n WHERE id='{post['id']}'")
                site.stop()
                for size in args.pool_sizes:
                    site.pool_size = size
                    site.start()
                    load(site.origin, 1, 30)
                    for concurrency in args.concurrency:
                        measurement = {"pool_size": size, "min_connections": size, **load(site.origin, concurrency, args.requests)}
                        report["measurements"].append(measurement)
                        print(json.dumps(measurement), flush=True)
                    site.stop()
                report["evidence"] = {**site.evidence, "benchmark_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
                require(all(row["errors"] == 0 and row["readyz"]["errors"] == 0
                            for row in report["measurements"]), "HTTP or readiness errors under load")
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
