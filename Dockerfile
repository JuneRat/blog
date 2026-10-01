# syntax=docker/dockerfile:1@sha256:ecfaec9ed6d810b56388c508f4121597bfbba70d41a6dfeee4d8cad5f295fc32
FROM node:22-bookworm-slim@sha256:43ac6c60b8f89723f746e8a92ce91abd5017e627ce1ddfe4238355d3a30b772c AS admin
WORKDIR /build/apps/admin
COPY apps/admin/package.json apps/admin/pnpm-lock.yaml apps/admin/pnpm-workspace.yaml ./
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

FROM debian:trixie-slim@sha256:a99cfc517144bc59b1978475ec53b46ecabec7e43635402ee5b77cc54cd1b20a AS application
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 blog \
    && useradd --uid 10001 --gid blog --no-create-home --home-dir /nonexistent blog \
    && install -d -o blog -g blog -m 0700 /var/lib/blog/config /var/lib/blog/media
WORKDIR /opt/blog
COPY --from=server /out/blog /usr/local/bin/blog
COPY --from=admin /build/apps/admin/dist/ ./admin/
COPY migrations/ ./migrations/
COPY themes/ ./themes/
# Local editor-created files may be 0600; public assets must be readable by USER.
RUN chmod -R a+rX /opt/blog
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="blog" \
      org.opencontainers.image.revision=$VCS_REF
ENV BLOG_BIND=0.0.0.0:8080 \
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

# Opt-in maintenance image; no Docker socket or host Python/PG installation needed.
FROM postgres:18@sha256:5a5a84b19854a9ffaa54082c166ff4ec27473a361e496e5ea167f298f2da9722 AS ops
RUN apt-get update \
    && apt-get install --yes --no-install-recommends python3 python3-toml restic age ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 blog \
    && useradd --uid 10001 --gid blog --no-create-home blog \
    && install -d -o blog -g blog -m 0700 /var/lib/blog/config /var/lib/blog/media
COPY --from=application /usr/local/bin/blog /usr/local/bin/blog
COPY --from=application /opt/blog /opt/blog
COPY scripts/recovery.py scripts/recovery_inventory.py scripts/schema_contract.py scripts/deployment_config.py scripts/compose_recovery.py scripts/database-roles.sql /opt/blog/scripts/
RUN chmod -R a+rX /opt/blog
WORKDIR /opt/blog
ENV PYTHONDONTWRITEBYTECODE=1
ENTRYPOINT ["python3", "-B", "/opt/blog/scripts/compose_recovery.py"]

# Keep the default build an application image.
FROM application AS runtime
