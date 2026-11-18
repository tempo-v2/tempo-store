CREATE TABLE daily_tactics_allowances (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    study_day INTEGER NOT NULL,
    extra INTEGER NOT NULL CHECK (extra > 0),
    PRIMARY KEY (dataset_id, study_day)
) STRICT;
