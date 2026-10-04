"""Real TLS/auth fixture checks and acceptance failure-evidence guards."""
import contextlib
from email.message import EmailMessage
import io
import json
from pathlib import Path
import smtplib
import ssl
import tempfile
import unittest
from unittest.mock import patch
from unittest.mock import Mock

import acceptance_smtp_tls as runner
from acceptance_smtp_tls_support import TlsSmtpSink, certificates, server_context
import acceptance_smtp_tls_support as fixtures
import test_compose as compose_runner
from acceptance_support import AcceptanceError
from test_compose import verify_smtp_counters


class TlsFixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="blog-smtp-unit-")
        cls.certs = certificates(Path(cls.temporary.name) / "certs")

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def sink(self, mode, certificate="valid", fault=None):
        sink = TlsSmtpSink(mode, server_context(self.certs, certificate), "fixture-user", "fixture-password",
                           "PrivateFixtureResponse", fault)
        self.addCleanup(sink.close)
        return sink

    def context(self):
        return ssl.create_default_context(cafile=str(self.certs / "ca.crt"))

    def connect(self, sink, mode):
        if mode == "tls":
            return smtplib.SMTP_SSL("127.0.0.1", sink.port, timeout=3, context=self.context())
        client = smtplib.SMTP("127.0.0.1", sink.port, timeout=3)
        client.starttls(context=self.context())
        return client

    def message(self, content="body"):
        message = EmailMessage()
        message["From"], message["To"] = "sender@acceptance.invalid", "invited@acceptance.invalid"
        message.set_content(content)
        return message

    def test_both_modes_require_tls_and_real_credentials_before_accepting_mail(self):
        for mode in ("tls", "starttls"):
            with self.subTest(mode=mode):
                sink = self.sink(mode)
                token = "d" * 64
                with self.connect(sink, mode) as client:
                    client.login("fixture-user", "fixture-password")
                    client.send_message(self.message("http://127.0.0.1:8080/admin/#password-reset=" + token))
                self.assertEqual(sink.token("invited@acceptance.invalid", "http://127.0.0.1:8080"), token)
                stats = sink.snapshot()
                self.assertEqual(stats["authenticated"], 1)
                self.assertEqual(stats["tls_handshakes"], 1)
                self.assertEqual(stats["accepted_messages"], 1)
                self.assertNotIn("cleartext_auth", stats)

    def test_wrong_password_and_plaintext_authentication_never_accept_mail(self):
        sink = self.sink("tls")
        with self.connect(sink, "tls") as client:
            with self.assertRaises(smtplib.SMTPAuthenticationError):
                client.login("fixture-user", "wrong")
        self.assertEqual(sink.snapshot()["auth_rejected"], 1)
        plain = self.sink("starttls", fault="no_starttls")
        with smtplib.SMTP("127.0.0.1", plain.port, timeout=3) as client:
            with self.assertRaises(smtplib.SMTPAuthenticationError):
                client.login("fixture-user", "fixture-password")
        self.assertEqual(plain.snapshot()["cleartext_auth"], 1)
        self.assertTrue(sink.server.messages.empty() and plain.server.messages.empty())

    def test_bad_hostname_and_expired_certificates_are_real_verification_failures(self):
        for certificate, expected_code in (("wrong_host", 64), ("expired", 10)):
            sink = self.sink("tls", certificate=certificate)
            with self.subTest(certificate=certificate), self.assertRaises(ssl.SSLCertVerificationError) as result:
                self.connect(sink, "tls")
            self.assertEqual(result.exception.verify_code, expected_code)

    def test_rejected_data_remains_only_in_memory_and_is_not_marked_accepted(self):
        sink = self.sink("tls", fault="data_rejected")
        with self.connect(sink, "tls") as client:
            client.login("fixture-user", "fixture-password")
            with self.assertRaises(smtplib.SMTPDataError):
                client.send_message(self.message())
        self.assertEqual(sink.snapshot()["received_messages"], 1)
        self.assertNotIn("accepted_messages", sink.snapshot())
        sink.close()
        self.assertTrue(sink.server.messages.empty())


class SmtpRunnerTests(unittest.TestCase):
    def test_existing_report_never_starts_a_fixture_or_overwrites_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "report.json"
            path.write_text("original")
            with patch("sys.argv", ["smtp", "--report", str(path)]), patch.object(runner, "certificates") as certs, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(runner.main(), 1)
            certs.assert_not_called()
            self.assertEqual(path.read_text(), "original")

    def test_unexpected_failure_does_not_report_exception_secrets(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "report.json"
            with patch("sys.argv", ["smtp", "--report", str(path)]), patch.object(runner, "admin_url", return_value="unused"), \
                    patch.object(runner, "certificates", side_effect=RuntimeError("private password")), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(runner.main(), 1)
            self.assertEqual(json.loads(path.read_text())["error"], "unexpected RuntimeError")
            self.assertNotIn("password", path.read_text())

    def test_success_cannot_hide_plaintext_or_missing_delivery_evidence(self):
        case = runner.CASES[1]
        counts = {"accepted_messages": 2, "authenticated": 2, "tls_handshakes": 2, "ehlo_tls": 2, "starttls_commands": 2}
        runner.verify_counters(case, counts)
        for changes in ({"cleartext_auth": 1}, {"cleartext_mail": 1}, {"tls_handshakes": 0}, {"accepted_messages": 1}):
            with self.subTest(changes=changes), self.assertRaises(AcceptanceError):
                runner.verify_counters(case, {**counts, **changes})

    def test_failed_data_case_requires_two_attempts_without_accepting_delivery(self):
        case = next(case for case in runner.CASES if case[0] == "data_rejected")
        for counts in ({}, {"connections": 2}, {"connections": 2, "received_messages": 2, "accepted_messages": 1}):
            with self.subTest(counts=counts), self.assertRaises(AcceptanceError):
                runner.verify_counters(case, counts)


class ContainerPipeTests(unittest.TestCase):
    config = {"security": "starttls", "username": "private-user", "password": "private-password",
              "certificate": "private-certificate", "key": "private-key"}

    def run_pipe(self, incoming, sink):
        output = io.StringIO()
        with patch.object(fixtures.sys, "stdin", io.StringIO(incoming)), \
                patch.object(fixtures.sys, "stdout", output), patch.object(fixtures, "server_context"), \
                patch.object(fixtures, "TlsSmtpSink", return_value=sink) as factory:
            result = fixtures.serve_stdio(2525)
        return result, output.getvalue(), factory

    def test_private_setup_tokens_and_protocol_counts_use_only_the_pipe(self):
        sink = Mock(port=2525)
        sink.token.side_effect = ["e" * 64, AcceptanceError("private response")]
        sink.snapshot.return_value = {"TLSv1.3": 2, "authenticated": 2}
        command = json.dumps({"recipient": "invited@acceptance.invalid", "origin": "http://127.0.0.1:8080"})
        incoming = "\n".join([json.dumps(self.config), command, command, '{"snapshot": true}', '"private bad command"', ''])
        result, output, factory = self.run_pipe(incoming, sink)
        self.assertEqual(result, 0)
        replies = [json.loads(line) for line in output.splitlines()]
        self.assertEqual(replies, [{"port": 2525}, {"token": "e" * 64},
                                  {"error": "SMTP acceptance delivery failed"},
                                  {"smtp": {"TLSv1.3": 2, "authenticated": 2}},
                                  {"error": "SMTP acceptance delivery failed"}])
        self.assertNotIn("private", output)
        self.assertEqual(factory.call_args.kwargs["port"], 2525)
        sink.close.assert_called_once_with()

    def test_missing_or_oversized_configuration_cannot_start_smtp(self):
        for incoming in ('{"password": "private-password"}\n', "x" * 32769):
            with self.subTest(length=len(incoming)):
                result, output, factory = self.run_pipe(incoming, Mock())
                self.assertEqual(result, 1)
                self.assertEqual(json.loads(output), {"error": "SMTP fixture failed"})
                factory.assert_not_called()

    def test_compose_mail_success_needs_tls_auth_and_both_deliveries(self):
        counts = {key: 2 for key in ("accepted_messages", "received_messages", "authenticated",
                                    "tls_handshakes", "ehlo_tls", "starttls_commands")}
        verify_smtp_counters(counts, "starttls")
        for change in ({"authenticated": 0}, {"tls_handshakes": 0}, {"accepted_messages": 1},
                       {"starttls_commands": 1}, {"cleartext_auth": 1}, {"cleartext_mail": 1}):
            with self.subTest(change=change), self.assertRaises(AcceptanceError):
                verify_smtp_counters({**counts, **change}, "starttls")

    def test_compose_unexpected_failure_records_failure_without_secret_exception_text(self):
        image = [{"Id": "sha256:fixture", "Architecture": "arm64", "Config": {"Labels": {}}}]
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            with patch("sys.argv", ["compose", "--report", str(report)]), \
                    patch.object(compose_runner.os, "environ", {}), \
                    patch.object(compose_runner.subprocess, "run", return_value=Mock(returncode=0, stdout=json.dumps(image))), \
                    patch.object(compose_runner, "exercise", side_effect=RuntimeError("private-password")), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(compose_runner.main(), 1)
            result = json.loads(report.read_text())
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["error"], "unexpected RuntimeError")
            self.assertNotIn("private-password", report.read_text())


if __name__ == "__main__":
    unittest.main()
