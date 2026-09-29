"""Keep development, CI and deployment on the same reviewed database image."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ContainerImageTests(unittest.TestCase):
    def test_database_image_matches_across_deployment_and_checks(self):
        references = set()
        for file in ("compose.yaml", ".github/workflows/ci.yml", "scripts/dev-db.sh"):
            matches = re.findall(r"postgres:18-alpine[^\s\"']*", (ROOT / file).read_text())
            self.assertTrue(matches, file)
            for match in matches:
                self.assertRegex(match, r"^postgres:18-alpine@sha256:[a-f0-9]{64}$")
                references.add(match)
        self.assertEqual(len(references), 1, "CI/dev database differs from the deployment image")

    def test_all_external_build_stages_are_pinned(self):
        stages = set()
        for image, name in re.findall(r"^FROM (\S+) AS (\S+)$", (ROOT / "Dockerfile").read_text(), re.M):
            if image not in stages:
                self.assertRegex(image, r"@sha256:[a-f0-9]{64}$")
            stages.add(name)
