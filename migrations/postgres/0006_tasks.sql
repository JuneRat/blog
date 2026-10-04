-- Three explicit maintenance kinds; migrations never seed runtime schedules.
CREATE TABLE task_runs (
    id uuid PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('html_rebuild','retention','publish_due')),
    status text NOT NULL CHECK (status IN ('queued','running','completed','failed','interrupted','cancelled')),
    trigger text NOT NULL CHECK (trigger IN ('manual','once','periodic','retry')),
    run_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    started_at timestamptz,
    finished_at timestamptz,
    retry_of uuid REFERENCES task_runs(id) ON DELETE SET NULL,
    report jsonb NOT NULL DEFAULT '{"html":null,"retention":null,"publication":null,"error":null}'::jsonb CHECK (jsonb_typeof(report)='object'),
    audit_actor_id uuid REFERENCES users(id) ON DELETE SET NULL,
    audit_ip_address inet,
    worker_id uuid,
    lease_token uuid,
    lease_expires_at timestamptz,
    CHECK ((status='running' AND worker_id IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL)
        OR (status<>'running' AND worker_id IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL)),
    CHECK ((status IN ('queued','running') AND finished_at IS NULL)
        OR (status NOT IN ('queued','running') AND finished_at IS NOT NULL))
);
CREATE UNIQUE INDEX task_runs_one_active_kind ON task_runs(kind) WHERE status IN ('queued','running');
CREATE INDEX task_runs_due ON task_runs(run_at,id) WHERE status='queued';
CREATE INDEX task_runs_history ON task_runs(created_at DESC,id DESC);
CREATE INDEX task_runs_kind_history ON task_runs(kind,created_at DESC,id DESC);
CREATE INDEX task_runs_expired ON task_runs(lease_expires_at) WHERE status='running';

CREATE TABLE task_schedules (
    kind text PRIMARY KEY CHECK (kind IN ('retention','publish_due')),
    enabled boolean NOT NULL,
    interval_seconds bigint NOT NULL,
    next_run_at timestamptz,
    version bigint NOT NULL DEFAULT 1 CHECK (version>0),
    CHECK ((kind='publish_due' AND enabled AND interval_seconds=30)
        OR (kind='retention' AND interval_seconds BETWEEN 3600 AND 2592000)),
    CHECK ((enabled AND next_run_at IS NOT NULL) OR (NOT enabled AND next_run_at IS NULL))
);
