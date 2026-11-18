CREATE TABLE game_failures (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    assessment TEXT,
    chosen_position TEXT,
    PRIMARY KEY (dataset_id, source, game_id, target_id)
) STRICT;
ALTER TABLE games ADD COLUMN played_date_ms INTEGER;
CREATE INDEX games_reprocess_start ON games(dataset_id, coalesce(start_ms, end_ms, played_date_ms, fetched_ms));
CREATE TABLE game_choice_resolutions (
    dataset_id TEXT NOT NULL,
    comparison_id TEXT NOT NULL,
    ply INTEGER NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    position TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source, game_id, position),
    FOREIGN KEY (dataset_id, comparison_id) REFERENCES game_comparisons(dataset_id, id)
) STRICT;
