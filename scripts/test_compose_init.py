from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile
import unittest

PROJECT = Path(__file__).resolve().parents[1]


class ComposeInitTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="blog-compose-init-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copyfile(PROJECT / "scripts/compose-init.sh", self.root / "scripts/compose-init.sh")
        shutil.copyfile(PROJECT / ".env.example", self.root / ".env.example")
        self.env_file = self.root / ".env"

    def initialize(self, expected=0):
        result = subprocess.run(["sh", str(self.root / "scripts/compose-init.sh")],
                                cwd="/tmp", capture_output=True, text=True)
        self.assertEqual(result.returncode, expected, result.stderr)
        return result

    def test_creation_is_private_and_passwords_are_not_rotated(self):
        result = self.initialize()
        initial = self.env_file.read_text()
        passwords = re.findall(r"^BLOG_(?:POSTGRES|OWNER)_PASSWORD=([a-f0-9]{64})$", initial, re.M)
        self.assertEqual(len(passwords), 2)
        self.assertNotEqual(*passwords)
        self.assertEqual(stat.S_IMODE(self.env_file.stat().st_mode), 0o600)
        for password in passwords:
            self.assertNotIn(password, result.stdout + result.stderr)
        self.initialize()
        self.assertEqual(self.env_file.read_text(), initial)

    def test_existing_values_and_comments_survive_without_executing_dotenv(self):
        original = "# Keep my settings\nexport BLOG_POSTGRES_PASSWORD='saved # password'\nCUSTOM=$(touch executed)\nBLOG_HTTP_PORT=9000\n"
        self.env_file.write_text(original)
        self.env_file.chmod(0o644)
        self.initialize()
        self.assertTrue(self.env_file.read_text().startswith(original))
        self.assertRegex(self.env_file.read_text(), r"BLOG_OWNER_PASSWORD=[a-f0-9]{64}\n$")
        self.assertFalse((self.root / "executed").exists())
        self.assertEqual(stat.S_IMODE(self.env_file.stat().st_mode), 0o600)

    def test_empty_assignments_are_filled_without_duplicate_keys(self):
        self.env_file.write_text('BLOG_POSTGRES_PASSWORD="" # not set\nexport BLOG_OWNER_PASSWORD=\'\'\n')
        self.initialize()
        lines = self.env_file.read_text().splitlines()
        self.assertEqual(len(lines), 2)
        for line in lines:
            self.assertRegex(line, r"^BLOG_(POSTGRES|OWNER)_PASSWORD=[a-f0-9]{64}$")

    def test_ambiguous_duplicate_keys_are_not_rewritten(self):
        original = "BLOG_POSTGRES_PASSWORD=first\nBLOG_POSTGRES_PASSWORD=second\n"
        self.env_file.write_text(original)
        self.initialize(expected=1)
        self.assertEqual(self.env_file.read_text(), original)
        self.assertFalse(list(self.root.glob(".env.init.*")))

    def test_symlink_target_is_not_modified(self):
        target = self.root / "private-target"
        target.write_text("leave unchanged")
        target.chmod(0o644)
        self.env_file.symlink_to(target)
        self.initialize(expected=1)
        self.assertEqual(target.read_text(), "leave unchanged")
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o644)


if __name__ == "__main__":
    unittest.main()
