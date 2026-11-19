//! Durable game facts and the bounded ranking projection shared by Study and Games.
use super::{database_error, game_choices, row_values};
use crate::store_error::conflict;
use crate::{
    ComparedGame, ComparedRelevance, EncounterKey, EncounterRepair, GameEncounters,
    GameTargetRelevance, GroupedOpponentGap, StoreError,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::BTreeMap;
use tempo_chess::{OpponentGap, PositionKey};
use tempo_types::{ContentVersion, DatasetId, Revision, TargetId};

pub(super) fn save_facts(
    tx: &Transaction<'_>,
    dataset: &str,
    compared: &ComparedGame,
    evidence: &ComparedRelevance,
) -> Result<(), StoreError> {
    let game = &compared.game;
    let learner = &compared.summary.learner;
    if let Some(occurred) = game
        .start_ms
        .or(game.end_ms)
        .or(game.played_date_ms)
        .filter(|occurred| *occurred <= game.fetched_ms)
    {
        tx.execute(
            "INSERT INTO game_relevance_inputs VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(dataset_id,source,game_id,learner) DO UPDATE
             SET occurred_ms=excluded.occurred_ms",
            params![dataset, game.source, game.id, learner, occurred],
        )
        .map_err(database_error)?;
        replace_encounters(
            tx,
            dataset,
            &game.source,
            &game.id,
            learner,
            &evidence.encounters,
        )?;
    }
    tx.execute(
        "DELETE FROM game_opponent_gaps WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND learner=?4",
        params![dataset, game.source, game.id, learner],
    ).map_err(database_error)?;
    let mut statement = tx
        .prepare_cached("INSERT INTO game_opponent_gaps VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
        .map_err(database_error)?;
    for (gap, revision) in &evidence.gaps {
        statement
            .execute(params![
                dataset,
                game.source,
                game.id,
                learner,
                gap.repertoire_id,
                gap.position.0,
                gap.actual_move,
                revision
            ])
            .map_err(database_error)?;
    }
    Ok(())
}

fn replace_encounters(
    tx: &Transaction<'_>,
    dataset: &str,
    source: &str,
    game: &str,
    learner: &str,
    encounters: &GameEncounters,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM game_encounters WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND learner=?4",
        params![dataset, source, game, learner],
    ).map_err(database_error)?;
    let mut statement = tx
        .prepare_cached("INSERT INTO game_encounters VALUES (?1,?2,?3,?4,?5,?6,?7)")
        .map_err(database_error)?;
    for (target, (version, missed)) in encounters {
        statement
            .execute(params![
                dataset,
                source,
                game,
                learner,
                target.0,
                version.0.to_be_bytes().as_slice(),
                missed
            ])
            .map_err(database_error)?;
    }
    Ok(())
}

pub(super) fn choice_encounter_keys(
    db: &Connection,
    dataset: &DatasetId,
    after: Option<&EncounterKey>,
) -> Result<Vec<EncounterKey>, StoreError> {
    let cursor = after.map(|key| {
        (
            key.source.as_str(),
            key.game_id.as_str(),
            key.learner.as_str(),
        )
    });
    let (source, game, learner) = cursor.unwrap_or(("", "", ""));
    row_values::read_selected(
        db,
        "SELECT source,game_id,learner FROM game_relevance_inputs AS inputs
         WHERE dataset_id=?1 AND (source,game_id,learner)>(?2,?3,?4)
           AND EXISTS (SELECT 1 FROM game_choice_boundaries AS choices
               WHERE (choices.dataset_id,choices.source,choices.game_id)
                 = (inputs.dataset_id,inputs.source,inputs.game_id))
         ORDER BY source,game_id,learner LIMIT 64",
        params![dataset.0, source, game, learner],
        |row| {
            Ok(EncounterKey {
                source: row.get(0).map_err(database_error)?,
                game_id: row.get(1).map_err(database_error)?,
                learner: row.get(2).map_err(database_error)?,
            })
        },
    )
}

pub(super) fn check_content_revision(
    db: &Connection,
    expected: Revision,
) -> Result<(), StoreError> {
    let revision: u64 = db
        .query_row(
            "SELECT content_revision FROM store_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if revision != expected.0 {
        return Err(conflict("content changed during game evidence projection"));
    }
    Ok(())
}

pub(super) fn repair_choice_encounters(
    db: &mut Connection,
    dataset: &DatasetId,
    content_revision: Revision,
    repairs: &[EncounterRepair],
) -> Result<usize, StoreError> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    check_dataset(&tx, dataset)?;
    check_content_revision(&tx, content_revision)?;
    let mut changed = 0;
    for repair in repairs {
        let key = &repair.key;
        let input = params![dataset.0, key.source, key.game_id, key.learner];
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM game_relevance_inputs
             WHERE (dataset_id,source,game_id,learner)=(?1,?2,?3,?4))",
                input,
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if !exists || repair.choices.is_empty() {
            return Err(conflict(
                "choice encounter input changed during reconciliation",
            ));
        }
        game_choices::ensure_current(&tx, dataset, &key.source, &key.game_id, &repair.choices)?;
        let old: GameEncounters = row_values::read_selected(
            &tx,
            "SELECT target_id,answer_version,missed FROM game_encounters
             WHERE (dataset_id,source,game_id,learner)=(?1,?2,?3,?4)",
            input,
            |row| {
                Ok((
                    TargetId(row.get(0).map_err(database_error)?),
                    (
                        ContentVersion(row_values::decode_counter(
                            row.get(1).map_err(database_error)?,
                        )?),
                        row.get(2).map_err(database_error)?,
                    ),
                ))
            },
        )?;
        if old != repair.encounters {
            replace_encounters(
                &tx,
                &dataset.0,
                &key.source,
                &key.game_id,
                &key.learner,
                &repair.encounters,
            )?;
            changed += 1;
        }
    }
    if changed > 0 {
        rebuild(&tx, &dataset.0)?;
    }
    tx.commit().map_err(database_error)?;
    Ok(changed)
}

pub(super) fn rebuild(tx: &Transaction<'_>, dataset: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM game_target_relevance WHERE dataset_id=?1",
        [dataset],
    )
    .map_err(database_error)?;
    tx.execute(
        "WITH recent AS MATERIALIZED (
           SELECT source,game_id,learner,max(occurred_ms) AS occurred FROM game_relevance_inputs AS game WHERE dataset_id=?1
           AND (NOT EXISTS (SELECT 1 FROM game_learner_accounts WHERE dataset_id=?1)
               OR EXISTS (SELECT 1 FROM game_learner_accounts AS configured
                   WHERE configured.dataset_id=?1 AND configured.learner=game.learner
                     AND (configured.source=game.source OR game.source='pgn')))
           GROUP BY source,game_id HAVING count(*)=1
           ORDER BY occurred DESC,source,game_id LIMIT 200
         ), by_version AS (
           SELECT target_id, hex(answer_version) AS version, count(*) AS exposures, sum(missed) AS misses
           FROM recent JOIN game_encounters USING(source,game_id,learner)
           WHERE dataset_id=?1 GROUP BY target_id,answer_version
         )
         INSERT INTO game_target_relevance
           SELECT ?1,target_id,sum(exposures),json_group_object(version,misses),(SELECT count(*) FROM recent)
           FROM by_version GROUP BY target_id",
        [dataset],
    ).map_err(database_error)?;
    Ok(())
}

pub(super) fn read(
    connection: &Connection,
    dataset: &DatasetId,
    model: &str,
) -> Result<BTreeMap<TargetId, GameTargetRelevance>, StoreError> {
    let mut query = connection.prepare_cached(
        "WITH ids AS (
           SELECT target_id FROM game_target_relevance WHERE dataset_id=?1
           UNION SELECT target_id FROM game_model_reach
                 WHERE dataset_id=?1 AND model_key=?2
                   AND content_revision=(SELECT content_revision FROM store_state WHERE singleton=1))
         SELECT ids.target_id,coalesce(exposure,0),
                coalesce(json_extract(misses_by_version,'$.'||hex(current_content.version)),0),
                coalesce(window_games,0),millionths
         FROM ids
         JOIN current_content ON current_content.target_id=ids.target_id AND current_content.dataset_id=?1
         LEFT JOIN game_target_relevance AS personal ON personal.target_id=ids.target_id AND personal.dataset_id=?1
         LEFT JOIN game_model_reach AS model ON model.target_id=ids.target_id AND model.dataset_id=?1
           AND model.model_key=?2 AND model.content_revision=(SELECT content_revision FROM store_state WHERE singleton=1)",
    ).map_err(database_error)?;
    query
        .query_map(params![dataset.0, model], |row| {
            Ok((
                TargetId(row.get(0)?),
                GameTargetRelevance {
                    exposure: row.get(1)?,
                    misses: row.get(2)?,
                    window_games: row.get(3)?,
                    modeled_millionths: row.get(4)?,
                },
            ))
        })
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)
}

pub(super) fn gaps(
    connection: &Connection,
    dataset: &DatasetId,
) -> Result<Vec<GroupedOpponentGap>, StoreError> {
    let mut statement = connection
        .prepare_cached(
            "WITH confirmed AS (
         SELECT DISTINCT gap.source,gap.game_id,gap.repertoire_id,position,actual_move
         FROM game_opponent_gaps AS gap
         WHERE gap.dataset_id=?1
           AND (NOT EXISTS (SELECT 1 FROM game_learner_accounts WHERE dataset_id=?1)
                OR EXISTS (SELECT 1 FROM game_learner_accounts AS configured
                   WHERE configured.dataset_id=?1 AND configured.learner=gap.learner
                     AND (configured.source=gap.source OR gap.source='pgn')))
         )
         SELECT repertoire_id,position,actual_move,count(*)
         FROM confirmed GROUP BY repertoire_id,position,actual_move
         ORDER BY count(*) DESC,repertoire_id,position,actual_move",
        )
        .map_err(database_error)?;
    statement
        .query_map([&dataset.0], |row| {
            Ok(GroupedOpponentGap {
                gap: OpponentGap {
                    repertoire_id: row.get(0)?,
                    position: PositionKey(row.get(1)?),
                    actual_move: row.get(2)?,
                },
                games: row.get(3)?,
            })
        })
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)
}

pub(super) fn decode_distribution(value: &str) -> Result<BTreeMap<String, f64>, StoreError> {
    row_values::decode(value)
}

pub(super) fn check_dataset(tx: &Transaction<'_>, dataset: &DatasetId) -> Result<(), StoreError> {
    let active: String = tx
        .query_row(
            "SELECT dataset_id FROM store_state WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if active != dataset.0 {
        return Err(crate::store_error::conflict("active dataset changed"));
    }
    Ok(())
}
