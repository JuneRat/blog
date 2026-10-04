#!/usr/bin/env python3
"""One-off initialization used by the 1Panel Compose template, in the same image."""
import os
from pathlib import Path
import secrets
import sys
from urllib.parse import urlsplit

from compose_recovery import private_write


def initialize(root=Path("/var/lib/blog/panel-init"), config=Path("/var/lib/blog/config"),
               themes=Path("/opt/blog/themes"), media=Path("/var/lib/blog/media")):
    os.umask(0o077)
    for directory in (root, config, themes, media):
        if directory.is_symlink():
            raise ValueError("初始化目录不能是符号链接")
        directory.mkdir(parents=True, exist_ok=True)
    origin = os.environ.get("BLOG_PUBLIC_BASE_URL", "")
    parsed = urlsplit(origin)
    if parsed.scheme not in ("http", "https") or not parsed.hostname or parsed.username or parsed.password or parsed.path not in ("", "/") or parsed.query or parsed.fragment:
        raise ValueError("请在面板填写完整站点地址，例如 https://blog.example.com")
    token = os.environ.get("BLOG_INSTALL_TOKEN", "")
    if not 20 <= len(token) <= 256:
        raise ValueError("请在面板设置至少 20 个字符的安装码")
    config_file = config / "config.toml"
    ready = root / "READY"
    if not ready.exists():
        # Publish the marker last; re-running an interrupted initialization uses
        # the already written credentials instead of rotating a live database.
        password_file = root / "postgres-password"
        owner_file = root / "owner-password"
        for file in (password_file, owner_file):
            if not file.exists():
                private_write(file, secrets.token_hex(32))
        owner = owner_file.read_text().strip()
        if len(owner) != 64 or any(c not in "0123456789abcdef" for c in owner):
            raise ValueError("数据库初始化凭据无效，未覆盖已有数据")
        sql = f"""CREATE ROLE blog_owner LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION PASSWORD '{owner}';
ALTER DATABASE blog OWNER TO blog_owner;
ALTER SCHEMA public OWNER TO blog_owner;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
"""
        private_write(root / "10-blog.sql", sql)
        if not config_file.exists():
            private_write(config_file, 'config_version = 1\n[database]\nurl = "postgres://blog_owner:' + owner + '@db:5432/blog"\n')
        private_write(ready, "1\n")
    # PostgreSQL's pinned Alpine image runs as uid/gid 70. Only it and this
    # one-off initializer receive the secret volume; the web container does not.
    os.chown(root, 70, 70)
    root.chmod(0o700)
    for entry in root.iterdir():
        if not entry.is_file() or entry.is_symlink():
            raise ValueError("数据库初始化目录含有未知条目")
        os.chown(entry, 70, 70)
        entry.chmod(0o600)
    for directory in (config, themes, media):
        os.chown(directory, 10001, 10001)
        directory.chmod(0o700 if directory != themes else 0o755)
    if config_file.exists():
        os.chown(config_file, 10001, 10001)
        config_file.chmod(0o600)
    print("部署配置已准备好，数据库密码已自动生成。请打开站点完成安装。")


if __name__ == "__main__":
    try:
        initialize()
    except Exception as error:
        # Do not expose unexpected exception representations or config contents.
        print(str(error) if isinstance(error, ValueError) else "初始化失败，请检查数据卷权限和可用空间", file=sys.stderr)
        sys.exit(1)
