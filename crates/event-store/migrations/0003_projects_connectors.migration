-- Projects are the owner-scoped aggregate shown consistently on every signed-in device
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    owner_subject TEXT NOT NULL,
    title TEXT NOT NULL,
    source_language TEXT NOT NULL,
    target_language TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_projects_owner_updated
    ON projects(owner_subject, updated_at DESC, id);

-- The separate relation keeps the existing sessions table and its event foreign keys compatible
CREATE TABLE IF NOT EXISTS project_sessions (
    project_id TEXT NOT NULL REFERENCES projects(id),
    session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id),
    created_by_subject TEXT NOT NULL,
    created_by_device TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(project_id, session_id)
);

CREATE INDEX IF NOT EXISTS idx_project_sessions_project_created
    ON project_sessions(project_id, created_at DESC, session_id);

-- Exactly one row per project makes recording ownership atomic across devices
CREATE TABLE IF NOT EXISTS recording_leases (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    session_id TEXT NOT NULL REFERENCES sessions(id),
    holder_device_id TEXT NOT NULL,
    lease_token_hash TEXT NOT NULL,
    generation INTEGER NOT NULL,
    heartbeat_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_recording_leases_expiry
    ON recording_leases(expires_at);

-- A monotonic project cursor supports durable Last-Event-ID replay on observers
CREATE TABLE IF NOT EXISTS project_updates (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT NOT NULL REFERENCES projects(id),
    session_id TEXT,
    update_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_project_updates_project_cursor
    ON project_updates(project_id, cursor);

-- Durable assembly state lets Core rebuild unfinished ASR windows after restart
CREATE TABLE IF NOT EXISTS audio_assembly_cursors (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source_id TEXT NOT NULL,
    last_assembled_sequence INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(session_id, source_id)
);

CREATE TABLE IF NOT EXISTS audio_windows (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source_id TEXT NOT NULL,
    first_sequence INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL,
    captured_at_ms INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    object_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(session_id, source_id, first_sequence, last_sequence)
);

-- Connector jobs are isolated from GPU jobs because they perform remote side effects
CREATE TABLE IF NOT EXISTS connector_jobs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    session_id TEXT REFERENCES sessions(id),
    connector TEXT NOT NULL,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'leased', 'completed', 'failed', 'conflict')),
    payload_json TEXT NOT NULL,
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

CREATE INDEX IF NOT EXISTS idx_connector_jobs_lease
    ON connector_jobs(status, available_at, created_at);

CREATE TABLE IF NOT EXISTS remote_object_maps (
    connector TEXT NOT NULL,
    object_type TEXT NOT NULL,
    local_id TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    remote_parent_id TEXT,
    content_hash TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(connector, object_type, local_id),
    UNIQUE(connector, remote_id)
);
