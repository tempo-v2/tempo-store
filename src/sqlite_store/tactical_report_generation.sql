-- Report data changes independently of Study revision. Store reads this row in
-- the same SQLite snapshot as the facts. Training request status is live detail.
CREATE TABLE tactical_report_generation (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    evidence INTEGER NOT NULL CHECK(evidence>=0),
    archive INTEGER NOT NULL CHECK(archive>=0)
) STRICT;
INSERT INTO tactical_report_generation VALUES(1,0,0);

CREATE INDEX tactical_report_comparisons
ON game_comparisons(dataset_id,source,game_id,ordinal DESC);

CREATE INDEX tactical_report_current_game
ON tactical_observations(dataset_id,source,game_id,learner,report,ply)
WHERE current=1;

CREATE INDEX tactical_report_decisions
ON tactical_observations(dataset_id,report,analysis_version,motif,id DESC)
WHERE current=1;

CREATE TRIGGER tactical_report_games_insert AFTER INSERT ON games
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_games_update AFTER UPDATE OF white,black,start_ms,end_ms,pgn,mainline_signature ON games
WHEN OLD.white IS NOT NEW.white
  OR OLD.black IS NOT NEW.black
  OR OLD.start_ms IS NOT NEW.start_ms
  OR OLD.end_ms IS NOT NEW.end_ms
  OR OLD.pgn IS NOT NEW.pgn
  OR OLD.mainline_signature IS NOT NEW.mainline_signature
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_games_delete AFTER DELETE ON games
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_game_comparisons_insert AFTER INSERT ON game_comparisons
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_game_comparisons_update AFTER UPDATE OF dataset_id,source,game_id,summary ON game_comparisons
WHEN OLD.dataset_id IS NOT NEW.dataset_id
  OR OLD.source IS NOT NEW.source
  OR OLD.game_id IS NOT NEW.game_id
  OR OLD.summary IS NOT NEW.summary
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_game_comparisons_delete AFTER DELETE ON game_comparisons
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1, archive=archive+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_observations_insert AFTER INSERT ON tactical_observations
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_observations_update AFTER UPDATE ON tactical_observations
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_observations_delete AFTER DELETE ON tactical_observations
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_analyses_insert AFTER INSERT ON tactical_analyses
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_analyses_update AFTER UPDATE ON tactical_analyses
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_analyses_delete AFTER DELETE ON tactical_analyses
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_positions_insert AFTER INSERT ON tactical_positions
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_positions_update AFTER UPDATE OF status ON tactical_positions
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_positions_delete AFTER DELETE ON tactical_positions
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_enrollment_insert AFTER INSERT ON tactical_enrollment
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_enrollment_update AFTER UPDATE OF expanded,learner ON tactical_enrollment
WHEN OLD.expanded IS NOT NEW.expanded
  OR OLD.learner IS NOT NEW.learner
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;

CREATE TRIGGER tactical_report_tactical_enrollment_delete AFTER DELETE ON tactical_enrollment
BEGIN
    UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1;
END;
