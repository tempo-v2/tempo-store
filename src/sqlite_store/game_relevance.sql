CREATE TABLE game_relevance_inputs (
    dataset_id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    occurred_ms INTEGER NOT NULL,
    PRIMARY KEY (dataset_id, source, game_id, learner),
    FOREIGN KEY (dataset_id, source, game_id) REFERENCES games(dataset_id, source, id)
) STRICT;
CREATE INDEX game_relevance_recent ON game_relevance_inputs(dataset_id, occurred_ms DESC, source, game_id);

CREATE TABLE game_encounters (
    dataset_id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    target_id TEXT NOT NULL,
    answer_version BLOB NOT NULL CHECK (length(answer_version)=8),
    missed INTEGER NOT NULL CHECK (missed IN (0,1)),
    PRIMARY KEY (dataset_id, source, game_id, learner, target_id),
    FOREIGN KEY (dataset_id, source, game_id, learner) REFERENCES game_relevance_inputs(dataset_id, source, game_id, learner),
    FOREIGN KEY (dataset_id, target_id, answer_version) REFERENCES content_history(dataset_id, target_id, version)
) STRICT;

CREATE TABLE game_target_relevance (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    target_id TEXT NOT NULL,
    exposure INTEGER NOT NULL,
    misses_by_version TEXT NOT NULL,
    window_games INTEGER NOT NULL,
    PRIMARY KEY (dataset_id, target_id)
) STRICT;

CREATE TABLE game_opponent_gaps (
    dataset_id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    repertoire_id TEXT NOT NULL,
    position TEXT NOT NULL,
    actual_move TEXT NOT NULL,
    document_revision TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source, game_id, learner, repertoire_id, position, actual_move),
    FOREIGN KEY (dataset_id, source, game_id) REFERENCES games(dataset_id, source, id)
) STRICT;
CREATE INDEX game_gap_group ON game_opponent_gaps(dataset_id, repertoire_id, position, actual_move, document_revision);

CREATE TABLE game_backfill_jobs (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    id TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (dataset_id, id)
) STRICT;

CREATE TABLE game_learner_accounts (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    learner TEXT NOT NULL,
    PRIMARY KEY (dataset_id, source, learner)
) STRICT;

CREATE TABLE game_move_models (
    model_key TEXT NOT NULL,
    position TEXT NOT NULL,
    distribution TEXT NOT NULL,
    PRIMARY KEY (model_key, position)
) STRICT;

CREATE TABLE game_model_reach (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    model_key TEXT NOT NULL,
    content_revision INTEGER NOT NULL,
    target_id TEXT NOT NULL,
    millionths INTEGER NOT NULL CHECK (millionths BETWEEN 0 AND 1000000),
    PRIMARY KEY (dataset_id, model_key, target_id)
) STRICT;
