CREATE TABLE repertoire_coverage (
    dataset_id TEXT NOT NULL,
    repertoire_id TEXT NOT NULL,
    input_key TEXT NOT NULL,
    summary TEXT NOT NULL,
    PRIMARY KEY (dataset_id, repertoire_id),
    FOREIGN KEY (dataset_id, repertoire_id)
        REFERENCES repertoire_documents(dataset_id, repertoire_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE repertoire_coverage_gaps (
    dataset_id TEXT NOT NULL,
    repertoire_id TEXT NOT NULL,
    position TEXT NOT NULL,
    actual_move TEXT NOT NULL,
    probability REAL NOT NULL CHECK (probability > 0 AND probability <= 1),
    PRIMARY KEY (dataset_id, repertoire_id, position, actual_move),
    FOREIGN KEY (dataset_id, repertoire_id)
        REFERENCES repertoire_coverage(dataset_id, repertoire_id) ON DELETE CASCADE
) STRICT;
