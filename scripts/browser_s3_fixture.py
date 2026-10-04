"""Small private S3 protocol fixture for the Docker acceptance test (not shipped)."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import datetime as dt
import hashlib
import io
import json
import os
import sys
from urllib.parse import urlsplit, parse_qs, unquote
from xml.sax.saxutils import escape

objects = {}
config = json.loads(sys.stdin.readline())


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def response(self, status, body=b"", headers=None):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Content-Type", "application/xml")
        for key, value in (headers or {}).items(): self.send_header(key, value)
        self.end_headers()
        if self.command != "HEAD": self.wfile.write(body)

    def key(self):
        if not self.headers.get("Authorization", "").startswith("AWS4-HMAC-SHA256 Credential=" + config["access_key"] + "/"):
            self.response(403, b"<Error><Code>AccessDenied</Code></Error>"); return None
        parsed = urlsplit(self.path)
        if not parsed.path.startswith("/backups/") and parsed.path != "/backups":
            self.response(404); return None
        return unquote(parsed.path[len("/backups/"):])

    def do_PUT(self):
        key = self.key()
        if key is None: return
        body = self.rfile.read(int(self.headers["Content-Length"]))
        # Small acceptance archives use PutObject, not multipart upload.
        objects[key] = body
        self.response(200, headers={"ETag": '"' + hashlib.md5(body).hexdigest() + '"'})

    def do_GET(self):
        key = self.key()
        if key is None: return
        query = parse_qs(urlsplit(self.path).query)
        if query.get("list-type") == ["2"]:
            prefix = query.get("prefix", [""])[0]
            entries = []
            for name, body in sorted(objects.items()):
                if name.startswith(prefix):
                    entries.append("<Contents><Key>" + escape(name) + "</Key><LastModified>2026-10-04T00:00:00.000Z</LastModified><Size>" + str(len(body)) + "</Size></Contents>")
            self.response(200, ('<?xml version="1.0"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>backups</Name><IsTruncated>false</IsTruncated>' + ''.join(entries) + '</ListBucketResult>').encode())
        elif key in objects:
            self.response(200, objects[key], {"ETag": '"' + hashlib.md5(objects[key]).hexdigest() + '"'})
        else:
            self.response(404, b"<Error><Code>NoSuchKey</Code></Error>")

    def do_HEAD(self):
        self.do_GET()

    def do_DELETE(self):
        key = self.key()
        if key is None: return
        objects.pop(key, None)
        self.response(204)


print("ready", flush=True)
ThreadingHTTPServer(("0.0.0.0", 9000), Handler).serve_forever()
