CREATE TABLE repertoire_suspensions (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    repertoire_id TEXT NOT NULL,
    PRIMARY KEY (dataset_id, repertoire_id)
) STRICT;

CREATE TABLE game_participation_revisions (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id),
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;
