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
