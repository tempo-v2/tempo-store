-- Day-local authority outlives any prepared generation.
CREATE TABLE study_day_authority (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    study_day INTEGER NOT NULL,
    opening_spent INTEGER NOT NULL DEFAULT 0 CHECK (opening_spent>=0),
    tactic_spent INTEGER NOT NULL DEFAULT 0 CHECK (tactic_spent>=0),
    spend_proven INTEGER NOT NULL DEFAULT 1 CHECK (spend_proven IN (0,1)),
    latest_skip INTEGER NOT NULL DEFAULT 0 CHECK (latest_skip>=0),
    skip_cutoff INTEGER NOT NULL DEFAULT 0 CHECK (skip_cutoff BETWEEN 0 AND latest_skip),
    PRIMARY KEY (dataset_id,study_day)
) STRICT;

CREATE TABLE study_deferrals (
    dataset_id TEXT NOT NULL,
    study_day INTEGER NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version)=8),
    work_kind TEXT NOT NULL CHECK (work_kind IN ('teach','test','correction')),
    obligation_revision BLOB NOT NULL CHECK (length(obligation_revision)=8),
    ordinal INTEGER NOT NULL CHECK (ordinal>0),
    PRIMARY KEY (dataset_id,study_day,target_id,version,work_kind,obligation_revision),
    UNIQUE (dataset_id,study_day,ordinal),
    FOREIGN KEY (dataset_id,study_day) REFERENCES study_day_authority(dataset_id,study_day)
) STRICT;
