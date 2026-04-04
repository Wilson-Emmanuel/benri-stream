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
