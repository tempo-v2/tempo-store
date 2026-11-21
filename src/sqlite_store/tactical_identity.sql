-- These lookup rows are a projection of Chess-owned current tactic keys.
-- Retained historical definitions are independent of the projection.
CREATE TABLE tactical_identities (
    target_id TEXT PRIMARY KEY REFERENCES current_content(target_id) ON DELETE CASCADE,
    identity TEXT NOT NULL
) STRICT;
CREATE INDEX tactical_identities_exact ON tactical_identities(identity);

CREATE TABLE tactical_sources (
    token TEXT NOT NULL,
    target_id TEXT NOT NULL REFERENCES tactical_identities(target_id) ON DELETE CASCADE,
    PRIMARY KEY(token,target_id)
) STRICT;
CREATE INDEX tactical_sources_target ON tactical_sources(target_id);
