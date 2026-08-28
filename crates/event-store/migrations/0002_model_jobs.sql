-- 模型任务必须先持久化，Core 或 GPU Worker 重启后才能继续处理
CREATE TABLE IF NOT EXISTS model_jobs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    job_type TEXT NOT NULL,
    priority INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'leased', 'completed', 'failed')),
    input_json TEXT NOT NULL,
    input_object_hash TEXT,
    result_json TEXT,
    idempotency_key TEXT NOT NULL UNIQUE,
    attempts INTEGER NOT NULL DEFAULT 0,
    available_at TEXT NOT NULL,
    lease_owner TEXT,
    lease_expires_at TEXT,
    last_error_kind TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_model_jobs_lease
    ON model_jobs(status, available_at, priority DESC, created_at);
CREATE INDEX IF NOT EXISTS idx_model_jobs_session
    ON model_jobs(session_id, status, created_at);

-- Worker 心跳只保存能力与模型元数据，不保存设备主机名或用户身份
CREATE TABLE IF NOT EXISTS worker_nodes (
    id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    capabilities_json TEXT NOT NULL,
    model_metadata_json TEXT NOT NULL,
    active_job_id TEXT,
    last_seen_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
