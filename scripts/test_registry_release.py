"""Exercise release identity checks without generating deployment packages."""
import contextlib
import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import registry_release as release

REVISION = "a" * 40
REPOSITORY = "Example/Blog"


class RegistryReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="blog-registry-release-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bundle = self.root / "verified"
        self.bundle.mkdir()
        self.output = self.root / "deployment"
        self.images = {}
        self.calls = []
        self.bad_digest = False
        self.bad_pull = False
        self.fail_push = False
        for index, (role, filename, _) in enumerate(release.IMAGE_ROLES, 1):
            image_id = "sha256:" + str(index) * 64
            (self.bundle / filename).write_text(f"{role}:{REVISION}\n")
            (self.bundle / f"{filename}_ID").write_text(image_id + "\n")
            self.images[role] = {
                "Id": image_id, "Os": "linux", "Architecture": "amd64",
                "Config": {"Labels": {
                    "org.opencontainers.image.revision": REVISION,
                    "org.opencontainers.image.source": f"https://github.com/{REPOSITORY}",
                }},
                "RepoDigests": [],
            }

    def docker(self, *arguments):
        self.calls.append(arguments)
        if arguments[:2] == ("image", "inspect"):
            reference = arguments[2]
            role = "blog-ops" if "blog-ops" in reference else "blog"
            info = copy.deepcopy(self.images[role])
            if reference.startswith("ghcr.io/"):
                destination = reference.split(":", 1)[0].split("@", 1)[0]
                info["RepoDigests"] = [destination + "@sha256:" + "d" * 64,
                                       "other.example/unrelated@sha256:" + "e" * 64]
                if self.bad_digest:
                    info["RepoDigests"] = ["other.example/unrelated@sha256:" + "e" * 64]
                if self.bad_pull and "@" in reference:
                    info["Id"] = "sha256:" + "f" * 64
            return json.dumps([info])
        if arguments[0] == "push" and self.fail_push:
            raise subprocess.CalledProcessError(1, ["docker", *arguments])
        self.assertIn(arguments[0], ("tag", "push", "pull"))
        return ""

    def publish(self, **overrides):
        arguments = dict(verified_dir=self.bundle, output=self.output,
                         repository=REPOSITORY, revision=REVISION)
        arguments.update(overrides)
        with patch.object(release, "docker", side_effect=self.docker), contextlib.redirect_stdout(io.StringIO()):
            release.publish(**arguments)

    def assert_no_registry_writes(self):
        self.assertFalse([call for call in self.calls if call[0] in ("tag", "push")])
        self.assertFalse(self.output.exists())

    def test_image_must_match_verified_artifact_before_publishing(self):
        self.images["blog"]["Id"] = "sha256:" + "f" * 64
        with self.assertRaisesRegex(ValueError, "differs from the verified"):
            self.publish()
        self.assert_no_registry_writes()

    def test_wrong_commit_source_or_platform_never_publishes(self):
        original = copy.deepcopy(self.images)
        for failure in ("revision", "source", "architecture"):
            with self.subTest(failure=failure):
                self.images = copy.deepcopy(original)
                if failure == "architecture":
                    self.images["blog"]["Architecture"] = "arm64"
                else:
                    self.images["blog"]["Config"]["Labels"][f"org.opencontainers.image.{failure}"] = "incorrect"
                with self.assertRaises(ValueError):
                    self.publish()
                self.assert_no_registry_writes()

    def test_artifact_tag_must_match_full_commit(self):
        (self.bundle / "IMAGE").write_text("blog:latest\n")
        with self.assertRaisesRegex(ValueError, "does not match the release commit"):
            self.publish()
        self.assert_no_registry_writes()

    def test_release_identifiers_cannot_inject_registry_paths_or_tags(self):
        for overrides in ({"repository": "Example/Blog/extra"}, {"repository": "../blog"},
                          {"repository": "Example/Blog:latest"}, {"revision": "main"}):
            with self.subTest(overrides=overrides), self.assertRaises(ValueError):
                self.publish(**overrides)
        self.assert_no_registry_writes()

    def test_missing_target_digest_does_not_create_deployment(self):
        self.bad_digest = True
        with self.assertRaisesRegex(ValueError, "published registry digest"):
            self.publish()
        self.assertFalse(self.output.exists())

    def test_pulled_image_must_equal_the_verified_image(self):
        self.bad_pull = True
        with self.assertRaisesRegex(ValueError, "does not match the tested image"):
            self.publish()
        self.assertFalse(self.output.exists())

    def test_failed_push_does_not_create_a_deployment_artifact(self):
        self.fail_push = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish()
        self.assertFalse(self.output.exists())

    def test_existing_deployment_is_not_overwritten(self):
        self.output.mkdir()
        (self.output / ".env").write_text("keep credentials\n")
        with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
            self.publish()
        self.assertEqual((self.output / ".env").read_text(), "keep credentials\n")
        self.assertEqual(self.calls, [])

    def test_success_publishes_only_the_verified_blog_image(self):
        (self.bundle / ".env").write_text("PRIVATE_VALUE=must-not-be-copied\n")
        self.publish()
        self.assertEqual([call[1] for call in self.calls if call[0] == "push"],
                         [f"ghcr.io/example/blog:sha-{REVISION}"])
        manifest = json.loads(self.output.read_text())
        self.assertEqual(manifest["revision"], REVISION)
        self.assertEqual(manifest["platform"], "linux/amd64")
        reference = "ghcr.io/example/blog@sha256:" + "d" * 64
        self.assertEqual(manifest["images"]["blog"]["reference"], reference)
        self.assertIn(("pull", reference), self.calls)
        self.assertNotIn("PRIVATE_VALUE", self.output.read_text())
        self.assertEqual({p.name for p in self.root.iterdir()}, {self.bundle.name, self.output.name})


if __name__ == "__main__":
    unittest.main()
