"""Private, bounded TLS SMTP fixtures. Never relay, persist mail or log commands."""
import base64
from collections import Counter
import hmac
from pathlib import Path
import queue
import socketserver
import ssl
import subprocess
import threading

from acceptance_smtp import SmtpSink, Server
from acceptance_support import AcceptanceError, require

FAULTS = {None, "no_starttls", "reject_starttls", "auth_rejected", "recipient_rejected",
          "data_rejected", "stall_after_data", "slow_commands"}


def certificates(root):
    """Generate ephemeral certificates; no key is installed in a host trust store."""
    root = Path(root)
    root.mkdir(mode=0o700)

    def openssl(*args):
        result = subprocess.run(["openssl", *args], cwd=root, capture_output=True, timeout=30)
        require(result.returncode == 0, "SMTP test certificate generation failed")

    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days", "1",
            "-subj", "/CN=blog-smtp-test-ca", "-addext", "basicConstraints=critical,CA:TRUE",
            "-keyout", "ca.key", "-out", "ca.crt")
    for name, san, days in (("valid", "DNS:localhost,IP:127.0.0.1", "1"),
                            ("wrong_host", "DNS:other.invalid", "1"),
                            ("expired", "DNS:localhost,IP:127.0.0.1", "0")):
        openssl("req", "-newkey", "rsa:2048", "-nodes", "-sha256", "-subj", "/CN=" + name,
                "-keyout", name + ".key", "-out", name + ".csr")
        (root / (name + ".ext")).write_text(
            "subjectAltName=" + san + "\nbasicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
        openssl("x509", "-req", "-sha256", "-days", days, "-in", name + ".csr",
                "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial",
                "-extfile", name + ".ext", "-out", name + ".crt")
    for path in root.glob("*.key"):
        path.chmod(0o600)
    return root


def server_context(root, name="valid"):
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(str(root / (name + ".crt")), str(root / (name + ".key")))
    return context


class TlsHandler(socketserver.BaseRequestHandler):
    def handle(self):
        server, stream, reader = self.server, self.request, None
        encrypted, authenticated = False, False
        server.record("connections")

        def send(response):
            if server.fault == "slow_commands" and server.stop.wait(4):
                raise OSError("fixture stopped")
            stream.sendall(response)

        def upgrade():
            nonlocal stream, reader, encrypted
            if reader is not None:
                reader.close()
            stream = server.context.wrap_socket(stream, server_side=True)
            reader = stream.makefile("rb")
            encrypted = True
            server.record("tls_handshakes")
            server.record(stream.version())

        try:
            stream.settimeout(16)
            if server.security == "tls":
                upgrade()
            else:
                reader = stream.makefile("rb")
            send(b"220 acceptance.invalid ESMTP\r\n")
            while not server.stop.is_set():
                line = reader.readline(4097)
                if not line or len(line) > 4096:
                    return
                parts = line.strip().split()
                if not parts:
                    return
                verb = parts[0].upper()
                if verb in (b"EHLO", b"HELO"):
                    server.record("ehlo_tls" if encrypted else "ehlo_cleartext")
                    if encrypted or server.fault == "no_starttls":
                        # no_starttls deliberately offers AUTH on cleartext: the
                        # application must still refuse to send credentials.
                        send(b"250-acceptance.invalid\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n")
                    else:
                        send(b"250-acceptance.invalid\r\n250-STARTTLS\r\n250 8BITMIME\r\n")
                elif verb == b"STARTTLS" and not encrypted:
                    server.record("starttls_commands")
                    if server.fault == "reject_starttls":
                        send(b"454 " + server.private_response + b"\r\n")
                        return
                    send(b"220 Ready for TLS\r\n")
                    upgrade()
                elif verb == b"AUTH":
                    server.record("auth_attempts")
                    if not encrypted:
                        server.record("cleartext_auth")
                        send(b"538 Encryption required\r\n")
                        return
                    payload = b""
                    if len(parts) >= 2 and parts[1].upper() == b"PLAIN":
                        if len(parts) == 2:
                            send(b"334 \r\n")
                            payload = reader.readline(4097).strip()
                        elif len(parts) == 3:
                            payload = parts[2]
                    try:
                        provided = base64.b64decode(payload, validate=True)
                    except ValueError:
                        provided = b""
                    wanted = b"\0" + server.username.encode() + b"\0" + server.password.encode()
                    authenticated = hmac.compare_digest(provided, wanted) and server.fault != "auth_rejected"
                    if authenticated:
                        server.record("authenticated")
                        send(b"235 Authenticated\r\n")
                    else:
                        server.record("auth_rejected")
                        send(b"535 " + server.private_response + b"\r\n")
                        return
                elif verb in (b"MAIL", b"RCPT", b"DATA"):
                    if not encrypted:
                        server.record("cleartext_mail")
                    if not encrypted or not authenticated:
                        send(b"530 TLS and authentication required\r\n")
                        return
                    if verb == b"RCPT" and server.fault == "recipient_rejected":
                        server.record("recipient_rejected")
                        send(b"550 " + server.private_response + b"\r\n")
                        return
                    if verb != b"DATA":
                        send(b"250 OK\r\n")
                        continue
                    send(b"354 End with a dot\r\n")
                    body = bytearray()
                    while True:
                        item = reader.readline(65537)
                        if item == b".\r\n":
                            break
                        if not item or len(body) + len(item) > 65536:
                            return
                        body.extend(item[1:] if item.startswith(b"..") else item)
                    # Keep even rejected DATA in memory to prove that a received
                    # but unacknowledged reset link is revoked by the application.
                    server.messages.put_nowait(bytes(body))
                    server.record("received_messages")
                    if server.fault == "stall_after_data":
                        server.stop.wait(16)
                        return
                    if server.fault == "data_rejected":
                        server.record("data_rejected")
                        send(b"554 " + server.private_response + b"\r\n")
                        return
                    server.record("accepted_messages")
                    send(b"250 Stored in memory\r\n")
                elif verb == b"QUIT":
                    send(b"221 Bye\r\n")
                    return
                elif verb in (b"RSET", b"NOOP"):
                    send(b"250 OK\r\n")
                else:
                    send(b"502 Unsupported\r\n")
                    return
        except (OSError, ValueError, queue.Full):
            server.record("closed_connections")
        finally:
            if reader is not None:
                reader.close()
            stream.close()


class TlsServer(Server):
    def record(self, key):
        with self.lock:
            self.counters[key] += 1


class TlsSmtpSink(SmtpSink):
    def __init__(self, security, context, username, password, private_response, fault=None):
        require(security in ("tls", "starttls") and fault in FAULTS, "invalid SMTP fixture mode")
        require(bool(username) and bool(password), "SMTP fixture needs explicit credentials")
        self.server = TlsServer(("127.0.0.1", 0), TlsHandler)
        self.server.security, self.server.context = security, context
        self.server.username, self.server.password = username, password
        self.server.private_response = private_response.encode()
        self.server.fault = fault
        self.server.messages = queue.Queue(maxsize=10)
        self.server.stop, self.server.lock = threading.Event(), threading.Lock()
        self.server.counters = Counter()
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def snapshot(self):
        with self.server.lock:
            return dict(self.server.counters)

    def close(self):
        self.server.stop.set()
        super().close()
