CREATE TABLE comment_settings (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    enabled boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0)
);
INSERT INTO comment_settings(id) VALUES(true);
CREATE TABLE post_comment_settings (
    post_id uuid PRIMARY KEY REFERENCES posts(id) ON DELETE CASCADE,
    enabled boolean NOT NULL DEFAULT true,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0)
);
CREATE TABLE comments (
    id uuid PRIMARY KEY,
    post_id uuid NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    parent_id uuid,
    user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    nickname text NOT NULL CHECK (char_length(nickname) BETWEEN 1 AND 64),
    body text NOT NULL CHECK (char_length(body) BETWEEN 1 AND 2000),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','rejected','spam')),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    request_id uuid NOT NULL UNIQUE,
    client_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(id, post_id),
    FOREIGN KEY(parent_id, post_id) REFERENCES comments(id, post_id) ON DELETE CASCADE,
    CHECK(parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX comments_public ON comments(post_id, parent_id, created_at, id) WHERE status='approved';
CREATE INDEX comments_moderation ON comments(status, created_at DESC, id DESC);
CREATE INDEX comments_client ON comments(client_hash, created_at DESC);
-- Replies have exactly one level, including direct database imports.
CREATE FUNCTION check_comment_parent() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND (NEW.parent_id IS DISTINCT FROM OLD.parent_id OR NEW.post_id <> OLD.post_id) THEN
        RAISE EXCEPTION 'comment relationship is immutable' USING ERRCODE='23514';
    END IF;
    IF NEW.parent_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM comments WHERE id=NEW.parent_id AND post_id=NEW.post_id AND parent_id IS NULL
    ) THEN RAISE EXCEPTION 'invalid comment parent' USING ERRCODE='23514'; END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER comments_parent BEFORE INSERT OR UPDATE OF parent_id, post_id ON comments
FOR EACH ROW EXECUTE FUNCTION check_comment_parent();
