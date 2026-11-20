CREATE TABLE repertoire_suspensions (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    repertoire_id TEXT NOT NULL,
    PRIMARY KEY (dataset_id, repertoire_id)
) STRICT;
