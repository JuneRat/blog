#!/usr/bin/env python3
"""Real HTTP acceptance: install, author, moderate, back up and restore a fresh site.

Requires BLOG_TEST_ADMIN_URL on loopback, a built blog binary and admin SPA.
BLOG_TEST_PG_CONTAINER selects PostgreSQL tools inside the same test container.
Only randomly named databases created by this run are removed. Existing site
configuration, databases and media are never used. Reports contain no credentials.
"""

import argparse
import base64
import copy
import datetime as dt
from http.cookiejar import CookieJar
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, HTTPCookieProcessor, ProxyHandler, Request, build_opener
import uuid

import recovery

PROJECT = Path(__file__).resolve().parent.parent
API = "/api/admin/v1"
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=")


class AcceptanceError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise AcceptanceError(message)


def clean_env():
    # Never inherit a developer's installed site, recovery mode, database service
    # file or proxy routing. PostgreSQL credentials are supplied explicitly later.
    return {key: value for key, value in os.environ.items()
            if key != "DATABASE_URL" and not key.startswith(("BLOG_", "PG"))}


def admin_url(value):
    try:
        config = recovery.db_config(value)
        parsed = urlsplit(value)
        require(config["PGHOST"] in ("127.0.0.1", "localhost", "::1"),
                "BLOG_TEST_ADMIN_URL must point to a loopback test instance")
        require(not parsed.query and not parsed.fragment,
                "BLOG_TEST_ADMIN_URL must not contain connection overrides")
        return value
    except (ValueError, recovery.RecoveryError):
        raise AcceptanceError("set an explicit PostgreSQL BLOG_TEST_ADMIN_URL") from None


def database_url(url, name):
    require(bool(recovery.SAFE_NAME.fullmatch(name)), "invalid test database name")
    parsed = urlsplit(url)
    return urlunsplit((parsed.scheme, parsed.netloc, "/" + name, "", ""))


def run_command(command, env, label, success=True, timeout=120):
    # Do not echo arguments, environments or subprocess output: installation
    # logs include the one-time token and failures can include connection URLs.
    try:
        result = subprocess.run(command, cwd=PROJECT, env=env, capture_output=True,
                                text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        raise AcceptanceError(f"{label}: timed out") from None
    require((result.returncode == 0) == success,
            f"{label}: unexpected exit status {result.returncode}")
    return result.stdout


class TestDatabase(recovery.PgTools):
    def execute(self, tool, args, database="postgres"):
        command, env = self.command(tool, args, database)
        safe_env = clean_env()
        if not self.container:
            safe_env.update({key: value for key, value in env.items() if key in self.config})
        return run_command(command, safe_env, tool, timeout=30).strip()

    def query(self, sql, database=None):
        return self.execute("psql", ["-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", sql],
                            database or self.config["PGDATABASE"])


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class AdminAssets(HTMLParser):
    def __init__(self):
        super().__init__()
        self.paths = set()

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "script" and attrs.get("src"):
            self.paths.add(attrs["src"])
        if tag == "link" and attrs.get("rel") in ("stylesheet", "modulepreload"):
            self.paths.add(attrs["href"])


class Client:
    def __init__(self, origin):
        self.origin = origin
        self.cookies = CookieJar()
        self.csrf = None
        self.opener = build_opener(ProxyHandler({}), NoRedirect(), HTTPCookieProcessor(self.cookies))

    def clone(self):
        other = Client(self.origin)
        other.csrf = self.csrf
        for cookie in self.cookies:
            other.cookies.set_cookie(copy.copy(cookie))
        return other

    def request(self, method, path, body=None, status=200, headers=None):
        require(path.startswith("/") and not path.startswith("//"), "HTTP path must be local")
        request_headers = {"Origin": self.origin}
        if self.csrf:
            request_headers["X-CSRF-Token"] = self.csrf
        if body is not None and not isinstance(body, bytes):
            body = json.dumps(body).encode()
            request_headers["Content-Type"] = "application/json"
        request_headers.update(headers or {})
        request = Request(self.origin + path, data=body, method=method, headers=request_headers)
        try:
            response = self.opener.open(request, timeout=30)
        except HTTPError as error:
            response = error
        except (URLError, TimeoutError):
            raise AcceptanceError(f"{method} {path}: connection failed") from None
        with response:
            raw = response.read()
            actual = response.status
            response_headers = response.headers
        code = ""
        if actual != status:
            try:
                candidate = json.loads(raw).get("code", "")
                if isinstance(candidate, str) and re.fullmatch(r"[a-z_]+", candidate):
                    code = f" ({candidate})"
            except (ValueError, AttributeError):
                pass
        require(actual == status, f"{method} {path}: HTTP {actual}{code}, expected {status}")
        return raw, response_headers

    def json(self, method, path, body=None, status=200, headers=None):
        raw, _ = self.request(method, path, body, status, headers)
        return json.loads(raw) if raw else None

    def me(self):
        result = self.json("GET", API + "/me")
        self.csrf = result["csrf_token"]
        return result

    def login(self, password):
        self.json("POST", "/auth/login/password", {"username": "acceptance-owner", "password": password})
        return self.me()


class Acceptance:
    def __init__(self, args, root):
        self.args = args
        self.root = root
        self.pg = TestDatabase(admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", "")),
                               os.environ.get("BLOG_TEST_PG_CONTAINER"))
        suffix = uuid.uuid4().hex
        self.source = "blog_acceptance_" + suffix
        self.target = "blog_restore_acceptance_" + suffix
        self.owned = set()
        self.process = None
        self.log = None
        self.bind = "127.0.0.1:0"
        self.origin = None
        self.password = "Test-" + secrets.token_hex(16) + "!"
        self.config = root / "config.toml"
        self.media_dir = root / "media"
        self.restore_dir = root / "restore"
        self.backup_dir = root / "backup"
        self.steps = []
        self.evidence = {}

    def env(self, database=None, restored=False, isolated=False):
        env = clean_env()
        theme = self.restore_dir / "resources/installed-themes/default" if restored else PROJECT / "themes/default"
        media = self.restore_dir / "resources/media" if restored else self.media_dir
        env.update(BLOG_CONFIG_FILE=str(self.config), BLOG_ADMIN_DIST=str(self.args.admin_dist),
                   BLOG_MIGRATIONS_DIR=str(PROJECT / "migrations/postgres"),
                   BLOG_THEME_DIR=str(theme), BLOG_MEDIA_DIR=str(media), BLOG_SECURE_COOKIES="0",
                   BLOG_RECOVERY_MODE="1" if isolated else "0", RUST_LOG="warn")
        if database:
            env["DATABASE_URL"] = database_url(self.args.admin_url, database)
        return env

    def stage(self, name, action):
        print(f"==> {name}", flush=True)
        start = time.monotonic()
        item = {"name": name, "status": "failed"}
        self.steps.append(item)
        try:
            action()
            item["status"] = "passed"
        finally:
            item["seconds"] = round(time.monotonic() - start, 3)

    def query(self, sql, restored=False):
        return self.pg.query(sql, self.target if restored else self.source)

    def prepare(self):
        require(self.args.binary.is_file(), "build the server first: cargo build -p server --bin blog")
        require((self.args.admin_dist / "index.html").is_file(),
                "build the admin SPA first: pnpm --dir apps/admin build")
        self.evidence.update(server_sha256=recovery.digest(self.args.binary),
                             admin_entry_sha256=recovery.digest(self.args.admin_dist / "index.html"),
                             runner_sha256=recovery.digest(Path(__file__)))
        for name in (self.source, self.target):
            require(not self.pg.query(f"SELECT 1 FROM pg_database WHERE datname='{name}'", "postgres"),
                    "random test database already exists; refusing to reuse it")
        self.pg.execute("createdb", ["--template=template0", self.source])
        self.owned.add(self.source)

    def start(self, database=None, restored=False, isolated=False, installer=False):
        require(self.process is None, "previous server must stop before starting another")
        path = self.root / f"server-{len(self.steps)}.log"
        self.log = path.open("wb")
        self.process = subprocess.Popen([str(self.args.binary), "serve", "--addr", self.bind],
                                        cwd=PROJECT, env=self.env(database, restored, isolated),
                                        stdout=self.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            require(self.process.poll() is None, "server exited before becoming ready")
            log = path.read_text(errors="replace")
            pattern = r"首次安装：(http://127\.0\.0\.1:\d+)/install" if installer else r"公开站点已启动：(http://127\.0\.0\.1:\d+)"
            address = re.search(pattern, log)
            token = re.search(r"安装码：([a-f0-9]{64})", log)
            if address and (token or not installer):
                self.origin = address.group(1)
                self.bind = urlsplit(self.origin).netloc
                self.guest = Client(self.origin)
                return token.group(1) if token else None
            time.sleep(0.05)
        raise AcceptanceError("server readiness timed out")

    def stop(self):
        if self.process is not None:
            process = self.process
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None

    def install(self):
        token = self.start(installer=True)
        _, headers = self.guest.request("GET", "/", status=303)
        require(headers.get("Location") == "/install", "fresh site must redirect to installation")
        self.guest.request("GET", "/healthz", status=503)
        self.guest.request("GET", "/install")
        info = self.guest.json("GET", "/api/install")
        require(info["database_configured"] is False, "installation must start without saved database")
        installed = self.guest.json("POST", "/api/install", {
            "database_url": database_url(self.args.admin_url, self.source), "public_base_url": self.origin,
            "username": "acceptance-owner", "password": self.password,
        }, headers={"X-Install-Token": token})
        require(installed["redirect"] == "/admin/", "installation must return a working admin entry")
        self.assert_installed()
        require(self.config.stat().st_mode & 0o077 == 0, "saved database credentials must be private")
        require(not self.config.with_suffix(".install-state.json").exists(),
                "completed installation must remove its temporary journal")
        config = self.config.read_text()
        require(self.password not in config and token not in config,
                "deployment configuration must not persist passwords or one-time tokens")
        self.installation_id = self.query("SELECT value->>'id' FROM settings WHERE key='installation'")
        require(re.fullmatch(r"[0-9a-f]{64}", self.installation_id) is not None,
                "database must retain the installation completion marker")
        require(self.query("SELECT count(*) FROM audit_logs WHERE action='installation.complete'") == "1",
                "first Owner and installation completion must be audited once")

    def assert_installed(self):
        self.guest.request("GET", "/healthz")
        raw, _ = self.guest.request("GET", "/admin/")
        assets = AdminAssets()
        assets.feed(raw.decode())
        require(bool(assets.paths), "admin entry must reference the production build")
        for path in sorted(assets.paths):
            require(path.startswith("/admin/assets/"), "admin assets must be served locally")
            body, headers = self.guest.request("GET", path)
            require(body and headers.get_content_type() != "text/html", "admin asset must not fall back to SPA HTML")
        self.guest.request("GET", "/api/install", status=404)
        _, headers = self.guest.request("GET", "/install", status=303)
        require(headers.get("Location") == "/admin/", "completed install must redirect to admin")

    def identity(self):
        self.admin = Client(self.origin)
        me = self.admin.login(self.password)
        require({"ownership.manage", "audit.read", "settings.manage"} <= set(me["permissions"]),
                "installed Owner must receive the permission registry")
        self.admin.json("PUT", API + "/me/profile", {
            "display_name": "Acceptance Owner", "bio": "安装与恢复验收", "expected_version": me["version"],
        })
        after = self.admin.me()
        require(after["user_id"] == me["user_id"] and after["version"] == me["version"] + 1,
                "profile update must preserve the authenticated session")
        self.admin.json("PUT", API + "/me/profile", {
            "display_name": "Stale edit", "bio": None, "expected_version": me["version"],
        }, status=409)
        previous = self.admin.clone()
        new_password = "Changed-" + secrets.token_hex(16) + "!"
        self.admin.json("POST", API + "/me/password", {
            "current_password": self.password, "new_password": new_password,
        })
        self.password = new_password
        self.admin.me()
        previous.request("GET", API + "/me", status=401)
        self.stop()
        self.start()  # No DATABASE_URL: boot entirely from the installation journal.
        self.assert_installed()
        require(self.admin.me()["display_name"] == "Acceptance Owner",
                "session and profile must survive a real process restart")

    def create(self, kind, slug, **fields):
        return self.admin.json("POST", API + "/" + kind,
                               {"slug": slug, "title": slug, "content": "Acceptance content", **fields}, status=201)

    def action(self, kind, item, action, **fields):
        return self.admin.json("POST", f"{API}/{kind}/{item['id']}/{action}",
                               {"expected_version": item["version"], **fields})

    def media_and_content(self):
        self.media = self.admin.json("POST", API + "/media?filename=acceptance.png", PNG, status=201,
                                     headers={"Content-Type": "image/png"})
        self.media_url = self.media["url"]
        require(self.media_url == "/media/" + self.media["id"], "media link must be stable and independent")
        self.assert_image()
        self.body = f"## 持久化正文\n\n**HTTP acceptance**\n\n![图片]({self.media_url})"
        tag = self.admin.json("POST", API + "/tags", {"name": "验收标签", "slug": "acceptance-tag"}, status=201)
        parent = self.admin.json("POST", API + "/categories", {"name": "父分类", "slug": "acceptance-parent"}, status=201)
        category = self.admin.json("POST", API + "/categories", {
            "name": "子分类", "slug": "acceptance-category", "parent": parent["slug"],
        }, status=201)
        require(category["parent_id"] == parent["id"], "category tree must preserve its parent")
        series = []
        for slug in ("acceptance-series-a", "acceptance-series-b"):
            item = self.admin.json("POST", API + "/series", {"name": slug, "slug": slug}, status=201)
            self.admin.json("PATCH", API + "/series/" + slug, {
                "name": slug, "cover_media_id": self.media["id"], "expected_version": item["version"],
            })
            series.append({"series_id": item["id"], "position": 0})
        self.post = self.create("posts", "acceptance-post", content=self.body, cover_media_id=self.media["id"],
                                tag_ids=[tag["id"]], category_id=category["id"], series=series)
        require(len(self.post["series"]) == 2, "one post must belong to both series")
        self.guest.request("GET", "/posts/acceptance-post", status=404)
        self.post = self.action("posts", self.post, "publish")
        self.page = self.action("pages", self.create("pages", "acceptance-page", content=self.body), "publish")
        require("author_id" not in self.page, "pages must remain site-wide resources")
        self.private = self.action("posts", self.create("posts", "acceptance-private", content=self.body,
                                                        visibility="private"), "publish")
        self.admin.json("PUT", API + "/me/avatar", {"avatar_media_id": self.media["id"]})
        settings = self.admin.json("GET", API + "/settings/site")
        self.admin.json("PUT", API + "/settings/site", {
            "title": "Acceptance Blog", "description": "全链路验收", "logo_media_id": self.media["id"],
            "expected_version": settings["version"],
        })
        self.assert_public_content()
        detail = self.admin.json("GET", API + "/media/" + self.media["id"])
        require(detail["media"]["reference_count"] == 7, "cover/body references must deduplicate per source")
        require({row["kind"] for row in detail["references"]} == {"post", "page", "series", "user", "site"},
                "all five media reference kinds must be observable")
        require("<strong>HTTP acceptance</strong>" in self.query(
            "SELECT content_html FROM posts WHERE slug='acceptance-post'"), "post HTML must be persisted")
        theme = self.admin.json("GET", API + "/settings/theme")
        self.admin.json("PUT", API + "/settings/theme", {"slug": "paper", "expected_version": theme["version"]})
        self.assert_public_content()

    def assert_image(self):
        raw, headers = self.guest.request("GET", self.media_url)
        require(raw == PNG and headers.get_content_type() == "image/png", "original image bytes must remain public")

    def assert_public_content(self):
        for path in ("/posts/acceptance-post", "/acceptance-page"):
            body, _ = self.guest.request("GET", path)
            require(b"<strong>HTTP acceptance</strong>" in body and self.media_url.encode() in body,
                    "public SSR must contain rendered content and media")
        for path in ("/", "/tags/acceptance-tag", "/categories/acceptance-category",
                     "/series/acceptance-series-a", "/series/acceptance-series-b", "/feed.xml", "/sitemap.xml"):
            body, _ = self.guest.request("GET", path)
            require(b"/posts/acceptance-post" in body, f"{path}: published post missing")
            require(b"/posts/acceptance-private" not in body, f"{path}: private post leaked")
        self.guest.request("GET", "/posts/acceptance-private", status=404)
        self.admin.request("GET", "/posts/acceptance-private", status=404)
        self.assert_image()

    def lifecycles(self):
        for kind, slug, public_path in (("posts", "acceptance-post", "/posts/acceptance-post"),
                                         ("pages", "acceptance-page", "/acceptance-page")):
            item = self.post if kind == "posts" else self.page
            trash = self.action(kind, item, "trash")
            self.guest.request("GET", public_path, status=404)
            listing = self.admin.json("GET", API + ("/post-trash" if kind == "posts" else "/page-trash"))
            require(listing["total"] == 1 and listing["items"][0]["id"] == item["id"],
                    f"{kind}: trash must preserve the content identity")
            restored = self.action(kind, trash, "restore")
            require(restored["status"] == "draft" and restored["slug"] == slug,
                    f"{kind}: restore must return to draft at the same address")
            published = self.action(kind, restored, "publish")
            if kind == "posts":
                self.post = published
            else:
                self.page = published
        detail = self.admin.json("GET", API + "/media/" + self.media["id"])
        require(detail["media"]["reference_count"] == 7, "trash and restore must preserve references")
        self.admin.json("DELETE", API + "/media/" + self.media["id"],
                        {"expected_version": detail["media"]["version"]}, status=204)
        self.assert_image()
        deleted = self.admin.json("GET", API + "/media/" + self.media["id"])["media"]
        require(deleted["deleted_at"] is not None and deleted["reference_count"] == 7,
                "soft deletion must retain media references")
        self.admin.json("POST", API + "/posts", {
            "slug": "acceptance-invalid-reference", "title": "Invalid reference", "content": self.body,
        }, status=400)
        # Leave this referenced image in the media trash for the backup journey.
        self.assert_public_content()

    def add_comment(self, nickname, body, parent=None):
        self.guest.json("POST", "/api/v1/posts/acceptance-post/comments", {
            "nickname": nickname, "body": body, "email": "guest@example.test", "parent_id": parent,
        }, status=202)
        pending = self.admin.json("GET", API + "/comments?status=pending&post_id=" + self.post["id"])
        matches = [item for item in pending["items"] if item["nickname"] == nickname]
        require(len(matches) == 1, "submitted guest comment must enter the moderation queue")
        item = matches[0]
        require(item["author_email"] == "guest@example.test" and item["ip_address"] == "127.0.0.1",
                "moderation view must preserve private contact and peer information")
        self.admin.json("POST", API + "/comments/" + item["id"],
                        {"version": item["version"], "status": "approved"}, status=204)
        return item

    def comments(self):
        root = self.add_comment("Root guest", "**根评论** [链接](https://example.test/)")
        reply = self.add_comment("Reply guest", "*第二层*", root["id"])
        nested = self.add_comment("Nested guest", "`第三层`", reply["id"])
        require(reply["root_id"] == root["id"] and nested["root_id"] == root["id"]
                and nested["parent_id"] == reply["id"], "nested reply must preserve direct parent and root")
        require("<strong>根评论</strong>" in self.query(
            f"SELECT content_html FROM comments WHERE id='{root['id']}'"), "comment HTML must be persisted")
        self.admin.json("POST", API + "/comments/" + root["id"],
                        {"version": root["version"] + 1, "status": "trash"}, status=204)
        self.root_comment = root["id"]
        self.reply_ids = {reply["id"], nested["id"]}
        self.assert_comments()
        policy = self.admin.json("GET", API + "/comment-settings")
        disabled = self.admin.json("PUT", API + "/comment-settings", {"enabled": False, "version": policy["version"]})
        visible = self.guest.json("GET", "/api/v1/posts/acceptance-post/comments")
        require(not visible["enabled"] and visible["total"] == 1, "closing comments must preserve public history")
        self.admin.json("PUT", API + "/comment-settings", {"enabled": True, "version": disabled["version"]})

    def assert_comments(self):
        path = "/api/v1/posts/acceptance-post/comments"
        roots = self.guest.json("GET", path)
        require(roots["total"] == 1 and len(roots["items"]) == 1, "deleted root with replies must remain listed")
        root = roots["items"][0]
        require(root["id"] == self.root_comment and root["placeholder"] and root["deleted"]
                and not root["content_html"] and not root["nickname"], "deleted root must become an anonymous placeholder")
        replies = self.guest.json("GET", path + "?root_id=" + self.root_comment)
        require(replies["total"] == 2 and {item["id"] for item in replies["items"]} == self.reply_ids,
                "all descendants must remain available in the second display level")
        for item in roots["items"] + replies["items"]:
            require(not ({"body", "author_email", "ip_address", "status", "version"} & item.keys()),
                    "public comment responses must not disclose private moderation fields")

    def schedule_and_stop(self):
        scheduled = []
        for kind in ("posts", "pages"):
            item = self.create(kind, "acceptance-scheduled-" + kind)
            scheduled.append((kind, item))
        self.due = dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=8)
        for kind, item in scheduled:
            result = self.action(kind, item, "schedule", published_at=self.due.isoformat())
            require(result["status"] == "scheduled", f"{kind}: appointment must be persisted")
        self.old_session = self.admin.clone()
        self.stop()  # Prove our only writer is gone before asserting maintenance.
        require(dt.datetime.now(dt.timezone.utc) < self.due, "source writer stopped too late for scheduling drill")

    def recovery_command(self, action, args, database=None):
        command = [sys.executable, "-B", str(PROJECT / "scripts/recovery.py"), action, *map(str, args)]
        if self.pg.container and action != "verify":
            command.extend(["--docker-container", self.pg.container])
        env = self.env()
        env["DATABASE_URL"] = database_url(self.args.admin_url, database) if database else self.args.admin_url
        return json.loads(run_command(command, env, "recovery " + action))

    def backup_restore(self):
        require(self.process is None, "backup requires the owned writer to be stopped")
        self.recovery_command("backup", ["--output", self.backup_dir, "--theme-dir", PROJECT / "themes/default",
                                          "--media-dir", self.media_dir, "--maintenance-confirmed"], self.source)
        self.recovery_command("verify", [self.backup_dir])
        manifest = json.loads((self.backup_dir / "manifest.json").read_text())
        result = self.recovery_command("restore", [self.backup_dir, "--target-db", self.target,
                                                    "--output", self.restore_dir, "--isolation-confirmed"])
        self.owned.add(self.target)
        require(result["counts"] == {**manifest["database_counts"], "sessions": 0},
                "all business row counts must survive restore; sessions must be revoked")
        require(result["verified_media"] == 1 and result["verified_references"] == 7,
                "restore must verify original media and all references")
        require(self.query("SELECT value->>'id' FROM settings WHERE key='installation'", True) == self.installation_id,
                "restore must retain the installation completion marker")
        self.evidence.update(schema=result["schema"]["id"], migrations=result["schema"]["migrations"],
                             backup_counts=manifest["database_counts"],
                             verified_media=result["verified_media"], verified_references=result["verified_references"])

    def isolated_verification(self):
        for command in (["serve", "--addr", "127.0.0.1:0"], ["publish-due"]):
            run_command([str(self.args.binary), *command], self.env(self.target, restored=True),
                        "recovery isolation guard", success=False, timeout=30)
        # Let the appointment become due while no server exists, then start the
        # verification server. An accidentally enabled initial scheduler tick
        # would publish immediately, so no 30-second polling delay is needed.
        deadline = time.monotonic() + 10
        while dt.datetime.now(dt.timezone.utc) <= self.due:
            require(time.monotonic() < deadline, "scheduled time wait exceeded its deadline")
            time.sleep(0.05)
        self.start(self.target, restored=True, isolated=True)
        self.assert_installed()
        self.old_session.request("GET", API + "/me", status=401)
        self.admin = Client(self.origin)
        self.admin.login(self.password)
        self.assert_public_content()
        self.assert_comments()
        for kind, prefix in (("posts", "/posts/"), ("pages", "/")):
            require(self.query(f"SELECT status FROM {kind} WHERE slug='acceptance-scheduled-{kind}'", True) == "scheduled",
                    "recovery verification must not run scheduled publication")
            self.guest.request("GET", prefix + "acceptance-scheduled-" + kind, status=404)
        self.verification_session = self.admin.clone()
        self.stop()

    def release_and_reopen(self):
        require(self.process is None, "stop verification before releasing the recovered database")
        self.recovery_command("release", ["--output", self.restore_dir, "--verification-confirmed"])
        require(self.query("SELECT count(*) FROM sessions", True) == "0", "release must revoke verification sessions")
        # Adapt the saved deployment file to the new connection, retaining the
        # original installation identity. Boot without DATABASE_URL to catch a
        # recovered installed site accidentally reopening installation.
        self.config.write_text("config_version = 1\n[database]\nurl = " +
                               json.dumps(database_url(self.args.admin_url, self.target)) +
                               "\n[server]\npublic_base_url = " + json.dumps(self.origin) + "\n")
        self.start(restored=True)
        self.assert_installed()
        self.verification_session.request("GET", API + "/me", status=401)
        self.admin = Client(self.origin)
        self.admin.login(self.password)
        self.assert_public_content()
        self.assert_comments()
        deadline = time.monotonic() + 10
        for kind, prefix in (("posts", "/posts/"), ("pages", "/")):
            while self.query(f"SELECT status FROM {kind} WHERE slug='acceptance-scheduled-{kind}'", True) != "published":
                require(time.monotonic() < deadline, "normal startup must resume due publication")
                time.sleep(0.1)
            self.guest.request("GET", prefix + "acceptance-scheduled-" + kind)
        self.create("posts", "acceptance-after-recovery")
        detail = self.admin.json("GET", API + "/media/" + self.media["id"])["media"]
        require(detail["deleted_at"] is not None, "media trash state must survive restoration")
        self.admin.json("POST", API + "/media/" + self.media["id"] + "/restore",
                        {"expected_version": detail["version"]}, status=204)
        self.assert_image()
        _, headers = self.admin.request("POST", "/auth/logout", status=303)
        require(headers.get("Location") == "/", "logout must return to the public site")
        self.admin.request("GET", API + "/me", status=401)
        self.stop()

    def cleanup(self):
        self.stop()
        # A failed pg_restore still belongs to this run only if its database
        # guard matches the random tag written into our private restore output.
        marker = self.restore_dir / "ISOLATED"
        errors = []
        if self.target not in self.owned and marker.is_file():
            try:
                tag = marker.read_text().strip()
                actual = self.pg.query("SELECT COALESCE(shobj_description(oid,'pg_database'), 'missing guard') "
                                       f"FROM pg_database WHERE datname='{self.target}'", "postgres")
                if tag.startswith(recovery.ISOLATION_PREFIX) and tag == actual:
                    self.owned.add(self.target)
                elif actual:
                    errors.append(self.target + " (ownership unproven; left untouched)")
            except (AcceptanceError, OSError):
                errors.append(self.target + " (could not verify ownership)")
        for name in sorted(self.owned):
            try:
                self.pg.query(f'DROP DATABASE "{name}" WITH (FORCE)', "postgres")
                self.owned.remove(name)
            except AcceptanceError:
                errors.append(name)
        require(not errors, "could not clean owned test databases: " + ", ".join(errors))

    def run(self):
        for name, action in (
            ("fresh database", self.prepare), ("first-run installation", self.install),
            ("identity and persistent sessions", self.identity), ("media, taxonomy and writing", self.media_and_content),
            ("content and media trash", self.lifecycles), ("nested comments and moderation", self.comments),
            ("scheduled publication and writer shutdown", self.schedule_and_stop), ("backup and isolated restore", self.backup_restore),
            ("isolated HTTP verification", self.isolated_verification), ("release and recovered site", self.release_and_reopen),
        ):
            self.stage(name, action)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/debug/blog")
    parser.add_argument("--admin-dist", type=Path, default=PROJECT / "apps/admin/dist")
    parser.add_argument("--report", type=Path, help="new JSON report path (no secrets, never overwrites)")
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    args.admin_dist = args.admin_dist.resolve()
    report = {"format": 1, "status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat()}
    start = time.monotonic()
    try:
        args.admin_url = admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", ""))
        require(args.report is None or not args.report.exists(), "report already exists; choose a new path")
        with tempfile.TemporaryDirectory(prefix="blog-acceptance-") as directory:
            suite = Acceptance(args, Path(directory))
            try:
                suite.run()
            finally:
                try:
                    suite.stage("cleanup", suite.cleanup)
                finally:
                    report.update(steps=suite.steps, evidence=suite.evidence)
        report["status"] = "passed"
    except AcceptanceError as error:
        report["error"] = str(error)
    except KeyboardInterrupt:
        report["error"] = "interrupted"
    except Exception as error:
        # Unknown exceptions can embed request bodies or environment values.
        report["error"] = "unexpected " + type(error).__name__
    report["seconds"] = round(time.monotonic() - start, 3)
    revision = subprocess.run(["git", "rev-parse", "HEAD"], cwd=PROJECT, capture_output=True, text=True, check=False)
    report["git_commit"] = revision.stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=PROJECT,
                           capture_output=True, text=True, check=False)
    report["tracked_changes"] = bool(dirty.stdout.strip())
    if args.report:
        try:
            with args.report.open("x") as output:
                json.dump(report, output, ensure_ascii=False, indent=2)
                output.write("\n")
        except OSError:
            print("acceptance: could not create the requested report", file=sys.stderr)
            return 1
    if report["status"] != "passed":
        print("acceptance: " + report["error"], file=sys.stderr)
        return 1
    print(f"Acceptance passed in {report['seconds']}s; temporary databases and files removed.")
    return 0


def interrupt(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupt)
    sys.exit(main())
