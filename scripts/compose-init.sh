#!/bin/sh
# Initialize Compose's single .env without executing it or rotating credentials.
set -eu
cd "$(CDPATH= cd "$(dirname "$0")/.." && pwd)"
umask 077

if [ -L .env ] || { [ -e .env ] && [ ! -f .env ]; }; then
    echo '.env must be a regular file, not a symbolic link or directory.' >&2
    exit 1
fi
if [ ! -e .env ]; then
    # noclobber prevents two initializers from overwriting one another's file.
    (set -C; cat .env.example > .env)
fi
chmod 600 .env
temporary=$(mktemp .env.init.XXXXXX)
trap 'rm -f "$temporary"' 0
trap 'exit 1' HUP INT TERM

random_password() {
    od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
}

# Secrets enter awk through its environment, not process arguments or stdout.
BLOG_INIT_PG_PASSWORD="$(random_password)" \
BLOG_INIT_OWNER_PASSWORD="$(random_password)" awk '
BEGIN {
    keys[1] = "BLOG_POSTGRES_PASSWORD"
    keys[2] = "BLOG_OWNER_PASSWORD"
    values[keys[1]] = ENVIRON["BLOG_INIT_PG_PASSWORD"]
    values[keys[2]] = ENVIRON["BLOG_INIT_OWNER_PASSWORD"]
    for (i = 1; i <= 2; i++) {
        if (length(values[keys[i]]) != 64 || values[keys[i]] ~ /[^0-9a-f]/) exit 1
    }
}
{
    key = $0
    sub(/^[ \t]*(export[ \t]+)?/, "", key)
    sub(/[ \t]*=.*/, "", key)
    if (key in values && $0 ~ /=/) {
        if (++seen[key] > 1) {
            print "Duplicate database password key in .env; leaving its contents unchanged." > "/dev/stderr"
            failed = 1
            exit 1
        }
        value = $0
        sub(/^[^=]*=[ \t]*/, "", value)
        sub(/[ \t]+#.*/, "", value)
        sub(/[ \t]+$/, "", value)
        if (value == "" || value == "\"\"" || value == "\047\047" || value ~ /^#/) {
            print key "=" values[key]
            next
        }
    }
    print
}
END {
    if (!failed) {
        for (i = 1; i <= 2; i++) {
            if (!(keys[i] in seen)) print keys[i] "=" values[keys[i]]
        }
    }
}' .env > "$temporary"

if ! cmp -s .env "$temporary"; then
    mv "$temporary" .env
fi
echo '.env is ready. Existing values were preserved; missing database passwords were generated.'
