-- WAL 提供持续写入期间的并发读取，启动代码会在迁移前显式启用
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

-- 会话表只保存当前投影，所有状态变化仍由 events 表追溯
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    source_language TEXT NOT NULL,
    target_language TEXT NOT NULL,
    privacy_mode TEXT NOT NULL,
    consent_confirmed INTEGER NOT NULL CHECK (consent_confirmed IN (0, 1)),
    demo_mode INTEGER NOT NULL CHECK (demo_mode IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- 事件是系统事实来源，同一来源序号只能落盘一次
CREATE TABLE IF NOT EXISTS events (
    event_id TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    captured_at_monotonic_ns INTEGER NOT NULL,
    captured_at_wall TEXT NOT NULL,
    ingested_at TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    causation_id TEXT,
    content_hash TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    UNIQUE(session_id, source_id, sequence)
);

CREATE INDEX IF NOT EXISTS idx_events_session_ingested
    ON events(session_id, ingested_at, event_id);
CREATE INDEX IF NOT EXISTS idx_events_session_type
    ON events(session_id, event_type, sequence);

-- 音频元数据引用内容寻址对象，ACK 状态与对象文件分离
CREATE TABLE IF NOT EXISTS audio_chunks (
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    captured_at_ms INTEGER NOT NULL,
    sample_rate INTEGER NOT NULL,
    channels INTEGER NOT NULL,
    encoding TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    object_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    acknowledged_at TEXT NOT NULL,
    PRIMARY KEY(session_id, source_id, sequence)
);

-- 资产原始文件和页级派生产物都使用稳定 ID 与对象哈希
CREATE TABLE IF NOT EXISTS assets (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    original_name TEXT NOT NULL,
    media_type TEXT NOT NULL,
    object_hash TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS asset_pages (
    id TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL REFERENCES assets(id),
    page_number INTEGER NOT NULL,
    title TEXT,
    text_content TEXT NOT NULL,
    object_hash TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(asset_id, page_number)
);

