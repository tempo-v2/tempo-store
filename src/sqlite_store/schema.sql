-- Version 1 separates current data from history retained across dataset changes.
CREATE TABLE datasets (
    id TEXT PRIMARY KEY
) STRICT;

CREATE TABLE content_history (
    ordinal INTEGER PRIMARY KEY,
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    item TEXT NOT NULL,
    UNIQUE (dataset_id, target_id, version)
) STRICT;

CREATE TABLE current_content (
    target_id TEXT PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    repertoire TEXT,
    position TEXT,
    item TEXT NOT NULL,
    CHECK ((repertoire IS NULL) = (position IS NULL)),
    UNIQUE (repertoire, position),
    UNIQUE (target_id, repertoire),
    FOREIGN KEY (dataset_id, target_id, version)
        REFERENCES content_history(dataset_id, target_id, version)
) STRICT;

CREATE TABLE repertoire_lines (
    ordinal INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    repertoire TEXT NOT NULL,
    UNIQUE (id, repertoire)
) STRICT;

CREATE TABLE line_targets (
    line_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    repertoire TEXT NOT NULL,
    target_id TEXT NOT NULL,
    PRIMARY KEY (line_id, ordinal),
    FOREIGN KEY (line_id, repertoire) REFERENCES repertoire_lines(id, repertoire),
    FOREIGN KEY (target_id, repertoire) REFERENCES current_content(target_id, repertoire)
) STRICT;

CREATE TABLE learning (
    target_id TEXT PRIMARY KEY REFERENCES current_content(target_id),
    state TEXT NOT NULL
) STRICT;

CREATE TABLE attempts (
    id TEXT PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    record TEXT NOT NULL,
    UNIQUE (id, dataset_id),
    UNIQUE (id, dataset_id, target_id, version),
    FOREIGN KEY (dataset_id, target_id, version)
        REFERENCES content_history(dataset_id, target_id, version)
) STRICT;

CREATE TABLE assessments (
    ordinal INTEGER PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    version BLOB NOT NULL CHECK (length(version) = 8),
    attempt_id TEXT,
    provider TEXT,
    event_id TEXT,
    record TEXT NOT NULL,
    CHECK (
        (attempt_id IS NOT NULL AND provider IS NULL AND event_id IS NULL)
        OR (attempt_id IS NULL AND provider IS NOT NULL AND event_id IS NOT NULL)
    ),
    FOREIGN KEY (dataset_id, target_id, version)
        REFERENCES content_history(dataset_id, target_id, version),
    FOREIGN KEY (attempt_id, dataset_id, target_id, version)
        REFERENCES attempts(id, dataset_id, target_id, version)
) STRICT;
CREATE UNIQUE INDEX human_assessment ON assessments(dataset_id, attempt_id)
    WHERE attempt_id IS NOT NULL;
CREATE UNIQUE INDEX external_assessment ON assessments(dataset_id, provider, event_id)
    WHERE attempt_id IS NULL;

CREATE TABLE receipts (
    id TEXT PRIMARY KEY,
    attempt_id TEXT REFERENCES attempts(id),
    command TEXT NOT NULL,
    result TEXT NOT NULL
) STRICT;

CREATE TABLE store_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    dataset_id TEXT NOT NULL REFERENCES datasets(id),
    revision BLOB NOT NULL CHECK (length(revision) = 8),
    active_attempt_id TEXT,
    policy TEXT NOT NULL,
    FOREIGN KEY (active_attempt_id, dataset_id) REFERENCES attempts(id, dataset_id)
) STRICT;
