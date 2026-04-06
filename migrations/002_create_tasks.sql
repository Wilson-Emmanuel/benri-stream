CREATE TABLE tasks (
    id UUID PRIMARY KEY,
    metadata_type VARCHAR(100) NOT NULL,
    metadata TEXT NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'PENDING',
    ordering_key VARCHAR(200),
    trace_id VARCHAR(32),
    attempt_count INTEGER NOT NULL DEFAULT 0,
    next_run_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    error TEXT,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tasks_status_next_run ON tasks(status, next_run_at) WHERE status = 'PENDING';
CREATE INDEX idx_tasks_status_started ON tasks(status, started_at) WHERE status = 'IN_PROGRESS';
CREATE INDEX idx_tasks_metadata_type ON tasks(metadata_type);

-- Enforces dedup-by-default semantics for TaskScheduler::schedule at the
-- database level. At most one active (PENDING or IN_PROGRESS) task may exist
-- per (metadata_type, ordering_key) pair. Null ordering keys are excluded —
-- tasks without an ordering key opt out of dedup.
--
-- TaskScheduler also performs an in-transaction lookup via
-- find_active_by_ordering_key before insert, which handles the common path
-- without provoking a constraint violation. This index is the backstop
-- against races where two concurrent transactions both see "no active task"
-- under READ COMMITTED isolation.
CREATE UNIQUE INDEX idx_tasks_active_ordering_key
    ON tasks(metadata_type, ordering_key)
    WHERE status IN ('PENDING', 'IN_PROGRESS') AND ordering_key IS NOT NULL;
