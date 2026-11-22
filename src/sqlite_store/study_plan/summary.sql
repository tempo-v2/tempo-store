CREATE TABLE IF NOT EXISTS study_summary_projection (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id),
    study_day INTEGER NOT NULL,
    summary_epoch BLOB NOT NULL CHECK(length(summary_epoch)=8),
    payload TEXT NOT NULL CHECK(length(payload)<=512)
) STRICT;
