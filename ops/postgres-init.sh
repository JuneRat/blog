#!/bin/sh
# Sourced by the official PostgreSQL entrypoint, only for a new data volume.
# Keep cluster administration separate from the application's schema owner.
(set -eu
psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
    --set ON_ERROR_STOP=1 --set owner_password="$BLOG_OWNER_PASSWORD" <<'SQL'
CREATE ROLE blog_owner LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION
    PASSWORD :'owner_password';
ALTER DATABASE blog OWNER TO blog_owner;
ALTER SCHEMA public OWNER TO blog_owner;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SQL
)
