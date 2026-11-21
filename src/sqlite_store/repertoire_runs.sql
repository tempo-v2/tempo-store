CREATE TABLE IF NOT EXISTS repertoire_run_steps (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 3),
    repertoire_id TEXT NOT NULL,
    source_id TEXT NOT NULL REFERENCES content_sources(id),
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    attempt_id TEXT UNIQUE,
    mode TEXT NOT NULL CHECK (mode IN ('"test"', '"practice"')),
    lead_in TEXT,
    skipped_warmup INTEGER NOT NULL CHECK (skipped_warmup IN (0,1)),
    PRIMARY KEY (dataset_id, ordinal),
    UNIQUE (dataset_id, target_id),
    FOREIGN KEY (dataset_id, target_id, version)
        REFERENCES content_history(dataset_id, target_id, version),
    FOREIGN KEY (attempt_id, dataset_id, target_id, version)
        REFERENCES attempts(id, dataset_id, target_id, version)
) STRICT;

CREATE INDEX IF NOT EXISTS last_applied_human_test
ON assessments(dataset_id, target_id, version, ordinal DESC)
WHERE attempt_id IS NOT NULL
  AND json_extract(record, '$.transition.effect') NOT IN ('practice_only', 'stale_positive_ignored');
