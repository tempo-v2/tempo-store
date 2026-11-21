CREATE TABLE study_exclusions (
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    marked_version BLOB NOT NULL,
    PRIMARY KEY (dataset_id, target_id),
    FOREIGN KEY (dataset_id, target_id, marked_version)
        REFERENCES content_history(dataset_id, target_id, version)
) STRICT;

CREATE INDEX tactical_requests_card_origin ON tactical_requests(dataset_id, target_id, version, id);

-- Game reports retain completed training links and hide excluded unfinished work.
CREATE VIEW tactical_report_requests AS
SELECT r.* FROM tactical_requests r
WHERE r.fulfilled=1 OR NOT EXISTS (
    SELECT 1 FROM study_exclusions x
    WHERE (x.dataset_id,x.target_id)=(r.dataset_id,r.target_id)
);
