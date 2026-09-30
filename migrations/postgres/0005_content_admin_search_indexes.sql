-- Installed by the migration/schema owner. pg_trgm is trusted; the application
-- and maintenance roles keep their existing privileges and cannot install it.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_extension e JOIN pg_namespace n ON n.oid=e.extnamespace
        WHERE e.extname='pg_trgm' AND n.nspname<>'public'
    ) THEN
        RAISE EXCEPTION 'pg_trgm must be installed in public before this migration; ask the database owner to provision it there';
    END IF;
END $$;
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;

CREATE INDEX posts_admin_active_idx ON posts (updated_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_admin_trash_idx ON posts (deleted_at DESC, id DESC) WHERE deleted_at IS NOT NULL;
CREATE INDEX posts_admin_author_active_idx ON posts (author_id, updated_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_admin_author_trash_idx ON posts (author_id, deleted_at DESC, id DESC) WHERE deleted_at IS NOT NULL;
CREATE INDEX pages_admin_active_idx ON pages (updated_at DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX pages_admin_trash_idx ON pages (deleted_at DESC, id DESC) WHERE deleted_at IS NOT NULL;

-- One candidate index instead of a separate large GIN index for every field.
-- Query predicates recheck each field to exclude a match spanning separators.
CREATE INDEX posts_admin_search_idx ON posts USING gin
    (lower(title || E'\n' || slug || E'\n' || content || E'\n' || coalesce(excerpt, '')) public.gin_trgm_ops);
CREATE INDEX pages_admin_search_idx ON pages USING gin
    (lower(title || E'\n' || slug || E'\n' || content) public.gin_trgm_ops);
