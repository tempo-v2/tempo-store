CREATE TABLE repertoire_coverage_start (
    dataset_id TEXT NOT NULL,
    repertoire_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    moves TEXT NOT NULL CHECK (json_valid(moves) AND json_type(moves) = 'array'),
    PRIMARY KEY (dataset_id, repertoire_id),
    FOREIGN KEY (dataset_id, repertoire_id)
        REFERENCES repertoire_documents(dataset_id, repertoire_id) ON DELETE CASCADE
) STRICT;
