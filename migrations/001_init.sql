-- Videos
CREATE TABLE videos (
    id UUID PRIMARY KEY,
    share_token VARCHAR(21) UNIQUE,
    title VARCHAR(100) NOT NULL,
    format VARCHAR(10) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'PENDING_UPLOAD',
    upload_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_videos_share_token ON videos(share_token) WHERE share_token IS NOT NULL;
CREATE INDEX idx_videos_status ON videos(status);
CREATE INDEX idx_videos_status_created ON videos(status, created_at);

-- Tasks
--
-- Scheduling config (max_retries, retry_base_delay, execution_interval,
-- processing_timeout) is NOT denormalized onto the row. The consumer reads
-- it from the typed TaskMetadata trait at run time. This means config
-- changes apply immediately to existing tasks on next run, without a
-- migration. The trade-off is that during a rolling deploy, two workers
-- with different code versions may use slightly different config values
-- for the same task — acceptable at our scale.
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
