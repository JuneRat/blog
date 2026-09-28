"""Shared HTTP client and business scenarios for native and Compose acceptance."""
import base64
import copy
from http.cookiejar import CookieJar
from html.parser import HTMLParser
import json
import re
from urllib.error import HTTPError, URLError
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

    def login(self, password):
        self.json("POST", "/auth/login/password", {"username": "acceptance-owner", "password": password})
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
        require(item["author_email"] == "guest@example.test" and item["ip_address"]
                and (self.expected_peer is None or item["ip_address"] == self.expected_peer),
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

