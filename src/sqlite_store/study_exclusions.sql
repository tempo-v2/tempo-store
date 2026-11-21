CREATE TABLE study_exclusions (
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    marked_version BLOB NOT NULL,
    PRIMARY KEY (dataset_id, target_id),
    FOREIGN KEY (dataset_id, target_id, marked_version)
        REFERENCES content_history(dataset_id, target_id, version)
) STRICT;

CREATE INDEX tactical_requests_card_origin ON tactical_requests(dataset_id, target_id, version, accepted_at_ms, id);
