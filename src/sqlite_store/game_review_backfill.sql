ALTER TABLE games ADD COLUMN mainline_signature TEXT;

-- Existing accepted Test attempts establish issuance and acceptance facts.
-- An absent attempted assessment or missing issuance remains unknown.
WITH assessed_tests AS (
    SELECT assessment.target_id, assessment.version,
           MAX(json_extract(attempt.record, '$.issued_at.unix_ms')) AS issued_ms,
           MAX(json_extract(assessment.record, '$.input.accepted_at.day')) AS accepted_day
    FROM assessments AS assessment
    JOIN attempts AS attempt ON attempt.id = assessment.attempt_id
    WHERE assessment.dataset_id = (SELECT dataset_id FROM store_state WHERE singleton = 1)
      AND json_extract(attempt.record, '$.mode') = 'test'
    GROUP BY assessment.target_id, assessment.version
), facts AS (
    SELECT assessed_tests.target_id,
           MAX(CASE WHEN current_content.version = assessed_tests.version
                    THEN assessed_tests.issued_ms END) AS issue_ms,
           MAX(assessed_tests.accepted_day) AS graded_day
    FROM assessed_tests
    JOIN current_content USING (target_id)
    GROUP BY assessed_tests.target_id
)
UPDATE learning
SET state = json_patch(
    learning.state,
    json_object('last_assessed_test_issue_ms', facts.issue_ms, 'last_graded_day', facts.graded_day)
)
FROM facts
WHERE facts.target_id = learning.target_id
  AND json_extract(learning.state, '$.last_graded_day') IS NULL
  AND facts.graded_day IS NOT NULL;

CREATE TABLE game_receipts (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    accepted TEXT NOT NULL,
    PRIMARY KEY (dataset_id, id)
) STRICT;

CREATE INDEX games_provider_url ON games(dataset_id, url) WHERE url IS NOT NULL;
