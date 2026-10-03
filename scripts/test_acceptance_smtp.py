"""Reset tokens remain transient, origin-bound and absent from diagnostics."""
from email.message import EmailMessage
import queue
from types import SimpleNamespace
import unittest

from acceptance_smtp import SmtpSink
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
