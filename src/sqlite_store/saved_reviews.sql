CREATE TABLE saved_reviews (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL,
    decision_index INTEGER NOT NULL CHECK (decision_index >= 0),
    attempt_id TEXT NOT NULL,
    saved_at_ms INTEGER NOT NULL CHECK (saved_at_ms >= 0),
    UNIQUE (dataset_id, target_id, version, decision_index),
    FOREIGN KEY (attempt_id, dataset_id, target_id, version)
        REFERENCES attempts(id, dataset_id, target_id, version) ON DELETE CASCADE
) STRICT;

CREATE INDEX saved_reviews_page ON saved_reviews(dataset_id, id DESC);
CREATE INDEX saved_reviews_attempt ON saved_reviews(attempt_id);
CREATE INDEX completed_attempts_by_version ON attempts(dataset_id, target_id, version)
    WHERE json_extract(record, '$.attempt.status') = 'complete';

CREATE TABLE saved_review_receipts (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    submission_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    review_id INTEGER NOT NULL,
    PRIMARY KEY (dataset_id, submission_id)
) STRICT;
