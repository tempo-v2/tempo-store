CREATE TABLE study_plan_generations (
    generation INTEGER PRIMARY KEY AUTOINCREMENT,
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    study_day INTEGER NOT NULL,
    membership_epoch BLOB NOT NULL CHECK (length(membership_epoch)=8),
    rank_revision BLOB NOT NULL CHECK (length(rank_revision)=8),
    expected_entries INTEGER NOT NULL CHECK (expected_entries>=0),
    staged_entries INTEGER NOT NULL DEFAULT 0 CHECK (staged_entries BETWEEN 0 AND expected_entries),
    phase TEXT NOT NULL DEFAULT 'staging' CHECK (phase IN ('staging','published','reclaiming')),
    UNIQUE (dataset_id,generation)
) STRICT;

CREATE TABLE study_projection_state (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id),
    study_day INTEGER,
    membership_epoch BLOB NOT NULL DEFAULT X'0000000000000000' CHECK (length(membership_epoch)=8),
    rank_revision BLOB NOT NULL DEFAULT X'0000000000000000' CHECK (length(rank_revision)=8),
    summary_epoch BLOB NOT NULL DEFAULT X'0000000000000000' CHECK (length(summary_epoch)=8),
    summary_dirty INTEGER NOT NULL DEFAULT 1 CHECK (summary_dirty IN (0,1)),
    head_generation INTEGER,
    FOREIGN KEY (dataset_id,head_generation)
        REFERENCES study_plan_generations(dataset_id,generation)
) STRICT;

-- Work authority lives with the attempt/Learning. No authoritative row refers
-- to these disposable rows. Removing a generation cannot cascade its entries.
CREATE TABLE study_plan_entries (
    generation INTEGER NOT NULL REFERENCES study_plan_generations(generation),
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version)=8),
    work_kind TEXT NOT NULL CHECK (work_kind IN ('teach','test','correction')),
    obligation_revision BLOB NOT NULL CHECK (length(obligation_revision)=8),
    priority_class INTEGER NOT NULL CHECK (priority_class IN (0,1,2)),
    due_day INTEGER,
    ordinal BLOB NOT NULL CHECK (length(ordinal)=8),
    state TEXT NOT NULL CHECK (state IN ('ready','issued','deferred','consumed')),
    PRIMARY KEY (generation,target_id,version,work_kind,obligation_revision),
    CHECK (
        (priority_class=0 AND work_kind='correction' AND due_day IS NULL)
        OR (priority_class=1 AND work_kind='test' AND due_day IS NOT NULL)
        OR (priority_class=2 AND work_kind='teach' AND due_day IS NULL)
    )
) STRICT;

CREATE INDEX study_plan_entries_ready ON study_plan_entries(
    generation,priority_class,due_day,ordinal,target_id,version,work_kind,obligation_revision
) WHERE state='ready';
