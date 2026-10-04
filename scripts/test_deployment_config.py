import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from deployment_config import resource_paths
from recovery_inventory import RecoveryError


class DeploymentConfigTests(unittest.TestCase):
    def test_existing_toml_is_resolved_by_blog_without_python_toml_dependency(self):
        with tempfile.TemporaryDirectory() as root:
            config = Path(root) / "config.toml"
            config.write_text("[paths]\nmedia_dir='selected/media'\n")
            response = subprocess.CompletedProcess([], 0, json.dumps({"fields": [
                {"key": "paths.media_dir", "value": "selected/media"},
                {"key": "paths.theme_dir", "value": "themes/default"},
                {"key": "paths.admin_dist", "value": "apps/admin/dist"},
            ]}), "")
            with patch("deployment_config.subprocess.run", return_value=response) as run:
                paths = resource_paths(str(config), "/test/blog")
            self.assertEqual(paths["media_dir"], "selected/media")
            self.assertEqual(run.call_args.args[0], ["/test/blog", "--config", str(config), "config", "show", "--for", "resources"])

    def test_explicit_missing_path_and_bad_response_fail_closed(self):
        with tempfile.TemporaryDirectory() as root:
            config = Path(root) / "missing.toml"
            with self.assertRaises(RecoveryError):
                resource_paths(str(config))
            config.write_text("bad configuration")
            response = subprocess.CompletedProcess([], 1, "", "secret source text")
            with patch("deployment_config.subprocess.run", return_value=response):
                with self.assertRaises(RecoveryError) as error:
                    resource_paths(str(config), "/test/blog")
            self.assertNotIn("secret source text", str(error.exception))

    def test_missing_default_file_still_uses_the_shared_rust_resolver(self):
        with patch.dict(os.environ, {"BLOG_MEDIA_DIR": "custom/media"}, clear=True):
            response = subprocess.CompletedProcess([], 0, json.dumps({"fields": [
                {"key": "paths.media_dir", "value": "custom/media"},
                {"key": "paths.theme_dir", "value": "themes/default"},
                {"key": "paths.admin_dist", "value": "apps/admin/dist"},
            ]}), "")
            with patch("deployment_config.Path.exists", return_value=False):
                with patch("deployment_config.subprocess.run", return_value=response) as run:
                    self.assertEqual(resource_paths(blog_bin="/test/blog")["media_dir"], "custom/media")
                    run.assert_called_once()
                    self.assertEqual(run.call_args.kwargs["env"]["BLOG_MEDIA_DIR"], "custom/media")
                    self.assertEqual(run.call_args.args[0][1:3], ["--config", "config.toml"])

    def test_cli_paths_override_environment_before_rust_validation(self):
        with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, {"BLOG_MEDIA_DIR": "env/media"}, clear=True):
            config = Path(root) / "config.toml"
            config.write_text("[paths]\nmedia_dir=42\n")
            response = subprocess.CompletedProcess([], 0, json.dumps({"fields": [
                {"key": "paths.media_dir", "value": "cli/media"},
                {"key": "paths.theme_dir", "value": "themes/default"},
                {"key": "paths.admin_dist", "value": "apps/admin/dist"},
            ]}), "")
            with patch("deployment_config.subprocess.run", return_value=response) as run:
                self.assertEqual(resource_paths(str(config), "/test/blog", {"media_dir": "cli/media"})["media_dir"], "cli/media")
                self.assertEqual(run.call_args.kwargs["env"]["BLOG_MEDIA_DIR"], "cli/media")


if __name__ == "__main__":
    unittest.main()
