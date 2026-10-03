#!/usr/bin/env python3
"""Local real-HTTP SMTP TLS/auth/failure acceptance against an owned random site.

Requires a built binary/admin SPA, openssl and BLOG_TEST_ADMIN_URL (loopback).
Only this run's databases, processes, keys and mail sinks are created/removed.
Reports contain counters and hashes; credentials, messages and tokens stay private.
"""
import argparse
import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import secrets
import signal
import ssl
import tempfile
import time

from acceptance import Acceptance, PROJECT, admin_url
from acceptance_support import API, AcceptanceError, Client, SiteScenario, require
from acceptance_smtp_tls_support import TlsSmtpSink, certificates, server_context


CASES = (
    ("tls_success", "tls", "valid", True, None),
    ("starttls_success", "starttls", "valid", True, None),
    ("tls_untrusted_ca", "tls", "valid", False, None),
    ("starttls_untrusted_ca", "starttls", "valid", False, None),
    ("tls_wrong_hostname", "tls", "wrong_host", True, None),
    ("starttls_wrong_hostname", "starttls", "wrong_host", True, None),
    ("tls_expired_certificate", "tls", "expired", True, None),
    ("tls_bad_credentials", "tls", "valid", True, "auth_rejected"),
    ("starttls_bad_credentials", "starttls", "valid", True, "auth_rejected"),
    ("starttls_not_offered", "starttls", "valid", True, "no_starttls"),
    ("starttls_rejected", "starttls", "valid", True, "reject_starttls"),
    ("recipient_rejected", "starttls", "valid", True, "recipient_rejected"),
    ("data_rejected", "tls", "valid", True, "data_rejected"),
    ("data_ack_timeout", "tls", "valid", True, "stall_after_data"),
    ("total_delivery_deadline", "starttls", "valid", True, "slow_commands"),
)


class MailSite(Acceptance):
    def __init__(self, args, root, certificates_root, case):
        super().__init__(args, root)
        self.case, self.certificates_root = case, certificates_root
        self.smtp_user = "smtp-" + secrets.token_hex(8)
        self.smtp_password = secrets.token_hex(24)
        self.private_response = "PrivateSmtpResponse" + secrets.token_hex(16)
        self.private_values = [self.smtp_user, self.smtp_password, self.private_response,
                               base64.b64encode(("\0" + self.smtp_user + "\0" + self.smtp_password).encode()).decode(),
                               "invite@acceptance.invalid", "recover@acceptance.invalid", "invited@acceptance.invalid"]

    def env(self, *args, **kwargs):
        env = super().env(*args, **kwargs)
        if self.smtp is not None:
            env.update(BLOG_SMTP_SECURITY=self.case[1], BLOG_SMTP_USERNAME=self.smtp_user,
                       BLOG_SMTP_PASSWORD=self.smtp_password)
            if self.case[3]:
                env["BLOG_SMTP_CA_PEM"] = (self.certificates_root / "ca.crt").read_text()
        return env

    def prepare(self):
        super().prepare()
        self.smtp.close()
        self.smtp = TlsSmtpSink(self.case[1], server_context(self.certificates_root, self.case[2]),
                                self.smtp_user, self.smtp_password, self.private_response, self.case[4])

    def token(self, recipient, origin):
        token = self.smtp.token(recipient, origin)
        self.private_values.append(token)
        return token

    def assert_private_logs(self):
        for path in self.root.glob("server-*.log"):
            logs = path.read_text(errors="replace")
            require(not any(value in logs for value in self.private_values), "SMTP secrets or reset token entered application logs")

    def failures(self):
        # Seed a password account using the install fixture's existing hash.
        # All issuance, failure cancellation and HTTP responses use production code.
        invite = self.admin.json("POST", API + "/users", {"username": "failed-invite", "email": "invite@acceptance.invalid"}, status=201)
        recover = self.admin.json("POST", API + "/users", {"username": "failed-recover", "email": "recover@acceptance.invalid"}, status=201)
        self.query("UPDATE users SET password_hash=(SELECT password_hash FROM users WHERE username='acceptance-owner') "
                   f"WHERE id='{recover['id']}'")
        began = time.monotonic()
        response = self.admin.json("POST", API + f"/users/{invite['id']}/invitation", status=500)
        elapsed = time.monotonic() - began
        require(response["code"] == "internal_error" and not any(value in json.dumps(response) for value in self.private_values),
                "invitation failure must expose only a generic error")
        require(elapsed < 16, "SMTP failure exceeded delivery deadline allowance")
        if self.case[4] == "slow_commands":
            require(11 <= elapsed <= 16, "whole-delivery timeout was not exercised")
        if self.case[4] == "stall_after_data":
            require(9 <= elapsed <= 16, "SMTP response timeout was not exercised")
        require(self.query(f"SELECT count(*) FROM account_links WHERE user_id='{invite['id']}' AND expires_at=issued_at") == "1",
                "failed invitation retained a usable token")
        received_token_rejected = None
        if self.case[4] in ("data_rejected", "stall_after_data"):
            token = self.token("invite@acceptance.invalid", self.origin)
            self.guest.json("POST", "/auth/password/reset", {"token": token, "password": self.password}, status=400)
            received_token_rejected = True
        began = time.monotonic()
        public = self.guest.json("POST", "/auth/password/recovery", {"email": "recover@acceptance.invalid"}, status=202)
        response_seconds = time.monotonic() - began
        unknown = self.guest.json("POST", "/auth/password/recovery", {"email": "missing@acceptance.invalid"}, status=202)
        require(public == unknown, "SMTP failure must not enumerate recovery accounts")
        require(response_seconds < 2, "recovery response waited for SMTP instead of returning asynchronously")
        deadline = time.monotonic() + 17
        while self.query(f"SELECT count(*) FROM account_links WHERE user_id='{recover['id']}' AND expires_at=issued_at") != "1":
            require(time.monotonic() < deadline, "failed asynchronous recovery did not cancel its token")
            time.sleep(.1)
        if self.case[4] in ("data_rejected", "stall_after_data"):
            self.token("recover@acceptance.invalid", self.origin)
        # Failure does not modify the existing password or revoke its session.
        recovered = Client(self.origin)
        recovered.login(self.password, "failed-recover")
        self.admin.me()
        return {"invitation_error_generic": True, "invitation_seconds": round(elapsed, 3),
                "recovery_response_seconds": round(response_seconds, 3), "recovery_response_generic": True,
                "failed_tokens_cancelled": 2, "received_invitation_token_rejected": received_token_rejected,
                "password_and_admin_session_preserved": True}


def verify_counters(case, counters):
    require(counters.get("cleartext_auth", 0) == 0 and counters.get("cleartext_mail", 0) == 0,
            "application sent credentials or mail without TLS")
    if case[0].endswith("_success"):
        require(counters.get("accepted_messages") == 2 and counters.get("authenticated") == 2
                and counters.get("tls_handshakes") == 2 and counters.get("ehlo_tls") == 2,
                "successful mail must authenticate and negotiate TLS for each delivery")
        if case[1] == "starttls":
            require(counters.get("starttls_commands") == 2, "STARTTLS upgrade was not exercised")
    else:
        require(counters.get("accepted_messages", 0) == 0 and counters.get("connections", 0) >= 2,
                "failure fixtures did not reject both delivery attempts")
        if case[2] != "valid" or not case[3] or case[4] in ("no_starttls", "reject_starttls"):
            require(counters.get("auth_attempts", 0) == 0 and counters.get("received_messages", 0) == 0,
                    "TLS rejection did not stop credentials and mail")
        if case[4] == "auth_rejected":
            require(counters.get("auth_rejected") == 2 and counters.get("received_messages", 0) == 0,
                    "authentication failure was not exercised")
        if case[4] == "recipient_rejected":
            require(counters.get("recipient_rejected") == 2 and counters.get("received_messages", 0) == 0,
                    "recipient rejection was not exercised")
        if case[4] == "no_starttls":
            require(counters.get("ehlo_cleartext") == 2 and counters.get("starttls_commands", 0) == 0,
                    "missing STARTTLS capability was not exercised")
        if case[4] in ("reject_starttls", "slow_commands") or (case[1] == "starttls" and not case[0].endswith("_success") and case[4] is None):
            require(counters.get("starttls_commands") == 2, "STARTTLS failure stage was not exercised")
        if case[4] in ("data_rejected", "stall_after_data"):
            require(counters.get("received_messages") == 2, "post-DATA failure was not exercised")


def exercise(args, root, certs, case, row):
    root.mkdir(mode=0o700)
    site = MailSite(args, root, certs, case)
    try:
        site.prepare()
        site.install()
        site.admin = Client(site.origin)
        site.admin.login(site.password)
        row["build"] = site.guest.json("GET", "/version")
        if args.expected_revision:
            require(row["build"]["revision"] == args.expected_revision, "SMTP runtime revision differs from expected build")
        row["server_sha256"] = site.evidence["server_sha256"]
        if case[0].endswith("_success"):
            row["flow"] = SiteScenario.account_emails(site, site.token)
            row["flow"]["transport"] = case[1] + " authenticated loopback SMTP"
        else:
            row["flow"] = site.failures()
        row["smtp"] = site.smtp.snapshot()
        verify_counters(case, row["smtp"])
        site.stop()
        site.assert_private_logs()
        row["private_logs_verified"] = True
    finally:
        site.cleanup()
        row["cleanup"] = not site.owned and site.process is None and site.smtp is None
    require(row["cleanup"], "SMTP acceptance resources were not fully cleaned")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/debug/blog")
    parser.add_argument("--admin-dist", type=Path, default=PROJECT / "apps/admin/dist")
    parser.add_argument("--case", choices=[case[0] for case in CASES], action="append")
    parser.add_argument("--expected-revision")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    report = {"status": "failed", "started_at": dt.datetime.now(dt.timezone.utc).isoformat(), "cases": [],
              "expected_revision": args.expected_revision, "platform": platform.platform(),
              "python_tls": ssl.OPENSSL_VERSION,
              "scripts_sha256": {name: hashlib.sha256((PROJECT / "scripts" / name).read_bytes()).hexdigest()
                                 for name in ("acceptance_smtp_tls.py", "acceptance_smtp_tls_support.py",
                                              "acceptance_smtp.py", "acceptance.py", "acceptance_support.py")}}
    try:
        require(not args.report.exists(), "report exists; choose a new path")
        require(args.expected_revision is None or re.fullmatch(r"[a-f0-9]{40}", args.expected_revision),
                "expected revision must be a full commit SHA")
        args.admin_url = admin_url(os.environ.get("BLOG_TEST_ADMIN_URL", ""))
        with tempfile.TemporaryDirectory(prefix="blog-smtp-tls-") as temporary:
            root = Path(temporary)
            certs = certificates(root / "certificates")
            report["certificates_sha256"] = {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                             for path in sorted(certs.glob("*.crt"))}
            for case in CASES:
                if args.case and case[0] not in args.case:
                    continue
                print("==> SMTP " + case[0], flush=True)
                row = {"case": case[0], "security": case[1], "status": "failed"}
                report["cases"].append(row)
                began = time.monotonic()
                try:
                    exercise(args, root / case[0], certs, case, row)
                    row["status"] = "passed"
                finally:
                    row["seconds"] = round(time.monotonic() - began, 3)
        report["status"] = "passed"
    except (AcceptanceError, KeyboardInterrupt) as error:
        report["error"] = str(error) if isinstance(error, AcceptanceError) else "interrupted"
    except Exception as error:
        report["error"] = "unexpected " + type(error).__name__
    report["finished_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    try:
        with args.report.open("x") as output:
            json.dump(report, output, ensure_ascii=False, indent=2)
            output.write("\n")
    except OSError:
        print("could not create SMTP report")
        return 1
    if report["status"] != "passed":
        print(report.get("error", "SMTP acceptance failed"))
        return 1
    return 0


if __name__ == "__main__":
    def interrupt(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt)
    raise SystemExit(main())
