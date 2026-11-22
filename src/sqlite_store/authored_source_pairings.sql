CREATE TABLE IF NOT EXISTS authored_source_pairings (
    dataset_id TEXT NOT NULL,
    repertoire_id TEXT NOT NULL,
    source_id TEXT NOT NULL REFERENCES content_sources(id),
    PRIMARY KEY (dataset_id, repertoire_id),
    FOREIGN KEY (dataset_id, repertoire_id)
        REFERENCES repertoire_documents(dataset_id, repertoire_id)
) STRICT;
