-- Ordered pagination for deleted media and selective audit filters.
CREATE INDEX media_trash_idx ON media (created_at DESC, id DESC) WHERE deleted_at IS NOT NULL;
CREATE INDEX audit_logs_actor_time_idx ON audit_logs (actor_id, created_at DESC, id DESC);
CREATE INDEX audit_logs_action_time_idx ON audit_logs (action, created_at DESC, id DESC);
