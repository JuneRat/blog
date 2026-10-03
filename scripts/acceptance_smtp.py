"""Bounded loopback-only SMTP sink for acceptance; never relays or persists mail."""
from email import policy
from email.parser import BytesParser
import queue
import re
import socketserver
import threading

from acceptance_support import AcceptanceError, require


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.request.settimeout(10)
        self.wfile.write(b"220 acceptance.invalid ESMTP\r\n")
        try:
            while True:
                line = self.rfile.readline(4097)
                if not line or len(line) > 4096:
                    return
                verb = line.split(None, 1)[0].upper()
                if verb in (b"EHLO", b"HELO"):
                    self.wfile.write(b"250-acceptance.invalid\r\n250 8BITMIME\r\n")
                elif verb in (b"MAIL", b"RCPT", b"RSET", b"NOOP"):
                    self.wfile.write(b"250 OK\r\n")
                elif verb == b"DATA":
                    self.wfile.write(b"354 End with a dot\r\n")
                    body = bytearray()
                    while True:
                        item = self.rfile.readline(65537)
                        if item == b".\r\n":
                            break
                        if not item or len(body) + len(item) > 65536:
                            return
                        body.extend(item[1:] if item.startswith(b"..") else item)
                    try:
                        self.server.messages.put_nowait(bytes(body))
                    except queue.Full:
                        self.wfile.write(b"452 Mailbox full\r\n")
                    else:
                        self.wfile.write(b"250 Stored in memory\r\n")
                elif verb == b"QUIT":
                    self.wfile.write(b"221 Bye\r\n")
                    return
                else:
                    self.wfile.write(b"502 Unsupported\r\n")
        except (TimeoutError, OSError):
            return


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True

    def handle_error(self, request, client_address):
        # SMTP data includes reset tokens; never dump a handler traceback.
        pass


class SmtpSink:
    def __init__(self):
        self.server = Server(("127.0.0.1", 0), Handler)
        self.server.messages = queue.Queue(maxsize=10)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.server.server_address[1]

    def token(self, recipient, origin):
        try:
            raw = self.server.messages.get(timeout=15)
        except queue.Empty:
            raise AcceptanceError("SMTP acceptance timed out waiting for delivery") from None
        message = BytesParser(policy=policy.default).parsebytes(raw)
        require(message["To"] == recipient, "SMTP recipient mismatch")
        body = message.get_content()
        links = re.findall(r"(?m)^" + re.escape(origin) + r"/admin/#password-reset=([a-f0-9]{64})\r?$", body)
        require(len(links) == 1, "SMTP must contain one reset link at the configured origin")
        return links[0]

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        while not self.server.messages.empty():
            self.server.messages.get_nowait()
