"""Exercise release assets, version guards and partial-publication recovery."""
import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import github_release as release

REVISION = "a" * 40
REPOSITORY = "Example/Blog"
TAG = "v0.1.0"
DIGEST = "sha256:" + "d" * 64


class GitHubReleaseTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="blog-github-release-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.project = self.root / "project"
        (self.project / "crates/server").mkdir(parents=True)
        (self.project / "apps/admin").mkdir(parents=True)
        (self.project / "crates/server/Cargo.toml").write_text('[package]\nname = "server"\nversion = "0.1.0"\n')
        (self.project / "apps/admin/package.json").write_text('{"version":"0.1.0"}')
        (self.project / "CHANGELOG.md").write_text('# Changes\n\n## 0.1.0\n\n- Initial release.\n')
        for name in release.COMPOSE_FILES:
            shutil.copyfile(release.ROOT / name, self.project / name)
        (self.project / ".env").write_text("SECRET=never-package-this\n")
        self.manifest = self.root / "report.json"
        self.report = {
            "repository": REPOSITORY, "revision": REVISION, "platform": "linux/amd64",
            "release_tag": TAG, "version": "0.1.0", "images": {"blog": {
                "id": "sha256:" + "1" * 64, "tag": f"ghcr.io/example/blog:sha-{REVISION}",
                "version_tag": "ghcr.io/example/blog:0.1.0", "reference": "ghcr.io/example/blog@" + DIGEST,
            }},
        }
        self.output = self.root / "release"
        self.calls = []
        self.remote = None
        self.remote_assets = {}
        self.remote_revision = REVISION
        self.fail_upload = False

    def prepare(self):
        self.manifest.write_text(json.dumps(self.report))
        return release.prepare(self.project, self.manifest, self.output, TAG, REPOSITORY, REVISION)

    def gh(self, *args):
        self.calls.append(args)
        if args[:2] == ("api", f"repos/{REPOSITORY}/commits/{TAG}"):
            return self.remote_revision + "\n"
        if args[:2] == ("api", f"repos/{REPOSITORY}/releases?per_page=100"):
            self.assertIn("--paginate", args)
            self.assertIn("--slurp", args)
            if self.remote is None:
                return "[[]]"
            result = copy.deepcopy(self.remote)
            result["assets"] = [{"name": name} for name in self.remote_assets]
            return json.dumps([[result]])
        if args[:2] == ("release", "create"):
            self.assertIn("--draft", args)
            self.assertIn("--verify-tag", args)
            self.assertIn("--generate-notes", args)
            self.remote = {"tag_name": TAG, "target_commitish": REVISION, "draft": True, "prerelease": False,
                           "body": (self.output / "RELEASE_NOTES.md").read_text() + "\nGenerated changes.",
                           "html_url": f"https://github.com/{REPOSITORY}/releases/tag/{TAG}"}
            return ""
        if args[:2] == ("release", "upload"):
            self.assertTrue(self.remote["draft"])
            self.assertNotIn("--clobber", args)
            if self.fail_upload:
                raise subprocess.CalledProcessError(1, ["gh", *args])
            for value in args[5:]:
                path = Path(value)
                self.assertNotIn(path.name, self.remote_assets)
                self.remote_assets[path.name] = path.read_bytes()
            return ""
        if args[:2] == ("release", "download"):
            directory = Path(args[args.index("--dir") + 1])
            for index, value in enumerate(args):
                if value == "--pattern":
                    name = args[index + 1]
                    (directory / name).write_bytes(self.remote_assets[name])
            return ""
        if args[:2] == ("release", "edit"):
            self.assertIn("--draft=false", args)
            self.assertEqual(set(self.remote_assets), set(release.ASSETS))
            self.remote["draft"] = False
            return ""
        self.fail(f"unexpected gh invocation: {args}")

    def publish(self):
        with patch.object(release, "gh", side_effect=self.gh):
            return release.publish(self.output, REPOSITORY, REVISION, TAG)

    def test_versions_must_match_both_applications_and_changelog(self):
        self.assertEqual(release.release_version(TAG, self.project), "0.1.0")
        for path, replacement in (("apps/admin/package.json", '{"version":"0.2.0"}'),
                                  ("crates/server/Cargo.toml", '[package]\nversion = "0.2.0"\n'),
                                  ("CHANGELOG.md", '# Changes\n## 0.2.0\n- Later.\n')):
            with self.subTest(path=path):
                file = self.project / path
                original = file.read_text()
                file.write_text(replacement)
                with self.assertRaises(ValueError):
                    release.release_version(TAG, self.project)
                file.write_text(original)

    def test_tag_format_rejects_paths_mutable_aliases_and_invalid_semver(self):
        for tag in ("latest", "0.1.0", "v0.1", "v01.1.0", "v0.1.0+build", "v0.1.0-01",
                    "v0.1.0/extra", "v0.1.0\n", "v0.1.0-" + "a" * 129):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.release_version(tag)
        self.assertEqual(release.release_version("v0.1.0-rc.1"), "0.1.0-rc.1")

    def test_assets_pin_only_the_blog_image_and_never_copy_local_configuration(self):
        originals = {name: (self.project / name).read_text() for name in release.COMPOSE_FILES}
        reference = self.prepare()
        self.assertEqual(reference, "ghcr.io/example/blog:0.1.0@" + DIGEST)
        self.assertEqual({path.name for path in self.output.iterdir()}, {*release.ASSETS, "RELEASE_NOTES.md"})
        for name in release.COMPOSE_FILES:
            actual = (self.output / name).read_text()
            self.assertIn("    image: " + reference + "\n", actual)
            original_image = next(line for line in originals[name].splitlines() if "image: ghcr.io/" in line)
            self.assertEqual(actual.replace("    image: " + reference, original_image), originals[name])
            self.assertEqual((self.project / name).read_text(), originals[name])
        self.assertNotIn("never-package-this", "".join(path.read_text() for path in self.output.iterdir()))
        self.assertIn("Initial release.", (self.output / "RELEASE_NOTES.md").read_text())

    def test_invalid_publication_report_never_creates_assets(self):
        original = copy.deepcopy(self.report)
        for field, value in (("repository", "Other/Blog"), ("revision", "b" * 40), ("release_tag", "v0.2.0"),
                             ("version", "0.2.0"), ("platform", "linux/arm64")):
            with self.subTest(field=field):
                self.report = {**copy.deepcopy(original), field: value}
                with self.assertRaises(ValueError):
                    self.prepare()
                self.assertFalse(self.output.exists())
        self.report = copy.deepcopy(original)
        self.report["images"]["blog"]["reference"] = "ghcr.io/other/blog@" + DIGEST
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertFalse(self.output.exists())

    def test_existing_output_and_unrecognized_templates_are_not_overwritten(self):
        template = self.project / "compose.yaml"
        template.write_text(template.read_text().replace("    image:", "    missing-image:"))
        with self.assertRaisesRegex(ValueError, "exactly one blog.image"):
            self.prepare()
        self.assertFalse(self.output.exists())
        shutil.copyfile(release.ROOT / "compose.yaml", template)
        self.output.mkdir()
        (self.output / ".env").write_text("keep\n")
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertEqual((self.output / ".env").read_text(), "keep\n")

    def test_release_is_only_published_after_all_attachments_are_verified(self):
        self.prepare()
        self.assertIn("/releases/tag/v0.1.0", self.publish())
        self.assertFalse(self.remote["draft"])
        actions = [call[1] for call in self.calls if call[0] == "release"]
        self.assertEqual(actions, ["create", "upload", "download", "edit"])

    def test_failed_upload_leaves_draft_and_retry_completes_without_overwriting(self):
        self.prepare()
        self.fail_upload = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish()
        self.assertTrue(self.remote["draft"])
        self.fail_upload = False
        self.remote_assets["compose.yaml"] = (self.output / "compose.yaml").read_bytes()
        self.calls.clear()
        self.publish()
        self.assertFalse(self.remote["draft"])
        self.assertFalse(any(call[:2] == ("release", "create") for call in self.calls))
        upload = next(call for call in self.calls if call[:2] == ("release", "upload"))
        self.assertNotIn(str(self.output / "compose.yaml"), upload)

    def test_published_release_retry_is_read_only(self):
        self.prepare()
        self.publish()
        self.calls.clear()
        self.publish()
        self.assertFalse(any(call[:2] in (("release", "create"), ("release", "upload"), ("release", "edit"))
                             for call in self.calls))

    def test_remote_asset_or_release_drift_is_not_overwritten(self):
        self.prepare()
        self.publish()
        self.remote_assets["compose.yaml"] = b"different release\n"
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
            self.publish()
        self.assertFalse(any(call[:2] in (("release", "upload"), ("release", "edit")) for call in self.calls))
        self.remote["target_commitish"] = "b" * 40
        with self.assertRaisesRegex(ValueError, "refusing to edit"):
            self.publish()

    def test_published_release_with_missing_attachment_is_not_silently_modified(self):
        self.prepare()
        self.publish()
        del self.remote_assets["compose.yaml"]
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "missing expected assets"):
            self.publish()
        self.assertFalse(any(call[:2] in (("release", "upload"), ("release", "edit")) for call in self.calls))

    def test_tampered_attachments_and_moved_tag_fail_before_release_creation(self):
        self.prepare()
        self.remote_revision = "b" * 40
        with self.assertRaisesRegex(ValueError, "verified commit"):
            self.publish()
        self.assertIsNone(self.remote)
        self.remote_revision = REVISION
        (self.output / "compose.yaml").write_text("changed\n")
        self.calls.clear()
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.publish()
        self.assertEqual(self.calls, [])

    def test_api_failure_is_not_treated_as_an_absent_release(self):
        error = subprocess.CalledProcessError(1, ["gh", "api"], stderr="connection timed out")
        with patch.object(release, "gh", side_effect=error), self.assertRaises(subprocess.CalledProcessError):
            release.find_release(REPOSITORY, TAG)

    def test_release_lookup_includes_drafts_and_all_pages_without_ambiguous_matches(self):
        draft = {"tag_name": TAG, "draft": True}
        pages = [[{"tag_name": "v0.0.1", "draft": False}], [draft]]
        with patch.object(release, "gh", return_value=json.dumps(pages)):
            self.assertEqual(release.find_release(REPOSITORY, TAG), draft)
        with patch.object(release, "gh", return_value=json.dumps([[draft], [draft]])):
            with self.assertRaisesRegex(ValueError, "ambiguous"):
                release.find_release(REPOSITORY, TAG)


if __name__ == "__main__":
    unittest.main()
