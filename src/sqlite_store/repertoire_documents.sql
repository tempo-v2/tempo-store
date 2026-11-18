CREATE INDEX current_content_by_repertoire
    ON current_content(dataset_id, repertoire, target_id) WHERE repertoire IS NOT NULL;

CREATE TABLE repertoire_documents (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    repertoire_id TEXT NOT NULL,
    source_id TEXT NOT NULL REFERENCES content_sources(id),
    PRIMARY KEY (dataset_id, repertoire_id)
) STRICT;
