CREATE TABLE tactical_analyses (
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    detector TEXT NOT NULL,
    analysis_version TEXT NOT NULL,
    game_fingerprint TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('complete','inconclusive','failed','timed_out','unsupported')),
    analyzed_at_ms INTEGER NOT NULL CHECK(analyzed_at_ms >= 0),
    decisions_scanned INTEGER NOT NULL CHECK(decisions_scanned >= 0),
    payload_fingerprint TEXT NOT NULL,
    error TEXT,
    acceptance TEXT NOT NULL,
    position_id INTEGER NOT NULL DEFAULT 0,
    deeper INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(dataset_id,source,game_id,learner,detector,analysis_version,game_fingerprint,position_id,deeper),
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
    producer_id TEXT NOT NULL,
    producer TEXT CHECK(producer IS NULL OR json_valid(producer)),
    analysis_version TEXT NOT NULL,
    outcome TEXT NOT NULL,
    details_json TEXT NOT NULL CHECK(json_valid(details_json)),
    observed_at_ms INTEGER NOT NULL CHECK(observed_at_ms >= 0),
    current INTEGER NOT NULL DEFAULT 1 CHECK(current IN (0,1)),
    UNIQUE(dataset_id,source,game_id,learner,detector,ply,motif,producer_id),
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

CREATE TABLE tactical_enrollment (
    id INTEGER PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    learner TEXT NOT NULL,
    priority INTEGER NOT NULL,
    expanded TEXT NOT NULL DEFAULT '',
    UNIQUE(dataset_id,source,game_id,learner),
    FOREIGN KEY(dataset_id,source,game_id) REFERENCES games(dataset_id,source,id)
) STRICT;
CREATE INDEX tactical_enrollment_pending ON tactical_enrollment(dataset_id,priority DESC,id DESC)
;

CREATE TABLE tactical_positions (
    id INTEGER PRIMARY KEY,
    enrollment_id INTEGER NOT NULL REFERENCES tactical_enrollment(id),
    game_fingerprint TEXT NOT NULL,
    ply INTEGER NOT NULL CHECK(ply >= 0),
    analysis_version TEXT NOT NULL,
    analyses TEXT NOT NULL CHECK(json_valid(analyses)),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending','complete','inconclusive')),
    deeper INTEGER NOT NULL DEFAULT 0 CHECK(deeper IN (0,1)),
    UNIQUE(enrollment_id,game_fingerprint,ply,analysis_version)
) STRICT;
CREATE INDEX tactical_positions_pending ON tactical_positions(enrollment_id,ply)
    WHERE status='pending';

CREATE TABLE tactical_pause (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id),
    paused INTEGER NOT NULL CHECK(paused IN (0,1))
) STRICT;

CREATE TABLE tactical_study_turn (
    dataset_id TEXT PRIMARY KEY REFERENCES datasets(id)
) STRICT;
