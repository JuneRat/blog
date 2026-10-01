#!/usr/bin/env python3
"""Compare foreground reads/edits with and without tasks in disposable Compose sites.

Uses an already built runtime image; never selects an existing project/database.
SQL constructs synthetic fixtures only. All measured edits and task submissions
use the real HTTP API; due publication uses the built-in 30-second schedule.
"""
import argparse
import datetime as dt
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid

from acceptance_support import API, AcceptanceError, Client, SiteScenario, require
from benchmark_public import MixedWriter, Samples, load, measurement_passed
from compose_recovery import dotenv

PROJECT = Path(__file__).resolve().parents[1]


def wait_for(action, timeout=60):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            result = action()
            if result:
                return result
        except AcceptanceError:
            pass
        time.sleep(.2)
    raise AcceptanceError("disposable site readiness timed out")


class ComposeSite(SiteScenario):
    def __init__(self, args, root, pool):
        self.args, self.root = args, root
        self.project = "blog-task-capacity-" + uuid.uuid4().hex[:12]
        self.password = "Capacity-" + secrets.token_hex(16) + "!"
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("BLOG_", "COMPOSE_", "PG"))
                    and key not in ("DATABASE_URL", "RUST_LOG", "GH_SECRET", "IDP_SECRET")}
        self.base = ["docker", "compose", "--project-directory", str(root), "-p", self.project]
        self.prepared = False
        root.mkdir()
        (root / "ops").mkdir()
        shutil.copyfile(PROJECT / "compose.yaml", root / "compose.yaml")
        shutil.copyfile(PROJECT / "ops/postgres-init.sh", root / "ops/postgres-init.sh")
        values = {
            "COMPOSE_PROJECT_NAME": self.project, "BLOG_IMAGE": args.image,
            "BLOG_POSTGRES_PASSWORD": secrets.token_hex(32), "BLOG_OWNER_PASSWORD": secrets.token_hex(32),
            "BLOG_HTTP_HOST": "127.0.0.1", "BLOG_HTTP_PORT": "0", "BLOG_METRICS_PORT": "0",
            "BLOG_SECURE_COOKIES": "0", "BLOG_DB_MAX_CONNECTIONS": str(pool),
            "BLOG_DB_MIN_CONNECTIONS": str(pool), "BLOG_DB_STATEMENT_TIMEOUT_MS": "20000",
            "BLOG_DB_LOCK_TIMEOUT_MS": "3000", "RUST_LOG": "warn", "BLOG_LOG_FORMAT": "json",
        }
        env_file = root / ".env"
        env_file.write_text(dotenv(values))
        env_file.chmod(0o600)
        self.owner_password = values["BLOG_OWNER_PASSWORD"]

    def compose(self, *args):
        try:
            result = subprocess.run([*self.base, *args], cwd=self.root, env=self.env,
                                    capture_output=True, text=True, timeout=180, check=False)
        except subprocess.TimeoutExpired:
            raise AcceptanceError("disposable Compose operation timed out") from None
        require(result.returncode == 0, f"disposable Compose {args[0]} failed")
        return result.stdout.strip()

    def query(self, sql):
        return self.compose("exec", "-T", "db", "psql", "-XAt", "-v", "ON_ERROR_STOP=1",
                            "-U", "postgres", "-d", "blog", "-c", sql)

    def client(self, port="8080"):
        address = self.compose("port", "blog", port)
        require(re.fullmatch(r"127\.0\.0\.1:\d+", address), "unexpected Compose published address")
        return Client("http://" + address)

    def prepare(self):
        # Record ownership before 'up': partial container creation also needs cleanup.
        self.prepared = True
        self.compose("up", "-d", "--no-build", "--pull", "never", "blog")
        self.guest = self.client()
        token = wait_for(lambda: re.search(r"安装码：([a-f0-9]{64})", self.compose("logs", "--no-color", "blog")))
        self.guest.json("POST", "/api/install", {
            "database_url": f"postgres://blog_owner:{self.owner_password}@db:5432/blog",
            "public_base_url": self.guest.origin, "username": "acceptance-owner", "password": self.password,
        }, headers={"X-Install-Token": token.group(1)})
        wait_for(lambda: self.guest.request("GET", "/readyz"))
        self.admin = self.client()
        self.admin.login(self.password)
        self.metrics = self.client("9090")
        self.build = self.guest.json("GET", "/version")
        if self.args.expected_revision:
            require(self.build["revision"] == self.args.expected_revision, "runtime revision differs from expected build")

    def cleanup(self):
        if self.prepared:
            self.compose("down", "--volumes", "--remove-orphans")
            self.prepared = False

    def fixture(self):
        post = self.action("posts", self.create("posts", "capacity-post", content="Capacity **body**.\n\n" * 100), "publish")
        self.action("pages", self.create("pages", "capacity-page"), "publish")
        self.query("INSERT INTO posts(id,author_id,title,slug,content,content_html,content_render_version,status,visibility,published_at,version) "
                   "SELECT gen_random_uuid(),author_id,'Capacity '||n,'capacity-'||n,content,content_html,content_render_version,status,visibility,published_at,1 "
                   f"FROM posts CROSS JOIN generate_series(1,{self.args.posts - 1}) n WHERE id='{post['id']}'")
        self.post = post
        self.content_marker = int(self.query(f"SELECT content_render_version FROM posts WHERE id='{post['id']}'"))
        policy = self.admin.json("GET", API + "/access-settings")
        self.admin.json("PUT", API + "/access-settings", {**policy, "guest_comments_enabled": True})
        self.guest.json("POST", "/api/v1/posts/capacity-post/comments", {
            "nickname": "Capacity seed", "body": "Seed comment", "parent_id": None,
        }, status=202)
        self.comment_marker = int(self.query("SELECT content_render_version FROM comments WHERE author_name='Capacity seed'"))
        self.query("UPDATE comments SET ip_address='192.0.2.11' WHERE author_name='Capacity seed'")
        ids = self.query("SELECT id FROM posts ORDER BY (slug<>'capacity-post'),slug "
                         f"LIMIT {self.args.writers}").splitlines()
        return [MixedWriter(self.admin.clone(), item, post["content"]) for item in ids]


def metric_values(body):
    """Select bounded pool/render/task series; exclude arbitrary request labels."""
    values = {}
    for line in body.decode().splitlines():
        if not line.startswith(("blog_database_pool_", "blog_render_", "blog_task_")):
            continue
        match = re.fullmatch(r"(\w+(?:\{.*\})?)\s+([-+\deE.]+)(?:\s+\d+)?", line)
        if match and "_bucket{" not in match[1]:
            value = float(match[2])
            if math.isfinite(value):
                values[match[1]] = value
    return values


def memory_bytes(value):
    match = re.match(r"([\d.]+)([KMGT]?i?B)", value.strip())
    require(match is not None, "unrecognized Docker memory units")
    units = {"B": 1, "kB": 1000, "KB": 1000, "MB": 1000**2, "GB": 1000**3,
             "TB": 1000**4, "KiB": 1024, "MiB": 1024**2, "GiB": 1024**3, "TiB": 1024**4}
    return round(float(match[1]) * units[match[2]])


class Monitor:
    def __init__(self, site):
        self.site, self.stop = site, threading.Event()
        self.metrics, self.resources, self.errors = {}, {}, 0

    def sample(self):
        values = metric_values(self.site.metrics.request("GET", "/metrics")[0])
        for key, value in values.items():
            row = self.metrics.setdefault(key, {"first": value, "min": value, "max": value, "last": value})
            row.update(min=min(row["min"], value), max=max(row["max"], value), last=value)
        containers = self.site.compose("ps", "-q", "blog", "db").splitlines()
        require(len(containers) == 2 and all(re.fullmatch(r"[0-9a-f]{12,64}", item) for item in containers),
                "resource sampling requires both owned Compose containers")
        result = subprocess.run(["docker", "stats", "--no-stream", "--format", "{{json .}}", *containers],
                                capture_output=True, text=True, timeout=15, check=False)
        require(result.returncode == 0, "Docker resource sampling failed")
        observed = set()
        for line in result.stdout.splitlines():
            item = json.loads(line)
            name = re.fullmatch(re.escape(self.site.project) + r"-(blog|db)-\d+", item["Name"])
            require(name is not None, "resource sample does not belong to this Compose project")
            service = name[1]
            observed.add(service)
            row = self.resources.setdefault(service, {"peak_memory_bytes": 0, "peak_cpu_percent": 0})
            row["peak_memory_bytes"] = max(row["peak_memory_bytes"], memory_bytes(item["MemUsage"].split("/")[0]))
            row["peak_cpu_percent"] = max(row["peak_cpu_percent"], float(item["CPUPerc"].rstrip("%")))
        require(observed == {"blog", "db"}, "resource sample missing an owned service")

    def run(self):
        started = time.monotonic()
        next_progress = started + 50
        while not self.stop.is_set():
            try:
                self.sample()
            except (AcceptanceError, OSError, ValueError, KeyError, subprocess.TimeoutExpired):
                self.errors += 1
            if time.monotonic() >= next_progress:
                print(json.dumps({"progress_seconds": round(time.monotonic() - started),
                                  "scheduler_available": self.metrics.get("blog_task_scheduler_available", {}).get("last"),
                                  "completed_transitions": {key: row["last"] for key, row in self.metrics.items()
                                                            if key.startswith("blog_task_finished_total{") and row["last"] > 0},
                                  "monitor_errors": self.errors}), flush=True)
                next_progress = time.monotonic() + 50
            if self.stop.wait(5):
                break


class Background:
    def __init__(self, site):
        self.site, self.stop = site, threading.Event()
        self.client = site.admin.clone()
        self.runs, self.cycles, self.error = [], 0, None
        self.skipped_rebuild_fixtures = 0
        self.result = {}
        self.submit = Samples()
        self.fixture_sql = Samples()
        self.observations = 0

    def cycle(self):
        n = self.cycles
        site = self.site
        # Fixture SQL changes only derived markers and creates synthetic expired /
        # scheduled data. Executions themselves go through production transactions.
        marker = site.content_marker
        began = time.perf_counter()
        active = site.query("SELECT count(*) FROM task_runs WHERE kind='html_rebuild' AND status IN ('queued','running')") != "0"
        if not active:
            site.query(f"UPDATE posts SET content_render_version={marker + 1} WHERE slug LIKE 'capacity-%'")
        else:
            # Submit/coalesce normally, but never invalidate already rebuilt rows
            # while this same synthetic migration is still being processed.
            self.skipped_rebuild_fixtures += 1
        site.query("INSERT INTO audit_logs(id,action,target_type,target_id,created_at) "
                   "SELECT gen_random_uuid(),'synthetic.capacity.expired','system','capacity',now()-interval '400 days' FROM generate_series(1,100); "
                   "INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version,ip_address,created_at) "
                   f"SELECT gen_random_uuid(),'{site.post['id']}','Capacity expired','preserved','<p>preserved</p>',{site.comment_marker},'192.0.2.10',now()-interval '400 days' FROM generate_series(1,100); "
                   "INSERT INTO posts(id,author_id,title,slug,content,content_html,content_render_version,status,published_at) "
                   f"SELECT gen_random_uuid(),author_id,'Scheduled capacity','task-capacity-{n}-'||g,'body','<p>body</p>',{marker},'scheduled',clock_timestamp()+interval '5 seconds' "
                   f"FROM posts CROSS JOIN generate_series(1,{site.args.scheduled_per_cycle}) g WHERE id='{site.post['id']}'")
        self.fixture_sql.add(200, time.perf_counter() - began)
        at = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=3)).isoformat()
        for kind in ("html_rebuild", "retention"):
            started = time.perf_counter()
            run = self.client.json("POST", API + "/tasks", {"kind": kind, "run_at": at if kind == "html_rebuild" else None}, status=202)
            self.submit.add(200, time.perf_counter() - started)
            if run["id"] not in self.runs:
                self.runs.append(run["id"])
        self.cycles += 1

    def observe_publication(self):
        # Database-visible published rows provide an observed upper bound that
        # includes claim/connection/lock/commit waits, unlike business updated_at.
        self.site.query("INSERT INTO capacity_publication_observations(post_id,visible_at) "
                        "SELECT id,clock_timestamp() FROM posts WHERE slug LIKE 'task-capacity-%' AND status='published' "
                        "ON CONFLICT(post_id) DO NOTHING")
        self.observations += 1

    def run(self):
        try:
            self.site.query("CREATE TABLE capacity_publication_observations(post_id uuid PRIMARY KEY REFERENCES posts(id),visible_at timestamptz NOT NULL)")
            while not self.stop.is_set():
                self.cycle()
                until = time.monotonic() + self.site.args.task_interval
                while not self.stop.wait(min(5, max(0, until - time.monotonic()))):
                    self.observe_publication()
                    if time.monotonic() >= until:
                        break
        except Exception as error:
            self.error = "background " + type(error).__name__

    def finish(self, foreground_window=None):
        require(self.error is None, self.error or "background failed")
        require(self.cycles > 0, "background workload did not run")
        ids = ",".join("'" + str(uuid.UUID(item)) + "'" for item in self.runs)
        wait_for(lambda: self.site.query(f"SELECT count(*) FROM task_runs WHERE id IN ({ids}) AND status NOT IN ('completed','failed','interrupted','cancelled')") == "0")
        wait_for(lambda: self.site.query("SELECT count(*) FROM posts WHERE slug LIKE 'task-capacity-%' AND status<>'published'") == "0")
        self.observe_publication()
        overlap = "false"
        if foreground_window:
            start, end = (dt.datetime.fromisoformat(value).isoformat() for value in foreground_window)
            overlap = f"finished_at BETWEEN '{start}'::timestamptz AND '{end}'::timestamptz"
        runs = json.loads(self.site.query(f"SELECT jsonb_agg(jsonb_build_object('kind',kind,'status',status,'report',report,'queue_seconds',greatest(0,extract(epoch FROM started_at-run_at)),'execution_seconds',extract(epoch FROM finished_at-started_at),'finished_during_foreground',{overlap})) FROM task_runs WHERE id IN ({ids})")) or []
        pending = self.client.json("GET", API + "/tasks?kind=html_rebuild")["pending_html"]
        publication = json.loads(self.site.query("SELECT jsonb_build_object('expected',count(*),'published',count(*) FILTER(WHERE status='published'),'observed',count(o.post_id),'max_delay_seconds',max(extract(epoch FROM o.visible_at-p.published_at)),'p95_delay_seconds',percentile_cont(0.95) WITHIN GROUP(ORDER BY extract(epoch FROM o.visible_at-p.published_at)),'business_max_delay_seconds',max(extract(epoch FROM p.updated_at-p.published_at))) FROM posts p LEFT JOIN capacity_publication_observations o ON o.post_id=p.id WHERE p.slug LIKE 'task-capacity-%'"))
        cleanup = json.loads(self.site.query("SELECT jsonb_build_object('remaining_comment_ips',(SELECT count(*) FROM comments WHERE author_name='Capacity expired' AND ip_address IS NOT NULL),'expired_comments_preserved',(SELECT count(*) FROM comments WHERE author_name='Capacity expired' AND content='preserved'),'seed_ip_preserved',(SELECT host(ip_address)='192.0.2.11' FROM comments WHERE author_name='Capacity seed'),'remaining_audit_rows',(SELECT count(*) FROM audit_logs WHERE action='synthetic.capacity.expired'))"))
        self.result = {"cycles": self.cycles, "submissions": self.submit.summary(), "runs": runs,
                       "requested_unique_runs": len(self.runs),
                       "publication": publication, "cleanup": cleanup, "pending_html": pending,
                       "skipped_rebuild_fixture_cycles": self.skipped_rebuild_fixtures,
                       "fixture_sql": self.fixture_sql.summary(), "publication_observations": self.observations,
                       "publication_observation_interval_seconds": 5}
        require(len(runs) == len(self.runs),
                "task history is incomplete; shorten the test or increase the task interval to avoid history pruning")
        require(all(run["status"] == "completed" for run in runs), "background task failed or interrupted")
        require(all(run["report"]["html"]["failure"] is None for run in runs if run["kind"] == "html_rebuild"), "HTML task reported failed rows")
        require(pending is not None and all(value == 0 for value in pending.values()),
                "HTML background workload left an unfinished rebuild backlog")
        require(sum(run["report"]["html"]["rebuilt"]["posts"] for run in runs if run["kind"] == "html_rebuild") > 0,
                "HTML background load did not rebuild any posts")
        require(publication["expected"] == self.cycles * self.site.args.scheduled_per_cycle
                and publication["published"] == publication["expected"] and publication["observed"] == publication["expected"], "publication workload fixture was not fully processed")
        require(cleanup["remaining_comment_ips"] == 0 and cleanup["remaining_audit_rows"] == 0
                and cleanup["expired_comments_preserved"] == self.cycles * 100 and cleanup["seed_ip_preserved"],
                "retention did not preserve comments or finish fixture cleanup")
        require(all(any(run["kind"] == kind and run["finished_during_foreground"] for run in runs)
                    for kind in ("html_rebuild", "retention")), "background work only completed after foreground load stopped")
        return self.result


def validate(args):
    require(bool(args.image) and not args.image.startswith("-"), "explicit runtime image required")
    require(all(1 <= value <= 100 for value in args.pool_sizes), "pool sizes must be 1..100")
    require(len(set(args.pool_sizes)) == len(args.pool_sizes), "pool sizes must be unique")
    require(args.expected_revision is None or re.fullmatch(r"[0-9a-f]{40}", args.expected_revision), "expected revision must be a full commit SHA")
    require(1 <= args.concurrency <= 128, "concurrency must be 1..128")
    require(2 <= args.posts <= 100000 and 1 <= args.writers <= min(32, args.posts), "invalid post/writer count")
    require(math.isfinite(args.duration) and 5 <= args.duration <= 3600, "duration must be 5..3600 seconds per measurement")
    require(10 <= args.write_interval_ms <= 60000, "write interval must be 10..60000 ms")
    require(math.isfinite(args.connection_lifetime_seconds) and 0 < args.connection_lifetime_seconds <= 240,
            "connection lifetime must be positive and at most 240 seconds (server default age is 300)")
    require(5 <= args.task_interval <= 600 and 1 <= args.scheduled_per_cycle <= 1000, "invalid background fixture cadence")
    require(all(math.isfinite(value) and value > 0 for value in
                (args.read_p95_ms, args.write_p95_ms, args.publication_delay_seconds)), "thresholds must be finite positive values")


def failed_checks(row, args):
    saves = row["writes"]["save"]
    checks = {"read_readiness_or_write_errors": measurement_passed(row),
              "no_successful_edits": saves["requests"] > saves["conflicts"],
              "read_p95": row["p95_ms"] is not None and row["p95_ms"] <= args.read_p95_ms,
              "write_p95": saves["p95_ms"] is not None and saves["p95_ms"] <= args.write_p95_ms,
              "monitor_errors": row["monitor"]["errors"] == 0,
              "scheduler_unavailable": row["monitor"]["metrics"].get("blog_task_scheduler_available", {}).get("last") == 1,
              "background_validation": "background_error" not in row}
    errors = row["monitor"]["metrics"].get('blog_task_scheduler_checks_total{result="error"}', {})
    checks["scheduler_errors"] = errors.get("last", 0) == errors.get("first", 0)
    if "background" in row and "background_error" not in row:
        delay = row["background"]["publication"]["max_delay_seconds"]
        checks["publication_delay"] = delay is not None and delay <= args.publication_delay_seconds
    return [name for name, success in checks.items() if not success]


def passed(row, args):
    return not failed_checks(row, args)


def measure(args, root, pool, tasks):
    site = ComposeSite(args, root, pool)
    try:
        site.prepare()
        writers = site.fixture()
        load(site.guest.origin, 1, 30, posts=args.posts, connection_lifetime=args.connection_lifetime_seconds)
        monitor = Monitor(site)
        background = Background(site) if tasks else None
        threads = [threading.Thread(target=monitor.run, daemon=True)]
        if background:
            threads.append(threading.Thread(target=background.run, daemon=True))
        started_threads = []
        try:
            for thread in threads:
                thread.start()
                started_threads.append(thread)
            began = json.loads(site.query("SELECT to_json(clock_timestamp())"))
            row = {"pool_size": pool, "background_tasks": tasks, "build": site.build,
                   **load(site.guest.origin, args.concurrency, 1000, duration=args.duration,
                          posts=args.posts, writers=writers, write_interval=args.write_interval_ms / 1000,
                          connection_lifetime=args.connection_lifetime_seconds)}
            ended = json.loads(site.query("SELECT to_json(clock_timestamp())"))
            row["foreground_window"] = [began, ended]
        finally:
            if background:
                background.stop.set()
            monitor.stop.set()
            try:
                if background and threads[-1] in started_threads:
                    threads[-1].join(timeout=180)
            finally:
                if threads[0] in started_threads:
                    threads[0].join(timeout=30)
            if background:
                require(not threads[-1].is_alive(), "background worker did not stop")
            require(not threads[0].is_alive(), "monitor did not stop")
        if background:
            try:
                row["background"] = background.finish(row["foreground_window"])
            except Exception as error:
                row["background"] = background.result
                row["background_error"] = str(error) if isinstance(error, AcceptanceError) else "background " + type(error).__name__
        # Include terminal counters after draining; peaks during the measured
        # interval still come from the five-second sampling loop above.
        try:
            monitor.sample()
        except (AcceptanceError, OSError, ValueError, KeyError, subprocess.TimeoutExpired):
            monitor.errors += 1
        row["monitor"] = {"metrics": monitor.metrics, "resources": monitor.resources, "errors": monitor.errors}
        row["passed"] = passed(row, args)
        row["failed_checks"] = failed_checks(row, args)
        return row
    finally:
        site.cleanup()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--expected-revision")
    parser.add_argument("--pool-sizes", type=int, nargs="+", default=[5, 10])
    parser.add_argument("--concurrency", type=int, default=16)
    parser.add_argument("--posts", type=int, default=2000)
    parser.add_argument("--writers", type=int, default=4)
    parser.add_argument("--duration", type=float, default=300)
    parser.add_argument("--write-interval-ms", type=int, default=250)
    parser.add_argument("--connection-lifetime-seconds", type=float, default=60)
    parser.add_argument("--task-interval", type=int, default=15)
    parser.add_argument("--scheduled-per-cycle", type=int, default=16)
    parser.add_argument("--read-p95-ms", type=float, default=500)
    parser.add_argument("--write-p95-ms", type=float, default=2000)
    parser.add_argument("--publication-delay-seconds", type=float, default=45)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = {"format": 1, "status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat(),
              "cpu_count": os.cpu_count(), "image": args.image,
              "workload": {key: value for key, value in vars(args).items() if key not in ("report", "image")},
              "measurements": [], "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "load_runner_sha256": hashlib.sha256((PROJECT / "scripts/benchmark_public.py").read_bytes()).hexdigest(),
              "threshold_scope": "local regression gates, not production SLA; CAS conflicts counted separately"}
    try:
        require(not args.report.exists(), "report exists; choose a new path")
        validate(args)
        report["platform"] = platform.platform()
        metadata = subprocess.run(["docker", "image", "inspect", args.image, "--format", "{{json .}}"],
                                  capture_output=True, text=True, timeout=30, check=False)
        require(metadata.returncode == 0, "runtime image must already exist locally")
        info = json.loads(metadata.stdout)
        report["image_metadata"] = {"id": info["Id"], "os": info["Os"], "architecture": info["Architecture"],
                                    "revision": info["Config"]["Labels"].get("org.opencontainers.image.revision")}
        with tempfile.TemporaryDirectory(prefix="blog-task-capacity-") as temporary:
            for pool in args.pool_sizes:
                for tasks in (False, True):
                    print(f"==> pool={pool}, background={tasks}, duration={args.duration}s", flush=True)
                    row = measure(args, Path(temporary) / f"pool-{pool}-tasks-{tasks}", pool, tasks)
                    report["measurements"].append(row)
                    print(json.dumps({key: row[key] for key in ("pool_size", "background_tasks", "requests", "requests_per_second", "p95_ms", "p99_ms", "passed", "failed_checks")}), flush=True)
        require(all(row["passed"] for row in report["measurements"]), "local workload thresholds or operation checks failed")
        report["status"] = "passed"
    except (AcceptanceError, KeyboardInterrupt) as error:
        report["error"] = str(error) if isinstance(error, AcceptanceError) else "interrupted"
    except Exception as error:
        report["error"] = "unexpected " + type(error).__name__
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
