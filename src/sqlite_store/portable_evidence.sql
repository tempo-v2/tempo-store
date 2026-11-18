CREATE TABLE restored_reset_boundaries (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    repertoire_id TEXT NOT NULL,
    reset_at_ms INTEGER NOT NULL CHECK (reset_at_ms >= 0),
    PRIMARY KEY (dataset_id, repertoire_id)
) STRICT;

CREATE TABLE restored_game_choices (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    position TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source, game_id, position)
) STRICT;

CREATE VIEW game_choice_boundaries AS
    SELECT dataset_id, source, game_id, position FROM game_choice_resolutions
    UNION ALL
    SELECT dataset_id, source, game_id, position FROM restored_game_choices;
