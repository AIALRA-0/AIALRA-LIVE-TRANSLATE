-- 首次领取时间独立保存，任务完成时更新 model_jobs.updated_at 也不会覆盖排队等待时长
CREATE TABLE IF NOT EXISTS model_job_metrics (
    job_id TEXT PRIMARY KEY REFERENCES model_jobs(id) ON DELETE CASCADE,
    first_leased_at TEXT NOT NULL
);
