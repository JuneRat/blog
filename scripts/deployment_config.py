"""Resolve resource paths through the application's configuration parser.

Database administration credentials stay explicitly supplied via DATABASE_URL.
Only sanitized resource fields cross stdout; Python needs no separate TOML parser.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

from recovery_inventory import RecoveryError


def resource_paths(config=None, blog_bin=None, overrides=None):
    overrides = {key: value for key, value in (overrides or {}).items() if value is not None}
    selected = config or os.environ.get("BLOG_CONFIG_FILE")
    path = Path(selected or "config.toml")
    if selected and not path.exists() and not path.is_symlink():
        raise RecoveryError("deployment TOML does not exist")
    binary = blog_bin or shutil.which("blog") or str(Path(__file__).resolve().parents[1] / "target/debug/blog")
    try:
        env = os.environ.copy()
        for key, value in overrides.items():
            env[{"media_dir": "BLOG_MEDIA_DIR", "theme_dir": "BLOG_THEME_DIR",
                 "admin_dist": "BLOG_ADMIN_DIST"}[key]] = str(value)
        result = subprocess.run([str(binary), "--config", str(path), "config", "show", "--for", "resources"],
                                capture_output=True, text=True, check=False, timeout=30, env=env)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RecoveryError("cannot run blog config show; build blog or supply --blog-bin") from error
    if result.returncode:
        raise RecoveryError("deployment configuration failed validation; run blog config check --for resources")
    try:
        fields = json.loads(result.stdout)["fields"]
        paths = {item["key"].removeprefix("paths."): item["value"]
                 for item in fields if item["key"].startswith("paths.")}
        if not all(isinstance(paths.get(key), str) and paths[key].strip()
                   for key in ("media_dir", "theme_dir", "admin_dist")):
            raise ValueError("missing resource paths")
        return paths
    except (ValueError, KeyError, TypeError) as error:
        raise RecoveryError("invalid resource configuration response") from error
