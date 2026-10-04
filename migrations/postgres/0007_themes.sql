-- Theme settings are independent of settings.theme (the active selection).
CREATE TABLE themes (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]{1,64}$'),
    config jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(config) = 'object' AND octet_length(config::text) <= 131072),
    config_schema_version integer NOT NULL CHECK (config_schema_version > 0),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    release text NOT NULL CHECK (release ~ '^[0-9a-f]{64}$'),
    -- Persist only field names needed to verify media refs without executing a package.
    media_fields text[] NOT NULL DEFAULT '{}' CHECK (cardinality(media_fields) <= 64 AND array_position(media_fields, NULL) IS NULL),
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);
ALTER TABLE media_refs DROP CONSTRAINT media_refs_source_type_check;
ALTER TABLE media_refs ADD CONSTRAINT media_refs_source_type_check CHECK (source_type IN ('post','page','series','user','site','theme'));
