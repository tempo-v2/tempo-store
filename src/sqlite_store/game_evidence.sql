CREATE TABLE games (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    id TEXT NOT NULL,
    url TEXT,
    white TEXT NOT NULL,
    black TEXT NOT NULL,
    start_ms INTEGER,
    end_ms INTEGER,
    fetched_ms INTEGER NOT NULL,
    pgn TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source, id),
    CHECK (start_ms <= end_ms)
) STRICT;

CREATE TABLE game_comparisons (
    ordinal INTEGER PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    summary TEXT NOT NULL,
    UNIQUE (dataset_id, id),
    FOREIGN KEY (dataset_id, source, game_id) REFERENCES games(dataset_id, source, id)
) STRICT;
CREATE INDEX game_comparisons_recent ON game_comparisons(dataset_id, ordinal DESC);

CREATE TABLE game_observations (
    dataset_id TEXT NOT NULL,
    comparison_id TEXT NOT NULL,
    ply INTEGER NOT NULL CHECK (ply > 0),
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    played TEXT NOT NULL,
    prescribed TEXT NOT NULL,
    PRIMARY KEY (dataset_id, comparison_id, ply),
    FOREIGN KEY (dataset_id, comparison_id) REFERENCES game_comparisons(dataset_id, id),
    FOREIGN KEY (dataset_id, target_id, version) REFERENCES content_history(dataset_id, target_id, version)
) STRICT;
CREATE INDEX game_observations_target ON game_observations(dataset_id, target_id, version);

CREATE TABLE successful_game_refreshes (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id),
    at_ms INTEGER NOT NULL
) STRICT;
