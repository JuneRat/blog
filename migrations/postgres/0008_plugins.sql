-- Per-plugin configuration is independent of site settings. The singleton keeps
-- the existing list-wide CAS and content-render revision in the same transaction.
CREATE TABLE plugin_runtime (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    schema_version integer NOT NULL CHECK (schema_version = 1),
    render_revision integer NOT NULL CHECK (render_revision BETWEEN 0 AND 2097151),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    updated_at timestamptz NOT NULL
);

CREATE TABLE plugins (
    id text PRIMARY KEY CHECK (id ~ '^[a-z][a-z0-9-]{0,47}$'),
    runtime_id boolean NOT NULL DEFAULT true REFERENCES plugin_runtime(singleton) CHECK (runtime_id),
    enabled boolean NOT NULL DEFAULT false,
    -- Covers 16 bounded text fields even when JSON escapes each control byte.
    config jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(config) = 'object' AND octet_length(config::text) <= 262144),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

-- Reject incompatible/corrupt legacy data before removing its source. Preserve
-- unavailable plugins too; removing compiled code must not erase configuration.
DO $$
DECLARE
    legacy jsonb;
    entry record;
    field record;
BEGIN
    SELECT value INTO legacy FROM settings WHERE key = 'plugins';
    IF FOUND THEN
        IF jsonb_typeof(legacy) IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION 'Invalid legacy plugin settings';
        END IF;
        IF (SELECT count(*) FROM jsonb_object_keys(legacy)) <> 3
            OR legacy->'schema_version' IS DISTINCT FROM '1'::jsonb
            OR jsonb_typeof(legacy->'render_revision') IS DISTINCT FROM 'number'
            OR (legacy->>'render_revision') !~ '^[0-9]+$'
            OR jsonb_typeof(legacy->'plugins') IS DISTINCT FROM 'object' THEN
            RAISE EXCEPTION 'Invalid or incompatible legacy plugin settings';
        END IF;
        IF (legacy->>'render_revision')::numeric NOT BETWEEN 0 AND 2097151
            OR (SELECT count(*) FROM jsonb_object_keys(legacy->'plugins')) > 128 THEN
            RAISE EXCEPTION 'Legacy plugin settings exceed supported limits';
        END IF;
        FOR entry IN SELECT key, value FROM jsonb_each(legacy->'plugins') LOOP
            IF entry.key !~ '^[a-z][a-z0-9-]{0,47}$'
                OR jsonb_typeof(entry.value) IS DISTINCT FROM 'object' THEN
                RAISE EXCEPTION 'Invalid legacy plugin state';
            END IF;
            IF (SELECT count(*) FROM jsonb_object_keys(entry.value)) <> 2
                OR jsonb_typeof(entry.value->'enabled') IS DISTINCT FROM 'boolean'
                OR jsonb_typeof(entry.value->'config') IS DISTINCT FROM 'object' THEN
                RAISE EXCEPTION 'Invalid legacy plugin state';
            END IF;
            IF (SELECT count(*) FROM jsonb_object_keys(entry.value->'config')) > 16 THEN
                RAISE EXCEPTION 'Legacy plugin configuration exceeds supported limits';
            END IF;
            FOR field IN SELECT key, value FROM jsonb_each(entry.value->'config') LOOP
                IF field.key !~ '^[a-z][a-z0-9-]{0,47}$'
                    OR jsonb_typeof(field.value) NOT IN ('boolean', 'number', 'string') THEN
                    RAISE EXCEPTION 'Invalid legacy plugin configuration';
                END IF;
                IF jsonb_typeof(field.value) = 'string'
                    AND octet_length(field.value #>> '{}') > 2048 THEN
                    RAISE EXCEPTION 'Legacy plugin configuration text is too long';
                END IF;
                IF jsonb_typeof(field.value) = 'number' THEN
                    IF (field.value #>> '{}') !~ '^-?[0-9]+$'
                        OR (field.value #>> '{}')::numeric NOT BETWEEN -2147483648 AND 2147483647 THEN
                        RAISE EXCEPTION 'Invalid legacy plugin configuration integer';
                    END IF;
                END IF;
            END LOOP;
        END LOOP;
    END IF;
END $$;

INSERT INTO plugin_runtime (schema_version, render_revision, version, updated_at)
SELECT (value->>'schema_version')::integer, (value->>'render_revision')::integer, version, updated_at
FROM settings WHERE key = 'plugins';

-- Legacy storage has only updated_at; use it for both timestamps on migrated rows.
INSERT INTO plugins (id, enabled, config, version, created_at, updated_at)
SELECT entry.key, (entry.value->>'enabled')::boolean, entry.value->'config', settings.version,
       settings.updated_at, settings.updated_at
FROM settings CROSS JOIN LATERAL jsonb_each(settings.value->'plugins') AS entry
WHERE settings.key = 'plugins';

DELETE FROM settings WHERE key = 'plugins';
