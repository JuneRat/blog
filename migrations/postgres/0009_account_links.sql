-- One pending invitation/recovery credential per account; never store raw tokens.
CREATE TABLE account_links (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    token_hash text NOT NULL UNIQUE CHECK (length(token_hash) = 43),
    email text NOT NULL,
    auth_version bigint NOT NULL,
    issued_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at >= issued_at)
);
