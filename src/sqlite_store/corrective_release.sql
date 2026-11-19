DROP VIEW game_choice_boundaries;

ALTER TABLE game_choice_resolutions RENAME TO legacy_choice_resolutions;
CREATE TABLE game_choice_resolutions (
    dataset_id TEXT NOT NULL,
    comparison_id TEXT NOT NULL,
    ply INTEGER NOT NULL,
    source TEXT NOT NULL,
    game_id TEXT NOT NULL,
    position TEXT NOT NULL,
    selected_target_id TEXT,
    selected_answer_version BLOB,
    CHECK ((selected_target_id IS NULL AND selected_answer_version IS NULL)
        OR (selected_target_id IS NOT NULL AND selected_answer_version IS NOT NULL
            AND length(selected_answer_version)=8
            AND selected_answer_version<>x'0000000000000000')),
    PRIMARY KEY (dataset_id, source, game_id, position),
    FOREIGN KEY (dataset_id, comparison_id) REFERENCES game_comparisons(dataset_id, id),
    FOREIGN KEY (dataset_id, selected_target_id, selected_answer_version)
        REFERENCES content_history(dataset_id, target_id, version)
) STRICT;
INSERT INTO game_choice_resolutions (dataset_id, comparison_id, ply, source, game_id, position)
SELECT dataset_id, comparison_id, ply, source, game_id, position FROM legacy_choice_resolutions;
DROP TABLE legacy_choice_resolutions;

ALTER TABLE restored_game_choices ADD COLUMN selected_target_id TEXT;
ALTER TABLE restored_game_choices ADD COLUMN selected_answer_version BLOB CHECK (
    (selected_target_id IS NULL AND selected_answer_version IS NULL)
    OR (selected_target_id IS NOT NULL AND selected_answer_version IS NOT NULL
        AND length(selected_answer_version)=8
        AND selected_answer_version<>x'0000000000000000')
);

CREATE VIEW game_choice_boundaries AS
    SELECT dataset_id, source, game_id, position, selected_target_id, selected_answer_version
    FROM game_choice_resolutions
    UNION ALL
    SELECT restored.dataset_id, restored.source, restored.game_id, restored.position,
           restored.selected_target_id, restored.selected_answer_version
    FROM restored_game_choices AS restored
    WHERE NOT EXISTS (
        SELECT 1 FROM game_choice_resolutions AS native
        WHERE (native.dataset_id,native.source,native.game_id,native.position)
            = (restored.dataset_id,restored.source,restored.game_id,restored.position));
