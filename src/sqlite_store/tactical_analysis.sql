CREATE TABLE tactical_analyses (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    detector TEXT NOT NULL,
    analysis_version TEXT NOT NULL,
    game_fingerprint TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('complete','failed','timed_out','unsupported')),
    analyzed_at_ms INTEGER NOT NULL CHECK(analyzed_at_ms >= 0),
    decisions_scanned INTEGER NOT NULL CHECK(decisions_scanned >= 0),
    payload_fingerprint TEXT NOT NULL,
    error TEXT,
    acceptance TEXT NOT NULL,
    PRIMARY KEY(dataset_id,source,game_id,learner,detector,analysis_version,game_fingerprint),
    FOREIGN KEY(dataset_id,source,game_id) REFERENCES games(dataset_id,source,id)
) STRICT;

CREATE TABLE tactical_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    dataset_id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    detector TEXT NOT NULL,
    ply INTEGER NOT NULL CHECK(ply >= 0),
    motif TEXT NOT NULL,
    analysis_version TEXT NOT NULL,
    outcome TEXT NOT NULL,
    details_json TEXT NOT NULL CHECK(json_valid(details_json)),
    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
    current INTEGER NOT NULL DEFAULT 1 CHECK(current IN (0,1)),
    UNIQUE(dataset_id,source,game_id,learner,detector,ply,motif),
    UNIQUE(id,dataset_id),
    FOREIGN KEY(dataset_id,source,game_id) REFERENCES games(dataset_id,source,id)
) STRICT;
CREATE INDEX tactical_observations_read ON tactical_observations(dataset_id,detector,id DESC);

CREATE TABLE tactical_requests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    observation_id INTEGER NOT NULL UNIQUE,
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK(length(version)=8),
    accepted_at_ms INTEGER NOT NULL CHECK(accepted_at_ms >= 0),
    attempt_id TEXT UNIQUE,
    fulfilled INTEGER NOT NULL DEFAULT 0 CHECK(fulfilled IN (0,1)),
    cancelled INTEGER NOT NULL DEFAULT 0 CHECK(cancelled IN (0,1)),
    CHECK(fulfilled=0 OR attempt_id IS NOT NULL),
    FOREIGN KEY(observation_id,dataset_id) REFERENCES tactical_observations(id,dataset_id),
    FOREIGN KEY(dataset_id,target_id,version) REFERENCES content_history(dataset_id,target_id,version),
    FOREIGN KEY(attempt_id,dataset_id,target_id,version) REFERENCES attempts(id,dataset_id,target_id,version)
) STRICT;
CREATE INDEX tactical_requests_pending ON tactical_requests(dataset_id,id DESC)
    WHERE fulfilled=0 AND cancelled=0;
