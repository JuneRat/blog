-- Bounded revision history also stores the pending editing copy of published content.
CREATE TABLE content_revisions (
    id UUID PRIMARY KEY,
    post_id UUID REFERENCES posts(id) ON DELETE CASCADE,
    page_id UUID REFERENCES pages(id) ON DELETE CASCADE,
    version BIGINT NOT NULL CHECK (version > 0),
    data JSONB NOT NULL CHECK (jsonb_typeof(data) = 'object'),
    media_ids UUID[] NOT NULL DEFAULT '{}',
    actor_id UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL,
    CHECK (num_nonnulls(post_id, page_id) = 1)
);
CREATE UNIQUE INDEX content_revisions_post_version ON content_revisions(post_id, version) WHERE post_id IS NOT NULL;
CREATE UNIQUE INDEX content_revisions_page_version ON content_revisions(page_id, version) WHERE page_id IS NOT NULL;
ALTER TABLE posts ADD COLUMN draft_revision_id UUID REFERENCES content_revisions(id) ON DELETE SET NULL;
ALTER TABLE pages ADD COLUMN draft_revision_id UUID REFERENCES content_revisions(id) ON DELETE SET NULL;
ALTER TABLE media_refs DROP CONSTRAINT media_refs_source_type_check;
ALTER TABLE media_refs ADD CONSTRAINT media_refs_source_type_check CHECK (source_type IN ('post','page','series','user','site','theme','revision'));
-- Polymorphic references must also disappear on revision pruning or parent cascade.
CREATE FUNCTION clear_revision_media_refs() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM media_refs WHERE source_type='revision' AND source_id=OLD.id;
    RETURN OLD;
END;
$$;
CREATE TRIGGER clear_revision_media_refs BEFORE DELETE ON content_revisions FOR EACH ROW EXECUTE FUNCTION clear_revision_media_refs();
