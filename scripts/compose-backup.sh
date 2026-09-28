#!/bin/sh
# Host orchestration only. Python, PostgreSQL tools and restic live in the ops image.
set -eu
umask 077
ROOT=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

usage() {
    cat <<'EOF'
Usage: sh scripts/compose-backup.sh COMMAND
  backup                         Stop blog, create backup, restart, sync and retain
  verify ARCHIVE                 Check archive, hashes and matching application
  restore ARCHIVE NEW_DIRECTORY   Restore into a new isolated Compose deployment
  check USER [--password-stdin]   Verify restored login, pages and media internally
  release                        Revoke verification sessions and start restored blog
  status                         Show last operation and available local archives
  remote-init                    Initialize the configured encrypted restic repository
  remote-list                    List remote snapshots (IDs needed for fetch)
  sync ARCHIVE_NAME               Retry uploading a local archive from backups/
  fetch SNAPSHOT_ID              Retrieve and verify one remote backup into backups/
  maintenance                    Run privacy retention with the dedicated database role
  media-plan NAME.json UUID...    Create an explicit media purge plan in backups/plans/
  media-apply NAME.json --maintenance-confirmed --break-links-confirmed
                                 Stop blog, apply the reviewed plan, then restart

Uses this deployment's existing .env. No additional env file is required.
EOF
}

action=${1:-help}
case "$action" in
    help|--help|-h) usage; exit 0 ;;
    backup|verify|restore|check|release|status|remote-init|remote-list|sync|fetch|restore-data|maintenance|media-plan|media-apply) ;;
    *) usage >&2; exit 2 ;;
esac
shift
[ -f .env ] && [ ! -L .env ] || { echo 'Initialize the deployment .env first.' >&2; exit 1; }
chmod 600 .env
[ ! -L backups ] || { echo 'backups must not be a symbolic link.' >&2; exit 1; }
mkdir -p backups
chmod 700 backups

dc() { docker compose --project-directory "$ROOT" "$@"; }
ops() {
    dc run --name "$OPS_CONTAINER" --rm --no-deps -T \
        -e "BLOG_HOST_UID=$(id -u)" -e "BLOG_HOST_GID=$(id -g)" "$@"
}
input_archive() {
    [ -f "$1" ] && [ ! -L "$1" ] || { echo 'Archive must be a regular file.' >&2; exit 1; }
    ARCHIVE_PATH=$(CDPATH= cd "$(dirname "$1")" && pwd)/$(basename "$1")
}

if [ "$action" = status ]; then
    if [ -f backups/status.json ]; then cat backups/status.json; else echo 'No backup/recovery operation recorded.'; fi
    if [ -f backups/last-successful-backup.json ]; then cat backups/last-successful-backup.json; fi
    for archive in backups/blog-*.tar.gz; do [ ! -f "$archive" ] || ls -lh "$archive"; done
    exit 0
fi

# Atomic directory lock also covers remote retention and recovery release. A hard
# kill intentionally leaves it in place; do not guess that another operator is dead.
if ! mkdir backups/.operation-lock 2>/dev/null; then
    echo 'Another operation holds backups/.operation-lock; inspect it before retrying.' >&2
    exit 1
fi
OPS_CONTAINER=blog-ops-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')
printf '%s\n' "pid=$$ action=$action container=$OPS_CONTAINER" > backups/.operation-lock/owner
restart=0
phase=starting
archive_name=
started=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
write_status() {
    # All interpolated values are generated here, never untrusted error/secret text.
    printf '{"action":"%s","status":"%s","phase":"%s","started_at":"%s","finished_at":"%s","archive":"%s"}\n' \
        "$action" "$1" "$phase" "$started" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$archive_name" > backups/.status.tmp
    mv backups/.status.tmp backups/status.json
    if [ "$action" = backup ] && [ "$1" = success ]; then
        cp backups/status.json backups/last-successful-backup.json
    fi
}
cleanup() {
    code=$?
    trap - 0 HUP INT TERM
    # An interrupted docker client may leave its container working. Stop that
    # exact operation before allowing the source application to write again.
    if docker inspect "$OPS_CONTAINER" >/dev/null 2>&1; then
        if ! docker stop --time 30 "$OPS_CONTAINER" >/dev/null; then
            code=1; phase=ops-stop-failed; restart=0
        fi
    fi
    if [ "$restart" = 1 ]; then
        if ! dc start --wait --wait-timeout 90 blog; then code=1; phase=restart-failed; fi
    fi
    if [ "$phase" != ops-stop-failed ]; then rm -rf backups/.context; fi
    if [ "$code" = 0 ]; then write_status success; else write_status failed; fi
    if [ "$phase" != ops-stop-failed ]; then
        rm -f backups/.operation-lock/owner
        rmdir backups/.operation-lock
    fi
    exit "$code"
}
trap cleanup 0
trap 'exit 130' HUP INT TERM
write_status running

case "$action" in
    media-plan)
        [ "$#" -ge 2 ] || { usage >&2; exit 2; }
        phase=media-plan
        ops ops media-plan "$@"
        ;;
    media-apply)
        [ "$#" = 3 ] && [ "$2" = --maintenance-confirmed ] && [ "$3" = --break-links-confirmed ] || { usage >&2; exit 2; }
        phase=stopping
        if [ -n "$(dc ps --status running -q blog)" ]; then
            restart=1
            dc stop blog
        fi
        phase=media-purge
        ops ops media-apply "$@"
        ;;
    maintenance)
        [ "$#" = 0 ] || { usage >&2; exit 2; }
        phase=privacy-retention
        ops maintenance
        ;;
    backup)
        [ "$#" = 0 ] || { usage >&2; exit 2; }
        phase=preflight
        container=$(dc ps --status running -q blog)
        [ -n "$container" ] || { echo 'Backup requires a running installed blog.' >&2; exit 1; }
        mkdir backups/.context
        dc --profile ops config --format json > backups/.context/compose.tmp
        mv backups/.context/compose.tmp backups/.context/compose.json
        # Capture only immutable image identity and effective environment, not
        # mutable health/log/host metadata from the full container inspection.
        docker inspect --format '{"Image":{{json .Image}},"Config":{"Env":{{json .Config.Env}},"Cmd":{{json .Config.Cmd}},"Entrypoint":{{json .Config.Entrypoint}}}}' \
            "$container" > backups/.context/container.tmp
        mv backups/.context/container.tmp backups/.context/container.json
        dc exec -T blog sha256sum /usr/local/bin/blog > backups/.context/binary.tmp
        mv backups/.context/binary.tmp backups/.context/binary.sha256
        ops ops preflight
        phase=stopping
        # Set before stopping so interruption or partial stop still attempts restart.
        restart=1
        dc stop blog
        phase=backup
        archive_name=$(ops ops backup)
        case "$archive_name" in blog-*.tar.gz) ;; *) echo 'Unexpected backup result.' >&2; exit 1 ;; esac
        phase=restart
        dc start --wait --wait-timeout 90 blog
        restart=0
        phase=remote-and-retention
        ops ops finalize "$archive_name"
        phase=complete
        echo "Backup saved: $ROOT/backups/$archive_name"
        ;;
    verify)
        [ "$#" = 1 ] || { usage >&2; exit 2; }
        input_archive "$1"
        phase=verify
        ops -v "$ARCHIVE_PATH:/input/backup.tar.gz:ro" ops verify
        ;;
    restore)
        [ "$#" = 2 ] || { usage >&2; exit 2; }
        input_archive "$1"
        # Exclusive mkdir prevents reuse of a running deployment or old volumes.
        target=$2
        [ ! -e "$target" ] && [ ! -L "$target" ] || { echo 'Restore directory must not already exist.' >&2; exit 1; }
        mkdir -m 700 "$target"
        target=$(CDPATH= cd "$target" && pwd)
        phase=prepare-restore
        ops -v "$ARCHIVE_PATH:/input/backup.tar.gz:ro" -v "$target:/target" ops prepare
        mkdir "$target/scripts" "$target/ops"
        cp compose.yaml "$target/compose.yaml"
        cp scripts/compose-backup.sh scripts/compose-init.sh "$target/scripts/"
        cp ops/postgres-init.sh "$target/ops/"
        cp .env.example "$target/.env.example"
        phase=isolated-restore
        # A clean environment prevents source COMPOSE_PROJECT_NAME/DATABASE_URL or
        # shell credentials from overriding the generated target .env.
        env -i PATH="$PATH" HOME="$HOME" \
            DOCKER_HOST="${DOCKER_HOST:-}" DOCKER_CONTEXT="${DOCKER_CONTEXT:-}" \
            sh "$target/scripts/compose-backup.sh" restore-data "$ARCHIVE_PATH"
        phase=complete
        echo "Restored into $target; run its check and release commands before starting blog."
        ;;
    restore-data)
        [ "$#" = 1 ] && [ -f .restore.json ] || { echo 'Use restore ARCHIVE NEW_DIRECTORY.' >&2; exit 1; }
        input_archive "$1"
        phase=starting-empty-database
        # prepare generated a random project. Never run the application here.
        dc up -d --no-build --wait --wait-timeout 90 db
        phase=isolated-restore
        ops -v "$ARCHIVE_PATH:/input/backup.tar.gz:ro" ops restore
        ;;
    check)
        [ -f .restore.json ] && [ "$#" -ge 1 ] && [ "$#" -le 2 ] || { usage >&2; exit 2; }
        phase=isolated-http-check
        if [ "$#" = 2 ]; then
            [ "$2" = --password-stdin ] || { usage >&2; exit 2; }
            ops ops check "$1" --password-stdin
        else
            dc run --name "$OPS_CONTAINER" --rm --no-deps ops check "$1"
        fi
        ;;
    release)
        [ "$#" = 0 ] && [ -f .restore.json ] || { usage >&2; exit 2; }
        phase=release
        # Fail if any verification/server container is still using the target DB.
        ops ops release
        phase=start-restored-blog
        dc up -d --no-build --wait --wait-timeout 90 blog
        dc port blog 8080
        ;;
    remote-init|remote-list)
        [ "$#" = 0 ] || { usage >&2; exit 2; }
        phase=$action
        ops ops "$action"
        ;;
    sync|fetch)
        [ "$#" = 1 ] || { usage >&2; exit 2; }
        phase=$action
        ops ops "$action" "$1"
        ;;
esac
