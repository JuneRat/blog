"""Bound readiness failures without touching a real Docker daemon or data volume."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name("dev-db.sh")


class DevDatabaseTests(unittest.TestCase):
    def run_script(self, mode, wait="1"):
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "docker"
            fake.write_text(f"#!{sys.executable}\n" + '''
import os, sys, time
args = sys.argv[1:]
mode = os.environ['TEST_DB_MODE']
if args[:2] == ['container', 'inspect']: sys.exit(0)
if args[0] == 'start': sys.exit(0)
if args[0] == 'inspect':
    print('exited' if mode == 'exited' else 'running'); sys.exit(0)
if args[0] == 'logs': print('diagnostic log'); sys.exit(0)
if args[0] == 'exec':
    if mode == 'hung': time.sleep(60)
    sys.exit(0 if mode == 'ready' else 1)
raise SystemExit('unexpected mutation')
''')
            fake.chmod(0o700)
            start = time.monotonic()
            result = subprocess.run(["bash", str(SCRIPT)], capture_output=True, text=True, timeout=8,
                                    env={**os.environ, "PATH": directory + os.pathsep + os.environ["PATH"],
                                         "TEST_DB_MODE": mode, "BLOG_PG_WAIT_SECONDS": wait})
            self.assertLess(time.monotonic() - start, 5)
            return result

    def test_ready(self):
        result = self.run_script("ready")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("就绪：", result.stdout)

    def test_failure_timeout_and_hung_probe(self):
        for mode in ("waiting", "hung", "exited"):
            with self.subTest(mode=mode):
                result = self.run_script(mode)
                self.assertEqual(result.returncode, 1)
                self.assertIn("diagnostic log", result.stderr)
                self.assertNotIn("就绪：", result.stdout)

    def test_rejects_invalid_timeout(self):
        for value in ("0", "-1", "abc", "3601"):
            with self.subTest(value=value):
                self.assertEqual(self.run_script("ready", value).returncode, 2)
