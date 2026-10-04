# syntax=docker/dockerfile:1@sha256:ecfaec9ed6d810b56388c508f4121597bfbba70d41a6dfeee4d8cad5f295fc32
FROM node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c AS admin
WORKDIR /build/apps/admin
COPY apps/admin/package.json apps/admin/pnpm-lock.yaml apps/admin/pnpm-workspace.yaml ./
COPY apps/admin/patches/ ./patches/
RUN npm install --global "$(node -p 'require("./package.json").packageManager')" \
    && pnpm install --frozen-lockfile
COPY apps/admin/ ./
RUN pnpm build

FROM rust:1.98.1-trixie@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546 AS server
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
ARG VCS_REF=unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    BLOG_BUILD_REVISION="$VCS_REF" cargo build --locked --release -p server --bin blog \
    && install -D target/release/blog /out/blog

# Runtime and recovery share one release image and PostgreSQL client version.
FROM postgres:18@sha256:5a5a84b19854a9ffaa54082c166ff4ec27473a361e496e5ea167f298f2da9722 AS runtime-tools
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl python3 python3-toml restic age \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 blog \
    && useradd --uid 10001 --gid blog --no-create-home --home-dir /nonexistent blog \
    && install -d -o blog -g blog -m 0700 /var/lib/blog/config /var/lib/blog/media \
    && install -d -o blog -g blog /opt/blog/themes
# Copy the prepared filesystem without inheriting PostgreSQL's VOLUME, server
# entrypoint or PGDATA environment into every application/maintenance container.
FROM scratch AS application
COPY --from=runtime-tools / /
WORKDIR /opt/blog
COPY --from=server /out/blog /usr/local/bin/blog
COPY --from=admin /build/apps/admin/dist/ ./admin/
COPY migrations/ ./migrations/
COPY scripts/recovery.py scripts/recovery_inventory.py scripts/schema_contract.py scripts/deployment_config.py scripts/compose_recovery.py scripts/database-roles.sql ./scripts/
COPY --chown=10001:10001 themes/default/ ./themes/default/
# Local editor-created files may be 0600; public assets must be readable by USER.
RUN chmod -R a+rX /opt/blog
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="blog" \
      org.opencontainers.image.revision=$VCS_REF
ENV PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/lib/postgresql/18/bin \
    LANG=C.UTF-8 \
    PYTHONDONTWRITEBYTECODE=1 \
    BLOG_BIND=0.0.0.0:8080 \
    BLOG_CONFIG_FILE=/var/lib/blog/config/config.toml \
    BLOG_MEDIA_DIR=/var/lib/blog/media \
    BLOG_ADMIN_DIST=/opt/blog/admin \
    BLOG_THEME_DIR=/opt/blog/themes/default \
    BLOG_MIGRATIONS_DIR=/opt/blog/migrations/postgres
USER 10001:10001
EXPOSE 8080
# This is readiness: installation deliberately reports 503 until completed.
HEALTHCHECK --interval=15s --timeout=5s --start-period=15s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 3 http://127.0.0.1:8080/readyz || exit 1
ENTRYPOINT ["/usr/local/bin/blog"]
CMD ["serve"]

# Compatibility build target for operators restoring older isolated backups.
# New releases publish only runtime; Compose selects this command on that image.
FROM application AS ops
USER 0:0
ENTRYPOINT ["python3", "-B", "/opt/blog/scripts/compose_recovery.py"]
CMD []

FROM application AS runtime
