"""Keep development, CI and deployment on the same reviewed database image."""
from pathlib import Path
import re
import shlex
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ContainerImageTests(unittest.TestCase):
    def test_database_image_matches_across_deployment_and_checks(self):
        references = set()
        for file in ("compose.postgres.yaml", "compose.legacy.yaml", ".github/workflows/ci.yml", "scripts/dev-db.sh"):
            matches = re.findall(r"\bpostgres:(?!//)[^\s\"']+", (ROOT / file).read_text())
            self.assertTrue(matches, file)
            for match in matches:
                self.assertRegex(match, r"^postgres:[^@\s]+@sha256:[a-f0-9]{64}$")
                references.add(match)
        self.assertEqual(len(references), 1, "CI/dev database differs from the deployment image")

    def test_all_external_build_stages_are_pinned(self):
        stages = set()
        source = re.sub(r"\\\r?\n", " ", (ROOT / "Dockerfile").read_text())
        instructions = re.findall(r"^\s*FROM\s+([^\r\n]+)$", source, re.M | re.I)
        self.assertTrue(instructions, "Dockerfile must contain a checked FROM instruction")
        for instruction in instructions:
            with self.subTest(instruction=instruction):
                words = shlex.split(instruction)
                while words and words[0].startswith("--"):
                    words.pop(0)
                self.assertTrue(words, "FROM must specify an image")
                image, *alias = words
                self.assertTrue(not alias or (len(alias) == 2 and alias[0].upper() == "AS"),
                                "unrecognized FROM syntax must not bypass the image check")
                if image.lower() not in stages and image.lower() != "scratch":
                    self.assertRegex(image, r"@sha256:[a-f0-9]{64}$",
                                     "external build images must be pinned")
                if alias:
                    stages.add(alias[1].lower())
