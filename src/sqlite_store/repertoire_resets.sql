CREATE TABLE repertoire_resets (
    command_id TEXT PRIMARY KEY,
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    repertoire_id TEXT NOT NULL,
    reset_at_ms INTEGER NOT NULL CHECK (reset_at_ms >= 0)
) STRICT;
CREATE INDEX repertoire_resets_latest ON repertoire_resets(dataset_id, repertoire_id, reset_at_ms);
CREATE INDEX attempts_target ON attempts(dataset_id, target_id);
CREATE INDEX assessments_target ON assessments(dataset_id, target_id);
CREATE INDEX receipts_attempt ON receipts(attempt_id) WHERE attempt_id IS NOT NULL;
