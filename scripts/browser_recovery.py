#!/usr/bin/env python3
"""Private JSON worker for the browser recovery controller (no HTTP or Docker access).

The server drains HTTP writers and background tasks before backup/restore. The
journal and encrypted archives live outside PostgreSQL; a durable recovery flag
keeps the site closed after any interrupted mutation. Never print subprocess
output, connection strings, recovery keys or remote credentials.
"""
import contextlib
import datetime as dt
import fcntl
import hashlib
import hmac
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import secrets
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
from types import SimpleNamespace
from urllib.parse import urlsplit

import recovery
from compose_recovery import encrypt_archive, private_write, recovery_environment
from recovery_inventory import RecoveryError, digest, schema_snapshot, validate_relations, owner_count

NAME = re.compile(r"blog-\d{8}T\d{6}Z-[a-f0-9]{12}\.tar\.gz\.age")
IMPORT = re.compile(r"upload-[a-f0-9]{32}\.age")
ID = re.compile(r"[a-f0-9]{32}")
MAX_ARCHIVE = 2 * 1024**3
MAX_EXTRACT = 4 * 1024**3
MAX_FILES = 100000
MAIL_ENV = frozenset(("BLOG_SMTP_HOST", "BLOG_SMTP_PORT", "BLOG_SMTP_SECURITY", "BLOG_SMTP_USERNAME",
                      "BLOG_SMTP_PASSWORD", "BLOG_SMTP_FROM", "BLOG_SMTP_CA_PEM"))


class UserFacingError(RecoveryError):
    """Only fixed messages authored by this adapter may cross the HTTP boundary."""


def require(ok, message):
    if not ok:
        raise UserFacingError(message)


def read_json(path, default=None):
    if not path.exists():
        return default
    require(not path.is_symlink() and path.stat().st_size < 4 * 1024**2, "本地恢复记录无效")
    return json.loads(path.read_text())


def write_json(path, value):
    private_write(path, json.dumps(value, ensure_ascii=False, indent=2))
    # Persist the rename as well as the file contents before any database mutation.
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def run(argv, *, data=None, env=None):
    result = subprocess.run(argv, input=data, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            env=env, check=False)
    require(result.returncode == 0, "操作工具执行失败，请检查连接、密钥和存储空间")
    return result.stdout


def safe_name(name, imported=False):
    require(isinstance(name, str) and bool((IMPORT if imported else NAME).fullmatch(name)), "备份文件名无效")
    return name


def public_key(identity):
    require(isinstance(identity, str) and len(identity) < 4096, "恢复密钥无效")
    return run(["age-keygen", "-y"], data=identity.encode()).decode().strip()


def key_document(value):
    require(isinstance(value, str) and len(value) < 16384, "恢复密钥无效")
    try:
        document = json.loads(value)
    except ValueError:
        # Earlier CLI-generated age identities are also supported for importing.
        document = {"identity": value}
    require(isinstance(document, dict), "恢复密钥无效")
    public_key(document.get("identity"))
    return document


def identity_digest(identity):
    normalized = "\n".join(line.strip() for line in identity.splitlines() if line.strip() and not line.lstrip().startswith("#"))
    return hashlib.sha256(normalized.encode()).hexdigest()


def bounded_copy(source, target, maximum):
    total = 0
    while True:
        chunk = source.read(1024 * 1024)
        if not chunk:
            return total
        total += len(chunk)
        require(total <= maximum, "备份解压后超过容量限制")
        target.write(chunk)


def safe_extract(archive, destination, maximum=MAX_EXTRACT):
    """Streaming extraction with byte/entry quotas; tar metadata never controls permissions."""
    seen = set()
    total = 0
    with tarfile.open(archive, mode="r|gz") as source:
        for member in source:
            path = PurePosixPath(member.name)
            require(len(seen) < MAX_FILES and len(member.name) < 1024
                    and not path.is_absolute() and path.parts and path.parts[0] == "backup"
                    and all(part not in ("", ".", "..") for part in member.name.split("/"))
                    and "\\" not in member.name and member.name not in seen
                    and (member.isdir() or member.isfile()), "备份含有不安全或重复的文件路径")
            seen.add(member.name)
            total += member.size
            require(0 <= member.size <= maximum and total <= maximum, "备份解压后超过容量限制")
            target = destination / member.name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True, mode=0o700)
            else:
                target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                with source.extractfile(member) as src, target.open("xb") as out:
                    require(bounded_copy(src, out, member.size) == member.size, "备份文件被截断")
                target.chmod(0o600)


class Store:
    def __init__(self, request):
        self.request = request
        self.root = Path(request["state_dir"])
        require(self.root.is_absolute() and not self.root.is_symlink(), "恢复目录无效")
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.root.chmod(0o700)
        for name in ("backups", "uploads", "jobs"):
            directory = self.root / name
            require(not directory.is_symlink(), "恢复目录不能是符号链接")
            directory.mkdir(exist_ok=True, mode=0o700)
        self.settings_path = self.root / "settings.json"
        self.settings = read_json(self.settings_path, {"format": 1, "site_id": secrets.token_hex(16),
                                                      "schedule": "off", "hour_utc": 18, "weekday": 0,
                                                      "keep": 7, "remote_keep": 30, "next_run": 0})
        self.job = None

    @contextlib.contextmanager
    def locked(self):
        with (self.root / "operation.lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise UserFacingError("已有备份或恢复任务正在进行") from None
            yield

    def save_settings(self):
        write_json(self.settings_path, self.settings)

    def jobs(self):
        return sorted((read_json(p) for p in (self.root / "jobs").glob("*.json")),
                      key=lambda item: item["started_at"], reverse=True)

    def status(self):
        settings = {key: self.settings.get(key) for key in
                    ("schedule", "hour_utc", "weekday", "keep", "remote_keep", "next_run", "recipient")}
        remote = self.settings.get("remote")
        if remote:
            settings["remote"] = {key: remote.get(key, "") for key in ("endpoint", "region", "bucket", "prefix")}
        backups = []
        for file in (self.root / "backups").glob("*.age"):
            if NAME.fullmatch(file.name) and not file.is_symlink():
                record = read_json(file.with_suffix(file.suffix + ".json"))
                if record and record.get("size") == file.stat().st_size:
                    backups.append(record)
        return {"initialized": bool(self.settings.get("key_confirmed")), "settings": settings,
                "backups": sorted(backups, key=lambda b: b["created_at"], reverse=True),
                "jobs": self.jobs()[:50], "recovery_required": (self.root / "RECOVERY_REQUIRED").exists(),
                "max_upload_bytes": MAX_ARCHIVE}

    def initialize(self):
        self.save_settings()
        for job in self.jobs():
            if job["status"] == "running":
                job.update(status="interrupted", message="任务被中断，可重新提交；恢复操作需重试或选择回滚副本")
                write_json(self.root / "jobs" / (job["id"] + ".json"), job)
        # Clean only this deployment's abandoned plaintext staging, while the
        # worker lock proves no other operation is using it.
        scratch = Path(self.request.get("scratch_dir", "/tmp")).resolve()
        for path in scratch.glob("blog-browser-" + self.settings["site_id"] + "-*"):
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
        self.clean_uploads()
        for job in self.jobs()[200:]:
            if job["status"] != "running":
                (self.root / "jobs" / (job["id"] + ".json")).unlink(missing_ok=True)
        for path in (self.root / "backups").glob("*.partial"):
            if path.is_file() and not path.is_symlink():
                path.unlink()
        return self.status()

    def clean_uploads(self):
        # In-progress/failed recovery archives remain available for retry.
        flag = read_json(self.root / "RECOVERY_REQUIRED", {})
        protected = {j.get("source") for j in self.jobs() if j["status"] == "running" or j["id"] == flag.get("job")}
        for path in (self.root / "uploads").glob("upload-*.age"):
            if IMPORT.fullmatch(path.name) and path.name not in protected and path.stat().st_mtime < time.time() - 86400:
                path.unlink()
        return {"cleaned": True}

    def generate_key(self):
        require(not self.settings.get("key_confirmed"), "恢复密钥已生成；请使用已保存的密钥")
        identity = run(["age-keygen"]).decode()
        recipient = public_key(identity)
        credential = secrets.token_hex(32)
        self.settings.update(recipient=recipient, identity_hash=identity_digest(identity),
                             credential_hash=hashlib.sha256(credential.encode()).hexdigest())
        self.save_settings()
        return {"key": json.dumps({"format": 1, "identity": identity, "emergency_token": credential,
                                   "site_id": self.settings["site_id"]}, ensure_ascii=False, indent=2)}

    def confirm_key(self):
        document = key_document(self.request.get("key"))
        require(public_key(document["identity"]) == self.settings.get("recipient"), "请选择刚才下载的恢复密钥")
        credential = document.get("emergency_token", "")
        require(isinstance(credential, str) and hmac.compare_digest(hashlib.sha256(credential.encode()).hexdigest(),
                self.settings.get("credential_hash", "")), "恢复密钥验证失败")
        self.settings["key_confirmed"] = True
        self.save_settings()
        return {"confirmed": True}

    def authenticate(self):
        value = self.request.get("key", "")
        require(isinstance(value, str) and len(value) <= 16384, "恢复密钥无效")
        try:
            document = json.loads(value)
        except ValueError:
            document = {"identity": value}
        require(isinstance(document, dict), "恢复密钥无效")
        credential = document.get("emergency_token", "")
        expected = self.settings.get("credential_hash", "")
        by_token = expected and isinstance(credential, str) and hmac.compare_digest(
            hashlib.sha256(credential.encode()).hexdigest(), expected)
        identity = document.get("identity", "")
        expected_identity = self.settings.get("identity_hash", "")
        by_identity = expected_identity and isinstance(identity, str) and hmac.compare_digest(identity_digest(identity), expected_identity)
        require(by_token or by_identity, "恢复密钥不属于此部署")
        return {"authenticated": True}

    def phase(self, phase, **extra):
        self.job.update(phase=phase, **extra)
        write_json(self.root / "jobs" / (self.job["id"] + ".json"), self.job)

    def begin(self, kind):
        identifier = self.request.get("job_id", secrets.token_hex(16))
        require(isinstance(identifier, str) and ID.fullmatch(identifier), "任务编号无效")
        self.job = {"id": identifier, "kind": kind, "status": "running", "phase": "checking",
                    "started_at": time.time(), "source": self.request.get("name"), "rollback": None,
                    "requested_by": self.request.get("requested_by", "system")}
        self.phase("checking")
        return identifier

    def client(self):
        return recovery.PgTools(self.request["database_url"])

    def quiet(self, pg):
        # Other app instances / external writers cannot be coordinated by this single-site controller.
        require(pg.query("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() "
                         "AND pid<>pg_backend_pid() AND backend_type='client backend'") == "0",
                "仍有其他程序连接数据库，请停止外部写入任务后重试")

    def scratch(self):
        base = Path(self.request.get("scratch_dir", "/tmp")).resolve()
        require(base.is_dir() and not base.is_symlink(), "临时目录无效")
        return tempfile.TemporaryDirectory(prefix="blog-browser-" + self.settings["site_id"] + "-", dir=base)

    def available(self, directory, required):
        require(shutil.disk_usage(directory).free >= required + 16 * 1024**2,
                "存储空间不足，请清理旧备份或增加存储容量")

    def environment(self, pg):
        environment = dict(os.environ)
        recovered = read_json(Path(self.request["config_path"]).parent / "recovered-secrets.json", {})
        environment = {**recovered, **environment}
        refs = recovery.secret_refs(pg)
        values = recovery_environment(environment, refs)
        return {key: value for key, value in values.items() if key in MAIL_ENV or key in refs}

    def snapshot(self, recipient, *, protected=False):
        import toml
        pg = self.client()
        self.quiet(pg)
        estimated = int(pg.query("SELECT pg_database_size(current_database())"))
        for source in (Path(self.request["media_dir"]), Path(self.request["theme_dir"]).parent):
            estimated += sum(item["size"] for item in recovery.file_records(source))
        self.available(self.root / "backups", estimated)
        name = "blog-" + dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + secrets.token_hex(6) + ".tar.gz.age"
        target = self.root / "backups" / name
        with self.scratch() as temporary:
            work = Path(temporary)
            self.available(work, estimated * 2)
            deployment = work / "deployment"
            deployment.mkdir(mode=0o700)
            config_path = Path(self.request["config_path"])
            configured = toml.loads(config_path.read_text()) if config_path.exists() else {}
            # Deployment addresses, database credentials and mount paths are not restored.
            write_json(deployment / "environment.json", self.environment(pg))
            private_write(deployment / "config.toml", toml.dumps({"mail": configured.get("mail", {})}))
            write_json(deployment / "browser.json", {"format": 1, "created_at": time.time(),
                                                    "site_id": self.settings["site_id"]})
            old_environment = dict(os.environ)
            try:
                os.environ.update(self.environment(pg))
                os.environ["DATABASE_URL"] = self.request["database_url"]
                with contextlib.redirect_stdout(io.StringIO()):
                    recovery.backup(SimpleNamespace(output=str(work / "backup"), docker_container=None,
                        maintenance_confirmed=True, theme_dir=self.request["theme_dir"],
                        media_dir=self.request["media_dir"], resource=[f"deployment={deployment}"]))
            finally:
                os.environ.clear()
                os.environ.update(old_environment)
            manifest = recovery.verify(work / "backup")
            self.quiet(pg)
            partial = target.with_suffix(".partial")
            try:
                encrypt_archive(work / "backup", partial, recipient)
                with partial.open("rb") as stream:
                    os.fsync(stream.fileno())
                os.replace(partial, target)
                record = {"name": name, "size": target.stat().st_size, "sha256": digest(target),
                          "created_at": time.time(), "backup_id": manifest["backup_id"], "protected": protected}
                write_json(target.with_suffix(target.suffix + ".json"), record)
            finally:
                partial.unlink(missing_ok=True)
        return name

    def archive_path(self):
        imported = bool(self.request.get("imported"))
        name = safe_name(self.request.get("name"), imported)
        file = self.root / ("uploads" if imported else "backups") / name
        require(file.is_file() and not file.is_symlink() and file.stat().st_size <= MAX_ARCHIVE, "备份文件不存在或过大")
        return file

    @contextlib.contextmanager
    def unpack(self):
        path = self.archive_path()
        document = key_document(self.request.get("key"))
        with self.scratch() as temporary:
            work = Path(temporary)
            self.available(work, path.stat().st_size * 2)
            # The key is only on tmpfs for this job and is never written to a durable volume.
            identity = work / "identity"
            private_write(identity, document["identity"])
            compressed = work / "archive.tar.gz"
            with compressed.open("xb") as output:
                process = subprocess.Popen(["age", "--decrypt", "--identity", str(identity), str(path)],
                                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                try:
                    bounded_copy(process.stdout, output, MAX_ARCHIVE)
                    require(process.wait() == 0, "无法解密备份，请检查恢复密钥或重新上传完整文件")
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()
            identity.unlink()
            safe_extract(compressed, work)
            compressed.unlink()
            root = work / "backup"
            # Compatibility follows the declared format and migration checksums, not an image hash.
            manifest = recovery.verify(root)
            deployment = root / "data" / "resources" / "deployment"
            require(deployment.is_dir(), "备份缺少站点配置，无法从后台恢复")
            secrets_env = read_json(deployment / "environment.json", {})
            require(isinstance(secrets_env, dict) and all(isinstance(k, str) and isinstance(v, str)
                                                        for k, v in secrets_env.items()), "备份密钥配置无效")
            recovery_environment(secrets_env, manifest["secret_refs"])
            yield root, manifest, document

    def restored_configuration(self, root, manifest):
        import toml
        data = root / "data" / "resources" / "deployment"
        target = Path(self.request["config_path"])
        configured = toml.loads(target.read_text()) if target.exists() else {}
        restored = toml.loads((data / "config.toml").read_text())
        if "mail" in restored:
            configured["mail"] = restored["mail"]
        environment = read_json(data / "environment.json", {})
        environment = {k: v for k, v in environment.items() if k in MAIL_ENV or k in manifest["secret_refs"]}
        # Check restored mail settings using the same parser as the running site.
        # Temporary validation files and keys remain outside the restored archive.
        text = toml.dumps(configured)
        check = root.parent / "validated-config.toml"
        private_write(check, text)
        write_json(root.parent / "recovered-secrets.json", environment)
        run([os.environ.get("BLOG_BIN", "/usr/local/bin/blog"), "--config", str(check), "config", "check", "--for", "serve"],
            env={**environment, **os.environ})
        return text, environment

    def inspect(self):
        with self.unpack() as (root, manifest, _):
            self.restored_configuration(root, manifest)
            size = sum(item["size"] for item in manifest["files"])
            for directory in (self.root, Path(self.request["media_dir"]), Path(self.request["theme_dir"]).parent):
                self.available(directory, size * 2)
            return {"backup_id": manifest["backup_id"], "created_at": manifest["created_at"],
                    "files": len(manifest["files"]), "bytes": size,
                    "counts": manifest["database_counts"], "compatible": True,
                    "message": "文件、密钥、数据库版本和空间检查通过。恢复将覆盖内容、账号、媒体和主题；部署地址与数据库连接保持当前设置。"}

    def replace_files(self, source, target):
        """Mount roots cannot be renamed. The durable gate hides partial replacement.

        Stage complete copies on the destination filesystem first. Re-running a
        restore or rollback clears any interrupted stage and replaces all entries.
        """
        target = Path(target)
        require(target.is_dir() and not target.is_symlink(), "恢复目标目录无效")
        stage = target / ".browser-recovery-stage"
        if stage.exists():
            require(not stage.is_symlink(), "恢复临时目录无效")
            shutil.rmtree(stage)
        recovery.copy_resource(source, stage)
        # Data reaches the destination filesystem before removing the old copy.
        for base, directories, files in os.walk(stage, topdown=False):
            for name in files:
                with (Path(base) / name).open("rb") as stream:
                    os.fsync(stream.fileno())
            descriptor = os.open(base, os.O_RDONLY)
            try: os.fsync(descriptor)
            finally: os.close(descriptor)
        for entry in target.iterdir():
            if entry == stage:
                continue
            if entry.is_dir() and not entry.is_symlink():
                shutil.rmtree(entry)
            else:
                entry.unlink()
        for entry in stage.iterdir():
            os.replace(entry, target / entry.name)
        stage.rmdir()
        descriptor = os.open(target, os.O_RDONLY)
        try: os.fsync(descriptor)
        finally: os.close(descriptor)

    def restore(self):
        import toml
        with self.unpack() as (root, manifest, document):
            restored_config, restored_environment = self.restored_configuration(root, manifest)
            pg = self.client()
            self.quiet(pg)
            require(pg.query("SELECT has_schema_privilege(current_schema(),'CREATE')") == "t",
                    "当前数据库账号没有恢复所需的建表权限")
            size = sum(item["size"] for item in manifest["files"])
            for directory in (self.root, Path(self.request["media_dir"]), Path(self.request["theme_dir"]).parent):
                self.available(directory, size * 2)
            self.phase("snapshot")
            # A rollback archive is never removed by automatic retention.
            try:
                rollback = self.snapshot(public_key(document["identity"]), protected=True)
                self.phase("snapshot", rollback=rollback)
            except Exception:
                require(self.request.get("allow_without_snapshot") is True,
                        "无法创建恢复前副本；原数据尚未改动。修复问题后重试，或明确选择放弃自动回滚副本")
            if not self.settings.get("key_confirmed"):
                # The authenticated installer has approved this key. Pair it
                # before mutation so a crash during a fresh restore is recoverable.
                self.settings.update(recipient=public_key(document["identity"]),
                                     identity_hash=identity_digest(document["identity"]), key_confirmed=True)
                credential = document.get("emergency_token", "")
                if isinstance(credential, str) and re.fullmatch(r"[a-f0-9]{64}", credential):
                    self.settings["credential_hash"] = hashlib.sha256(credential.encode()).hexdigest()
                self.save_settings()
            write_json(self.root / "RECOVERY_REQUIRED", {"job": self.job["id"], "backup": manifest["backup_id"]})
            self.phase("database")
            dump = root / "data" / "database.dump"
            pg.run("pg_restore", ["--exit-on-error", "--single-transaction", "--clean", "--if-exists",
                                   "--no-owner", "--no-acl", "--dbname=" + pg.config["PGDATABASE"]], input_path=dump)
            pg.query("DELETE FROM sessions; DELETE FROM account_links")
            recovery.validate_restored(pg, pg.config["PGDATABASE"], manifest, root / "data")
            self.phase("files")
            data = root / "data" / "resources"
            self.replace_files(data / "media", self.request["media_dir"])
            installed = data / "installed-themes"
            require(installed.is_dir(), "备份缺少主题集合")
            self.replace_files(installed, Path(self.request["theme_dir"]).parent)
            self.phase("configuration")
            config_path = Path(self.request["config_path"])
            private_write(config_path, restored_config)
            write_json(config_path.parent / "recovered-secrets.json", restored_environment)
            config_path.with_suffix(".install-state.json").unlink(missing_ok=True)
            self.phase("verifying")
            recovery.validate_media(Path(self.request["media_dir"]), manifest["media"])
            schema_snapshot(pg)
            validate_relations(pg)
            require(owner_count(pg) > 0, "恢复后的站点缺少可登录管理员")
            # The server clears this only after successfully rebuilding the website.
            return {"backup_id": manifest["backup_id"], "rollback": self.job["rollback"], "needs_activation": True}

    def backup(self):
        require(self.settings.get("key_confirmed"), "请先下载并保存恢复密钥")
        self.phase("backup")
        name = self.snapshot(self.settings["recipient"])
        self.settings["next_run"] = next_run(self.settings)
        self.save_settings()
        return {"name": name}

    def finalize_backup(self):
        # The server has reopened the website before remote network I/O begins.
        name = safe_name(self.request.get("name"))
        self.phase("uploading", archive=name)
        warning = None
        if self.settings.get("remote"):
            try:
                self.remote_upload(name)
            except Exception:
                warning = "本地备份完成，但远程上传失败。请检查远程设置后重试上传"
        if not warning:
            self.prune()
        return {"name": name, "warning": warning}

    def prune(self):
        ordinary = [b for b in self.status()["backups"] if not b.get("protected")]
        for record in ordinary[self.settings["keep"]:]:
            self.delete(record["name"])

    def discard_upload(self):
        require(not (self.root / "RECOVERY_REQUIRED").exists(), "恢复尚未完成，暂不能删除恢复文件")
        name = safe_name(self.request.get("name"), imported=True)
        (self.root / "uploads" / name).unlink(missing_ok=True)
        return {"deleted": True}

    def delete(self, name=None):
        name = safe_name(name or self.request.get("name"))
        require(not (self.root / "RECOVERY_REQUIRED").exists(), "恢复尚未完成，暂不能删除备份")
        path = self.root / "backups" / name
        path.unlink(missing_ok=True)
        path.with_suffix(path.suffix + ".json").unlink(missing_ok=True)
        return {"deleted": True}

    def save_schedule(self):
        values = self.request["settings"]
        require(values.get("schedule") in ("off", "daily", "weekly"), "备份频率无效")
        for key, maximum in (("hour_utc", 23), ("weekday", 6), ("keep", 100), ("remote_keep", 365)):
            require(type(values.get(key)) is int and (1 if key in ("keep", "remote_keep") else 0) <= values[key] <= maximum,
                    "定时备份设置无效")
        require(values["schedule"] == "off" or self.settings.get("key_confirmed"), "请先下载恢复密钥")
        self.settings.update({k: values[k] for k in ("schedule", "hour_utc", "weekday", "keep", "remote_keep")})
        self.settings["next_run"] = next_run(self.settings)
        self.save_settings()
        return self.status()

    def remote_client(self, remote=None):
        import boto3
        from botocore.config import Config
        remote = remote or self.settings.get("remote")
        require(remote, "请先配置远程存储")
        return boto3.client("s3", endpoint_url=remote["endpoint"], region_name=remote["region"],
                            aws_access_key_id=remote["access_key"], aws_secret_access_key=remote["secret_key"],
                            config=Config(signature_version="s3v4", connect_timeout=10, read_timeout=60,
                                          retries={"max_attempts": 2}, s3={"addressing_style": "path"}))

    def save_remote(self):
        remote = self.request.get("remote")
        if remote is None:
            self.settings.pop("remote", None)
            self.save_settings()
            return {"saved": True}
        require(isinstance(remote, dict) and all(isinstance(remote.get(k), str) and len(remote[k]) <= 2048
                                                for k in ("endpoint", "region", "bucket", "prefix", "access_key", "secret_key")),
                "远程存储设置无效")
        parsed = urlsplit(remote["endpoint"])
        require(parsed.scheme in ("https", "http") and parsed.hostname and not parsed.username
                and not parsed.password and not parsed.query and not parsed.fragment and parsed.path in ("", "/"), "S3 服务地址无效")
        require(re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]{1,254}", remote["bucket"])
                and re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9/_-]{0,199}", remote["prefix"]), "存储桶或路径无效")
        if not remote["secret_key"] and self.settings.get("remote"):
            remote["secret_key"] = self.settings["remote"]["secret_key"]
        if not remote["access_key"] and self.settings.get("remote"):
            remote["access_key"] = self.settings["remote"]["access_key"]
        require(remote["access_key"] and remote["secret_key"] and remote["region"], "请填写访问密钥和区域")
        client = self.remote_client(remote)
        name = remote["prefix"].rstrip("/") + "/connection-test-" + secrets.token_hex(16)
        body = secrets.token_bytes(32)
        try:
            client.put_object(Bucket=remote["bucket"], Key=name, Body=body)
            result = client.get_object(Bucket=remote["bucket"], Key=name)
            try:
                require(result["Body"].read(33) == body, "远程读写校验失败")
            finally:
                result["Body"].close()
            client.list_objects_v2(Bucket=remote["bucket"], Prefix=remote["prefix"] + "/", MaxKeys=1)
        finally:
            client.delete_object(Bucket=remote["bucket"], Key=name)
        self.settings["remote"] = remote
        self.save_settings()
        return {"saved": True, "tested": True}

    def remote_list(self):
        remote = self.settings["remote"]
        prefix = remote["prefix"].rstrip("/") + "/"
        result = []
        pages = self.remote_client().get_paginator("list_objects_v2").paginate(Bucket=remote["bucket"], Prefix=prefix)
        for page in pages:
            for entry in page.get("Contents", []):
                name = entry["Key"][len(prefix):]
                if NAME.fullmatch(name):
                    result.append({"name": name, "size": entry["Size"], "created_at": entry["LastModified"].timestamp()})
                    require(len(result) <= 10000, "远程备份数量过多，请使用专属备份路径")
        return {"backups": sorted(result, key=lambda item: item["created_at"], reverse=True)}

    def remote_upload(self, name=None):
        name = safe_name(name or self.request.get("name"))
        remote = self.settings["remote"]
        file = self.root / "backups" / name
        require(file.is_file() and not file.is_symlink(), "本地备份不存在")
        from boto3.s3.transfer import TransferConfig
        self.remote_client().upload_file(str(file), remote["bucket"], remote["prefix"].rstrip("/") + "/" + name,
                                         Config=TransferConfig(use_threads=False))
        # Only remove our own archives in the configured prefix, after a successful upload.
        for entry in self.remote_list()["backups"][self.settings["remote_keep"]:]:
            self.remote_client().delete_object(Bucket=remote["bucket"], Key=remote["prefix"].rstrip("/") + "/" + entry["name"])
        return {"uploaded": True}

    def remote_download(self):
        remote = self.settings["remote"]
        name = safe_name(self.request.get("name"))
        result = self.remote_client().get_object(Bucket=remote["bucket"], Key=remote["prefix"].rstrip("/") + "/" + name)
        output_name = "upload-" + secrets.token_hex(16) + ".age"
        output = self.root / "uploads" / output_name
        try:
            require(result["ContentLength"] <= MAX_ARCHIVE, "远程备份超过容量限制")
            self.available(output.parent, result["ContentLength"])
            with output.open("xb") as stream:
                bounded_copy(result["Body"], stream, MAX_ARCHIVE)
        except BaseException:
            output.unlink(missing_ok=True)
            raise
        finally:
            result["Body"].close()
        return {"name": output_name, "imported": True}

    def execute(self):
        action = self.request["action"]
        if action == "status":
            return self.status()
        operations = {"initialize": self.initialize, "keygen": self.generate_key, "key-confirm": self.confirm_key,
                      "authenticate": self.authenticate, "backup": self.backup, "finalize-backup": self.finalize_backup, "restore": self.restore,
                      "clean-uploads": self.clean_uploads, "discard-upload": self.discard_upload,
                      "inspect": self.inspect, "delete": self.delete, "schedule": self.save_schedule,
                      "remote-save": self.save_remote, "remote-list": self.remote_list,
                      "remote-upload": self.remote_upload, "remote-download": self.remote_download}
        require(action in operations, "不支持的恢复操作")
        with self.locked():
            journaled = action in ("backup", "finalize-backup", "restore", "inspect", "remote-save", "remote-list", "remote-upload", "remote-download")
            if not journaled:
                return operations[action]()
            if action == "finalize-backup":
                identifier = self.request.get("job_id", "")
                require(isinstance(identifier, str) and ID.fullmatch(identifier), "任务编号无效")
                self.job = read_json(self.root / "jobs" / (identifier + ".json"))
                require(self.job and self.job["kind"] == "backup", "找不到备份任务")
                self.phase("uploading", status="running")
            else:
                self.begin(action)
            try:
                result = operations[action]()
                self.phase("complete", status="succeeded", finished_at=time.time(), result=result,
                           message=result.get("warning") or "操作完成")
                return result
            except Exception:
                if action == "backup":
                    # A failing schedule retries at the next slot, not every polling tick.
                    self.settings["next_run"] = next_run(self.settings)
                    self.save_settings()
                self.phase("failed", status="failed", finished_at=time.time(),
                           message="操作未完成，详见本次错误提示。恢复中断时可使用恢复前副本回滚")
                raise


def next_run(settings, now=None):
    if settings["schedule"] == "off":
        return 0
    now = now or dt.datetime.now(dt.timezone.utc)
    candidate = now.replace(hour=settings["hour_utc"], minute=0, second=0, microsecond=0)
    while candidate <= now or (settings["schedule"] == "weekly" and candidate.weekday() != settings["weekday"]):
        candidate += dt.timedelta(days=1)
    return candidate.timestamp()


def main():
    os.umask(0o077)
    try:
        request = json.loads(sys.stdin.read(65537))
        result = Store(request).execute()
        print(json.dumps({"ok": True, "result": result}, ensure_ascii=False))
    except UserFacingError as error:
        print(json.dumps({"ok": False, "error": str(error)}, ensure_ascii=False))
        return 1
    except RecoveryError:
        # Never infer safety from language or redact fragments of database output.
        print(json.dumps({"ok": False, "error": "备份校验或数据库操作失败，请检查文件完整性、数据库版本及访问权限"}, ensure_ascii=False))
        return 1
    except Exception:
        print(json.dumps({"ok": False, "error": "操作失败，请检查数据库连接、存储配置和可用空间"}, ensure_ascii=False))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
