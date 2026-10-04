"""Reset tokens remain transient, origin-bound and absent from diagnostics."""
from email.message import EmailMessage
import io
import json
import queue
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from acceptance_smtp import SmtpSink, serve_stdio
from acceptance_support import AcceptanceError


class MailLinkTests(unittest.TestCase):
    def receive(self, body, recipient="invited@acceptance.invalid"):
        sink = SmtpSink.__new__(SmtpSink)
        sink.server = SimpleNamespace(messages=queue.Queue())
        message = EmailMessage()
        message["To"] = recipient
        message.set_content(body)
        sink.server.messages.put(message.as_bytes())
        return sink

    def test_decodes_transport_encoding_and_consumes_mail_once(self):
        token = "a" * 64
        sink = self.receive("请设置密码\nhttp://127.0.0.1:8080/admin/#password-reset=" + token)
        self.assertEqual(sink.token("invited@acceptance.invalid", "http://127.0.0.1:8080"), token)
        self.assertTrue(sink.server.messages.empty())

    def test_wrong_origin_duplicate_links_and_wrong_recipient_fail_without_token_output(self):
        token = "b" * 64
        origin = "http://127.0.0.1:8080"
        link = origin + "/admin/#password-reset=" + token
        for body, recipient in (("http://other.invalid/admin/#password-reset=" + token, "invited@acceptance.invalid"),
                                (link + "\n" + link, "invited@acceptance.invalid"),
                                (link, "wrong@acceptance.invalid")):
            with self.subTest(recipient=recipient), self.assertRaises(AcceptanceError) as result:
                self.receive(body, recipient).token("invited@acceptance.invalid", origin)
            self.assertNotIn(token, str(result.exception))

    def test_private_pipe_consumes_requests_and_closes_the_sink_without_echoing_bad_input(self):
        token = "c" * 64
        sink = Mock(port=2525)
        sink.token.side_effect = [token, AcceptanceError("private delivery details")]
        command = json.dumps({"recipient": "invited@acceptance.invalid", "origin": "http://127.0.0.1:8080"})
        output = io.StringIO()
        with patch("acceptance_smtp.SmtpSink", return_value=sink), \
                patch("acceptance_smtp.sys.stdin", io.StringIO(command + "\n[\"private-invalid\"]\n" + command + "\n")), \
                patch("acceptance_smtp.sys.stdout", output):
            serve_stdio(2525)
        replies = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(replies[:2], [{"port": 2525}, {"token": token}])
        self.assertEqual(replies[2:], [{"error": "SMTP acceptance delivery failed"}] * 2)
        self.assertNotIn("private", output.getvalue())
        self.assertEqual(sink.token.call_count, 2)
        sink.close.assert_called_once_with()

    def test_oversized_pipe_request_stops_without_reading_mail(self):
        sink = Mock(port=2525)
        with patch("acceptance_smtp.SmtpSink", return_value=sink), \
                patch("acceptance_smtp.sys.stdin", io.StringIO("x" * 4097 + "\n")), \
                patch("acceptance_smtp.sys.stdout", io.StringIO()):
            serve_stdio(2525)
        sink.token.assert_not_called()
        sink.close.assert_called_once_with()
