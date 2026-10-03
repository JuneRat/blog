"""Shared HTTP client and business scenarios for native and Compose acceptance."""
import base64
import copy
import datetime as dt
from http.cookiejar import CookieJar
from html.parser import HTMLParser
import io
import json
from pathlib import Path
import re
import secrets
import time
import uuid
import zipfile
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, HTTPCookieProcessor, ProxyHandler, Request, build_opener

API = "/api/admin/v1"

PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=")


class AcceptanceError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise AcceptanceError(message)


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

    def login(self, password, username="acceptance-owner"):
        self.json("POST", "/auth/login/password", {"username": username, "password": password})
        return self.me()


class SiteScenario:
    expected_peer = "127.0.0.1"

    def __init__(self, admin, guest, query, expected_peer=None):
        self.admin, self.guest, self.query = admin, guest, query
        self.expected_peer = expected_peer

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

    def account_emails(self, receive_token, *, username="acceptance-invited",
                       email="invited@acceptance.invalid", link_origin=None):
        require(self.guest.json("GET", "/auth/password/recovery")["enabled"], "test SMTP must be enabled")
        link_origin = link_origin or self.guest.origin
        user = self.admin.json("POST", API + "/users", {"username": username, "email": email}, status=201)
        self.admin.request("PUT", API + f"/users/{username}/roles/author", status=204)
        self.admin.json("POST", API + f"/users/{user['id']}/invitation")
        token = receive_token(email, link_origin)
        password = "Invite-" + secrets.token_hex(16) + "!"
        self.guest.json("POST", "/auth/password/reset", {"token": token, "password": password})
        for _ in range(2):
            self.guest.json("POST", "/auth/password/reset", {"token": token, "password": password}, status=400)
        invited = Client(self.guest.origin)
        invited.login(password, username)
        stale = invited.clone()
        # Reset and recovery share three admissions per minute. Verify the
        # production limit, then wait for both the client budget and the
        # account's one-minute issuance cooldown (retained after consumption).
        _, limited = self.guest.request("POST", "/auth/password/recovery", {"email": "missing@acceptance.invalid"}, status=429)
        retry_after = int(limited.get("Retry-After", "0"))
        require(1 <= retry_after <= 60, "recovery admission must include a bounded Retry-After")
        deadline = time.monotonic() + max(retry_after, 60) + 0.1
        while time.monotonic() < deadline:
            time.sleep(max(0, min(1, deadline - time.monotonic())))
        public_response = self.guest.json("POST", "/auth/password/recovery", {"email": email}, status=202)
        token = receive_token(email, link_origin)
        unknown = self.guest.json("POST", "/auth/password/recovery", {"email": "missing@acceptance.invalid"}, status=202)
        require(public_response == unknown, "recovery response must not enumerate accounts")
        password = "Recovered-" + secrets.token_hex(16) + "!"
        self.guest.json("POST", "/auth/password/reset", {"token": token, "password": password})
        stale.request("GET", API + "/me", status=401)
        invited.login(password, username)
        self.admin.request("DELETE", API + f"/users/{username}/roles/author", status=204)
        invited.json("POST", API + "/posts", {"slug": "revoked-write", "title": "Rejected", "content": "Rejected"}, status=403)
        require("post.create" not in invited.me()["permissions"], "role revocation must keep the session but remove write permission")
        return {"transport": "loopback SMTP sink", "deliveries": 2,
                "invitation_replay_rejected": True, "old_session_revoked": True}

    def create(self, kind, slug, **fields):
        return self.admin.json("POST", API + "/" + kind,
                               {"slug": slug, "title": slug, "content": "Acceptance content", **fields}, status=201)

    def action(self, kind, item, action, **fields):
        return self.admin.json("POST", f"{API}/{kind}/{item['id']}/{action}",
                               {"expected_version": item["version"], **fields})

    def task_view(self, kind=None):
        raw, headers = self.admin.request("GET", API + "/tasks" + ("?kind=" + kind if kind else ""))
        require(headers.get("Cache-Control") == "no-store", "task state must not be cached")
        return json.loads(raw)

    def wait_task(self, task_id, kind, expected="completed", timeout=45):
        """Fail immediately on the wrong terminal result, rather than hiding it as a timeout."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            row = next((item for item in self.task_view(kind)["runs"]["items"] if item["id"] == task_id), None)
            require(row is not None, "accepted task disappeared from its recent history")
            if row["status"] not in ("queued", "running"):
                require(row["status"] == expected, "unexpected task result: " + row["status"])
                return row
            time.sleep(.1)
        raise AcceptanceError("task execution timed out")

    def browser_task_fixture(self):
        """Only the runner's random database gets an intentionally invalid old derived row."""
        post = self.create("posts", "browser-task-failure", content="Browser task fixture")
        self.query(f"UPDATE posts SET content='![missing](/media/{uuid.uuid4()})',content_render_version=1 WHERE id='{post['id']}'")
        return post["id"]

    def tasks(self, restart):
        """Shared native/container lifecycle drill. restart calls its hook with the writer stopped.

        HTTP creates requests and content; SQL only ages private fixtures or advances
        their due time, so CI need not wait an hour for the minimum retention interval.
        """
        endpoint = API + "/tasks"
        view = self.task_view()
        require(view["available"] and view["retention_available"], "installed owner-backed tasks must be available")
        schedules = {row["kind"]: row for row in view["schedules"]}
        require(not schedules["retention"]["enabled"] and schedules["publish_due"]["interval_seconds"] == 30,
                "task defaults changed")
        post = self.create("posts", "task-queue-acceptance", content="**Queued task body**")
        self.query(f"UPDATE posts SET content_render_version=1 WHERE id='{post['id']}'")
        before = self.query(f"SELECT version||'|'||updated_at FROM posts WHERE id='{post['id']}'")
        future = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(hours=1)).isoformat()
        cancelled = self.admin.json("POST", endpoint, {"kind": "html_rebuild", "run_at": future}, status=202)
        require(cancelled["status"] == "queued" and cancelled["can_cancel"], "future HTML request must wait")
        result = self.admin.json("POST", endpoint + "/" + cancelled["id"] + "/cancel")
        require(result["status"] == "cancelled", "future plan was not cancelled")
        require(self.query(f"SELECT count(*) FROM audit_logs WHERE action='post.html.rebuild' AND target_id='{post['id']}'") == "0",
                "cancelled future request executed business writes")
        planned = self.admin.json("POST", endpoint, {"kind": "html_rebuild", "run_at": future}, status=202)

        def due_while_stopped():
            require(self.query(f"SELECT status FROM task_runs WHERE id='{planned['id']}'") == "queued",
                    "future request must still be queued before restart")
            self.query(f"UPDATE task_runs SET run_at=clock_timestamp()-interval '1 second' WHERE id='{planned['id']}'")

        restart(due_while_stopped)
        done = self.wait_task(planned["id"], "html_rebuild")
        require(done["trigger"] == "once" and done["report"]["html"]["rebuilt"]["posts"] == 1,
                "restart did not execute the same durable request exactly once")
        require(self.query(f"SELECT version||'|'||updated_at FROM posts WHERE id='{post['id']}'") == before,
                "HTML task changed content editing fields")
        require("<strong>Queued task body</strong>" in self.query(f"SELECT content_html FROM posts WHERE id='{post['id']}'"),
                "queued task did not persist rebuilt HTML")
        require(self.query(f"SELECT count(*) FROM audit_logs WHERE action='post.html.rebuild' AND target_id='{post['id']}'") == "1",
                "restarting the queued request duplicated or skipped its business audit")

        bad = self.create("posts", "task-retry-acceptance", content="Retry fixture")
        self.query(f"UPDATE posts SET content='![missing](/media/{uuid.uuid4()})',content_render_version=1 WHERE id='{bad['id']}'")
        failed = self.admin.json("POST", endpoint, {"kind": "html_rebuild", "run_at": None}, status=202)
        failed = self.wait_task(failed["id"], "html_rebuild", "failed")
        require(failed["can_retry"] and failed["report"]["html"]["failure"]["id"] == bad["id"],
                "failure must identify the stale fixture and permit retry")
        self.query(f"UPDATE posts SET content='**Repaired task body**' WHERE id='{bad['id']}'")
        retry = self.admin.json("POST", endpoint + "/" + failed["id"] + "/retry", status=202)
        require(retry["id"] != failed["id"] and retry["retry_of"] == failed["id"], "retry must create a new task identity")
        retried = self.wait_task(retry["id"], "html_rebuild")
        require(retried["report"]["html"]["rebuilt"]["posts"] == 1, "retry did not rebuild remaining content")
        original = next(row for row in self.task_view("html_rebuild")["runs"]["items"] if row["id"] == failed["id"])
        require(original["report"] == failed["report"] and original["status"] == "failed", "retry overwrote the failed record")

        old, recent, audit = (str(uuid.uuid4()) for _ in range(3))
        self.query(f"INSERT INTO comments(id,post_id,author_name,content,content_html,content_render_version,ip_address,created_at) "
                   f"VALUES('{old}','{post['id']}','Expired fixture','preserve body','<p>preserve body</p>',1,'192.0.2.10',clock_timestamp()-interval '400 days'),"
                   f"('{recent}','{post['id']}','Recent fixture','preserve recent','<p>preserve recent</p>',1,'192.0.2.11',clock_timestamp()); "
                   f"INSERT INTO audit_logs(id,action,target_type,target_id,created_at) VALUES('{audit}','acceptance.expired','system','fixture',clock_timestamp()-interval '400 days')")
        schedule = next(row for row in self.task_view()["schedules"] if row["kind"] == "retention")
        enabled = self.admin.json("PUT", endpoint + "/retention-schedule", {
            "enabled": True, "interval_seconds": 3600, "next_run_at": future, "version": schedule["version"],
        })
        self.query("UPDATE task_schedules SET next_run_at=clock_timestamp()-interval '1 second' WHERE kind='retention'")
        deadline = time.monotonic() + 15
        cleanup = None
        while time.monotonic() < deadline:
            cleanup = next((row for row in self.task_view()["latest"] if row["kind"] == "retention"), None)
            if cleanup:
                break
            time.sleep(.1)
        require(cleanup is not None, "enabled retention schedule never queued a task")
        cleaned = self.wait_task(cleanup["id"], "retention")
        self.admin.json("PUT", endpoint + "/retention-schedule", {
            "enabled": False, "interval_seconds": 3600, "next_run_at": None, "version": enabled["version"],
        })
        require(cleaned["trigger"] == "periodic" and cleaned["report"]["retention"]["comment_ips"] == 1
                and cleaned["report"]["retention"]["audit_logs"] == 1, "periodic retention did not report actual cleanup")
        require(self.query(f"SELECT (ip_address IS NULL)||'|'||content FROM comments WHERE id='{old}'") == "true|preserve body"
                and self.query(f"SELECT host(ip_address) FROM comments WHERE id='{recent}'") == "192.0.2.11"
                and self.query(f"SELECT count(*) FROM audit_logs WHERE id='{audit}'") == "0", "retention deleted live data or failed to clear expired data")

        appointments = []
        due = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=1)).isoformat()
        for kind in ("posts", "pages"):
            item = self.create(kind, "task-periodic-" + kind)
            self.action(kind, item, "schedule", published_at=due)
            appointments.append((kind, item))
        # Persisted schedules still use their normal 30s cadence; only this
        # disposable fixture advances its next check so the test stays bounded.
        time.sleep(1.1)
        self.query("UPDATE task_schedules SET next_run_at=clock_timestamp()-interval '1 second' WHERE kind='publish_due'")
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if all(self.query(f"SELECT status FROM {kind} WHERE id='{item['id']}'") == "published" for kind, item in appointments):
                break
            time.sleep(.1)
        require(all(self.query(f"SELECT status FROM {kind} WHERE id='{item['id']}'") == "published" for kind, item in appointments),
                "automatic publisher did not process due Post and Page")
        publication = None
        while time.monotonic() < deadline:
            completed = [row for row in self.task_view("publish_due")["runs"]["items"]
                         if row["status"] == "completed" and row["trigger"] == "periodic"
                         and (row["report"].get("publication") or {}).get("published", 0)]
            if sum(row["report"]["publication"]["published"] for row in completed) >= 2:
                publication = completed[0]
                break
            time.sleep(.1)
        require(publication is not None, "actual publication has no completed periodic execution report")
        for kind, _ in appointments:
            self.guest.request("GET", ("/posts/" if kind == "posts" else "/") + "task-periodic-" + kind)
        return {"cancelled_id": cancelled["id"], "restarted_queue_id": planned["id"],
                "failed_id": failed["id"], "retry_id": retry["id"], "retention_id": cleanup["id"],
                "publication_id": publication["id"], "test_clock_advanced": True}

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
        self.admin.json("PUT", API + "/me/avatar", {
            "avatar_media_id": self.media["id"],
            "expected_version": self.admin.json("GET", API + "/me")["version"],
        })
        settings = self.admin.json("GET", API + "/settings/site")
        self.admin.json("PUT", API + "/settings/site", {
            "title": "Acceptance Blog", "description": "全链路验收", "logo_media_id": self.media["id"],
            "expected_version": settings["version"],
        })
        self.assert_public_content()
        detail = self.admin.json("GET", API + "/media/" + self.media["id"])
        self.assert_media_references(detail)
        require("<strong>HTTP acceptance</strong>" in self.query(
            "SELECT content_html FROM posts WHERE slug='acceptance-post'"), "post HTML must be persisted")
        theme = self.admin.json("GET", API + "/settings/theme")
        require(all(item["slug"] != "paper" for item in theme["available"]),
                "Paper must be installed as a third-party theme")
        source = Path(__file__).resolve().parent.parent / "theme-packages/paper"
        archive = io.BytesIO()
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as package:
            for item in sorted(source.rglob("*")):
                if item.is_file():
                    package.write(item, "paper/" + item.relative_to(source).as_posix())
        package = archive.getvalue()
        checked = self.admin.json("POST", API + "/themes/validate-package", package,
                                  headers={"Content-Type": "application/zip"})
        installed = self.admin.json("POST", API + "/themes", package, status=201,
                                    headers={"Content-Type": "application/zip"})
        require(installed["slug"] == "paper" and installed["release"] == checked["release"],
                "Paper installation must publish the validated release")
        self.admin.json("PUT", API + "/settings/theme", {"slug": "paper", "expected_version": theme["version"]})
        self.assert_public_content()

    def assert_image(self):
        raw, headers = self.guest.request("GET", self.media_url)
        require(raw == PNG and headers.get_content_type() == "image/png", "original image bytes must remain public")

    def assert_media_references(self, detail):
        live = [row for row in detail["references"] if row["kind"] not in ("post_revision", "page_revision")]
        history = [row for row in detail["references"] if row["kind"] in ("post_revision", "page_revision")]
        require(len(live) == 7 and {row["kind"] for row in live} == {"post", "page", "series", "user", "site"},
                "cover/body references must deduplicate per live source")
        require(len(history) >= 3 and detail["media"]["reference_count"] == len(live) + len(history),
                "media protection must include all retained revision references")

    def editing_revisions(self):
        self.pending_content = []
        for kind, item, public in (("posts", self.post, "/posts/acceptance-post"),
                                   ("pages", self.page, "/acceptance-page")):
            path = f"{API}/{kind}/{item['id']}"
            original = self.admin.json("GET", path + "/revisions")[-1]
            marker = "UnpublishedAcceptance" + kind
            edited = self.admin.json("PATCH", path, {"content": self.body + "\n\n" + marker,
                                                        "expected_version": item["version"]})
            require(edited["has_pending_changes"], "published edits must become server drafts")
            require(marker.encode() not in self.guest.request("GET", public)[0], "editing draft leaked into public HTML")
            require(marker.encode() not in self.guest.request("GET", "/search/?q=HTTP")[0], "editing draft leaked into search")
            self.admin.json("PATCH", path, {"title": "Stale", "expected_version": item["version"]}, status=409)
            published = self.action(kind, edited, "publish")
            require(not published["has_pending_changes"] and marker.encode() in self.guest.request("GET", public)[0],
                    "explicit publish must expose the saved editing draft")
            restored = self.admin.json("POST", path + f"/revisions/{original['id']}/restore",
                                       {"expected_version": published["version"]})
            require(restored["has_pending_changes"] and restored["content"] == self.body,
                    "history restore must create new editing content")
            require(marker.encode() in self.guest.request("GET", public)[0], "history restore must preserve the current publication")
            self.admin.json("POST", path + f"/revisions/{original['id']}/restore",
                            {"expected_version": published["version"]}, status=409)
            result = self.action(kind, restored, "publish")
            require(marker.encode() not in self.guest.request("GET", public)[0], "publishing restored history must update public content")
            if kind == "posts":
                self.post = result
            else:
                self.page = result
            # Keep a real pending edit through backup/restore, without altering
            # the media fixture's seven live reference sources.
            slug = "acceptance-pending-" + kind
            pending = self.action(kind, self.create(kind, slug, content="Public before recovery"), "publish")
            pending_path = f"{API}/{kind}/{pending['id']}"
            self.admin.json("PATCH", pending_path, {"content": "Pending after recovery", "expected_version": pending["version"]})
            self.pending_content.append((pending_path, ("/posts/" if kind == "posts" else "/") + slug))
        self.assert_pending_content()
        self.assert_media_references(self.admin.json("GET", API + "/media/" + self.media["id"]))

    def assert_pending_content(self):
        for path, public in self.pending_content:
            item = self.admin.json("GET", path)
            require(item["has_pending_changes"] and item["content"] == "Pending after recovery",
                    "pending server edit must survive recovery")
            body = self.guest.request("GET", public)[0]
            require(b"Public before recovery" in body and b"Pending after recovery" not in body,
                    "restored publication must remain isolated from the editing draft")
            require(len(self.admin.json("GET", path + "/revisions")) == 3, "revision history must survive recovery")

    def theme_releases(self):
        source = Path(__file__).resolve().parent.parent / "theme-packages/paper"
        slug = "acceptance-theme"

        def package(release):
            archive = io.BytesIO()
            with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
                for item in sorted(source.rglob("*")):
                    if not item.is_file():
                        continue
                    name = item.relative_to(source).as_posix()
                    body = item.read_bytes()
                    if name == "theme.json":
                        manifest = json.loads(body)
                        manifest.update(slug=slug, name="Acceptance Theme")
                        body = json.dumps(manifest).encode()
                    if name == "templates/index.html":
                        body = body.replace(b"{% block content %}",
                            b"{% block content %}<p>{{ theme.config.note }}</p><p>AcceptanceRelease" + release.encode() + b"</p>")
                    output.writestr(slug + "/" + name, body)
                output.writestr(slug + "/settings.schema.json", json.dumps({"config_schema_version": 1,
                    "fields": [{"key": "note", "type": "text", "label": "Note", "default": "Default", "max_length": 100}]}))
            return archive.getvalue()

        headers = {"Content-Type": "application/zip"}
        self.admin.json("POST", API + "/themes", package("One"), status=201, headers=headers)
        path = API + f"/themes/{slug}"
        config = self.admin.json("GET", path + "/settings")
        config = self.admin.json("PUT", path + "/settings", {"id": config["id"], "expected_release": config["release"],
            "config_schema_version": config["config_schema_version"], "expected_version": config["version"],
            "config": {"note": "PreservedAcceptanceConfig"}})
        selection = self.admin.json("GET", API + "/settings/theme")
        preview = self.admin.json("POST", path + "/preview", {"expected_release": config["release"]})
        require("PreservedAcceptanceConfig" in preview["html"] and "AcceptanceReleaseOne" in preview["html"],
                "preview must render the configured installed release")
        require(self.admin.json("GET", API + "/settings/theme") == selection, "preview must not activate a theme")
        selected = self.admin.json("PUT", API + "/settings/theme", {"slug": slug, "expected_version": selection["version"]})
        identity = {"id": config["id"], "expected_release": config["release"], "expected_version": config["version"]}
        upgraded = self.admin.json("POST", path + "/upgrade?" + urlencode(identity), package("Two"), headers=headers)
        current = self.admin.json("GET", path + "/settings")
        require(current["config"] == config["config"] and current["id"] == config["id"]
                and current["release"] != config["release"], "upgrade must retain identity and settings")
        require(self.admin.json("GET", API + "/settings/theme")["version"] == selected["version"],
                "upgrade must retain activation version")
        require(b"AcceptanceReleaseTwo" in self.guest.request("GET", "/")[0], "upgraded release must be active")
        previous = self.admin.json("GET", path + "/previous")["previous"]
        require(previous["release"] == config["release"], "upgrade must retain the previous release")
        self.admin.json("POST", path + "/rollback", identity, status=409)
        rolled = self.admin.json("POST", path + "/rollback", {"id": current["id"],
            "expected_release": current["release"], "expected_version": current["version"]})
        require(rolled["release"] == config["release"] and b"AcceptanceReleaseOne" in self.guest.request("GET", "/")[0],
                "rollback must reactivate the previous release")
        require(self.admin.json("GET", path + "/settings")["config"] == config["config"], "rollback must preserve settings")
        require(self.admin.json("GET", path + "/previous")["previous"]["release"] == upgraded["release"],
                "rollback must retain the replaced release for recovery")
        self.theme_checkpoint = {"path": path, "id": config["id"], "config": config["config"],
                                 "release": config["release"], "previous": upgraded["release"]}
        self.admin.json("PUT", API + "/settings/theme", {"slug": selection["slug"], "expected_version": selected["version"]})
        self.assert_public_content()

    def assert_theme_recovery(self):
        expected = self.theme_checkpoint
        current = self.admin.json("GET", expected["path"] + "/settings")
        require(all(current[key] == expected[key] for key in ("id", "config", "release")),
                "restored theme must preserve identity, configuration and current release")
        previous = self.admin.json("GET", expected["path"] + "/previous")["previous"]
        require(previous["release"] == expected["previous"], "restored theme must retain its rollback release")

    def assert_public_content(self):
        for path in ("/posts/acceptance-post", "/acceptance-page"):
            body, _ = self.guest.request("GET", path)
            require(b"<strong>HTTP acceptance</strong>" in body and self.media_url.encode() in body,
                    "public SSR must contain rendered content and media")
        for path in ("/", "/tags/acceptance-tag", "/categories/acceptance-category",
                     "/series/acceptance-series-a", "/series/acceptance-series-b", "/feed.xml", "/sitemap.xml",
                     "/search/?q=acceptance", "/archive/", "/authors/acceptance-owner"):
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
        self.assert_media_references(detail)
        expected_references = detail["media"]["reference_count"]
        self.admin.json("DELETE", API + "/media/" + self.media["id"],
                        {"expected_version": detail["media"]["version"]}, status=204)
        self.assert_image()
        deleted = self.admin.json("GET", API + "/media/" + self.media["id"])["media"]
        require(deleted["deleted_at"] is not None and deleted["reference_count"] == expected_references,
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
        require(item["author_email"] == "guest@example.test" and item["ip_address"]
                and (self.expected_peer is None or item["ip_address"] == self.expected_peer),
                "moderation view must preserve private contact and peer information")
        self.admin.json("POST", API + "/comments/" + item["id"],
                        {"version": item["version"], "status": "approved"}, status=204)
        return item

    def comments(self):
        policy = self.admin.json("GET", API + "/access-settings")
        self.admin.json("PUT", API + "/access-settings",
                        {**policy, "guest_comments_enabled": True})
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
