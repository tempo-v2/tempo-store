CREATE TABLE tactical_qualifications (
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL,
    result TEXT NOT NULL CHECK (result IN ('pending','kept','shortened','extended','revised','withheld')),
    PRIMARY KEY (dataset_id,target_id),
    FOREIGN KEY (dataset_id,target_id,version) REFERENCES content_history(dataset_id,target_id,version)
) STRICT;

CREATE VIEW withheld_game_tactics AS
SELECT q.dataset_id,q.target_id FROM tactical_qualifications q
JOIN current_content c USING(dataset_id,target_id,version)
WHERE q.result IN ('pending','withheld');

DROP VIEW tactical_report_requests;
CREATE VIEW tactical_report_requests AS
SELECT r.* FROM tactical_requests r
WHERE r.fulfilled=1 OR NOT EXISTS (
    SELECT 1 FROM study_exclusions x
    WHERE (x.dataset_id,x.target_id)=(r.dataset_id,r.target_id)
    UNION ALL
    SELECT 1 FROM withheld_game_tactics w
    WHERE (w.dataset_id,w.target_id)=(r.dataset_id,r.target_id)
);
