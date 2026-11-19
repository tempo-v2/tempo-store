//! Native choice provenance and the narrow decoder for pre-attribution databases.
use super::{database_error, row_values};
use crate::store_error::{conflict, invalid};
use crate::{
    ChoiceAttribution, ComparedGame, GameChoices, GameFailureKey, GameSummary, ResolvedGameChoice,
    StoreError,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use std::collections::BTreeSet;
use tempo_chess::{ContentItem, ContentKind};
use tempo_types::{ContentVersion, DatasetId, TargetId};

#[derive(Debug, Default)]
pub struct ChoiceMigrationReport {
    pub recovered: usize,
    pub unattributed: usize,
    pub conflicting: usize,
}

pub(super) fn attribution(
    row: &Row<'_>,
    column: usize,
) -> Result<Option<ChoiceAttribution>, StoreError> {
    let target: Option<String> = row.get(column).map_err(database_error)?;
    let version: Option<Vec<u8>> = row.get(column + 1).map_err(database_error)?;
    match (target, version) {
        (Some(target), Some(version)) => Ok(Some(ChoiceAttribution {
            target: TargetId(target),
            answer_version: ContentVersion(row_values::decode_counter(version)?),
        })),
        (None, None) => Ok(None),
        _ => Err(invalid("incomplete selected answer provenance")),
    }
}

pub(super) fn read(
    db: &Connection,
    dataset: &DatasetId,
    source: &str,
    game: &str,
) -> Result<GameChoices, StoreError> {
    row_values::read_selected(
        db,
        "SELECT position,selected_target_id,selected_answer_version FROM game_choice_boundaries
         WHERE dataset_id=?1 AND source=?2 AND game_id=?3",
        params![dataset.0, source, game],
        |row| Ok((row.get(0).map_err(database_error)?, attribution(row, 1)?)),
    )
}

pub(super) fn insert_restored(
    tx: &Transaction<'_>,
    dataset: &str,
    choice: &ResolvedGameChoice,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO restored_game_choices
          (dataset_id,source,game_id,position,selected_target_id,selected_answer_version)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            dataset,
            choice.source,
            choice.game_id,
            choice.position,
            choice.attribution.as_ref().map(|choice| &choice.target.0),
            choice.attribution.as_ref().map(|choice| choice
                .answer_version
                .0
                .to_be_bytes()
                .to_vec()),
        ],
    )
    .map_err(database_error)?;
    Ok(())
}

pub(super) fn attribution_matches(
    item: &ContentItem,
    position: &str,
    version: ContentVersion,
) -> bool {
    version.0 > 0
        && version.0 <= item.version.0
        && matches!(&item.definition.kind, ContentKind::Repertoire { position: actual, .. } if actual.0 == position)
}

pub(super) fn failure_attribution(key: &GameFailureKey) -> ChoiceAttribution {
    ChoiceAttribution {
        target: key.target.clone(),
        answer_version: key.version,
    }
}

pub(super) fn validate_new(
    tx: &Transaction<'_>,
    previous: &crate::Snapshot,
    game: &ComparedGame,
) -> Result<(), StoreError> {
    let Some(choice) = &game.resolved_choice else {
        return Ok(());
    };
    let selected = &choice.attribution;
    let parent = parent_summary(
        tx,
        &previous.dataset.0,
        &choice.comparison_id,
        &game.game.source,
        &game.game.id,
    )?;
    if !game.relevance.as_ref().is_some_and(|evidence| {
        !evidence.prior_choices.contains_key(&choice.position)
            && evidence.encounters.get(&selected.target) == Some(&(selected.answer_version, true))
    }) || game.summary.source != game.game.source
        || game.summary.game_id != game.game.id
        || !parent.is_some_and(|parent| {
            parent.learner == game.summary.learner
                && saved_candidate(&parent, choice.ply, &choice.position, selected)
        })
        || !previous
            .catalog
            .items
            .get(&selected.target)
            .is_some_and(|item| {
                item.active()
                    && item.version == selected.answer_version
                    && attribution_matches(item, &choice.position, selected.answer_version)
            })
        || !game.observations.iter().any(|observation| {
            observation.ply == choice.ply
                && observation.target == selected.target
                && observation.version == selected.answer_version
                && observation.played != observation.prescribed
                && previous.catalog.items[&selected.target]
                    .definition
                    .decisions[0]
                    .expected
                    .uci
                    == observation.prescribed
        })
        || game.failures.iter().any(|failure| {
            failure.target != selected.target || failure.content_version != selected.answer_version
        })
    {
        return Err(invalid(
            "game choice does not match the saved candidate and observation",
        ));
    }
    Ok(())
}

pub(super) fn ensure_current(
    db: &Connection,
    dataset: &DatasetId,
    source: &str,
    game: &str,
    read_set: &GameChoices,
) -> Result<(), StoreError> {
    if read(db, dataset, source, game)? != *read_set {
        return Err(conflict("accepted game choices changed during comparison"));
    }
    Ok(())
}

fn parent_summary(
    db: &Connection,
    dataset: &str,
    comparison: &str,
    source: &str,
    game: &str,
) -> Result<Option<GameSummary>, StoreError> {
    let summary: Option<String> = db
        .query_row(
            "SELECT summary FROM game_comparisons
         WHERE dataset_id=?1 AND id=?2 AND source=?3 AND game_id=?4",
            params![dataset, comparison, source, game],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    summary.as_deref().map(row_values::decode).transpose()
}

fn saved_candidate(
    parent: &GameSummary,
    ply: usize,
    position: &str,
    selected: &ChoiceAttribution,
) -> bool {
    parent.choices.iter().any(|choice| {
        choice.ply == ply
            && choice.position == position
            && choice.candidates.iter().any(|candidate| {
                candidate.target_id == selected.target
                    && candidate.version == selected.answer_version
            })
    })
}

fn stored_item(
    db: &Connection,
    dataset: &str,
    choice: &ChoiceAttribution,
    native: bool,
) -> Result<Option<ContentItem>, StoreError> {
    let version = choice.answer_version.0.to_be_bytes();
    let item: Option<String> = db
        .query_row(
            "SELECT item FROM content_history WHERE dataset_id=?1 AND target_id=?2 AND version=?3",
            params![dataset, choice.target.0, version.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    let item = if item.is_none() && !native {
        db.query_row(
            "SELECT history.item FROM current_content AS current
             JOIN content_history AS history USING(dataset_id,target_id,version)
             WHERE current.dataset_id=?1 AND current.target_id=?2",
            params![dataset, choice.target.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?
    } else {
        item
    };
    item.as_deref().map(row_values::decode).transpose()
}

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<ChoiceMigrationReport, StoreError> {
    let closures: Vec<(String, String, String, String)> = row_values::read_rows(
        tx,
        "SELECT dataset_id,source,game_id,position FROM game_choice_boundaries
         UNION SELECT dataset_id,source,game_id,chosen_position FROM game_failures
               WHERE chosen_position IS NOT NULL",
        |row| {
            Ok((
                row.get(0).map_err(database_error)?,
                row.get(1).map_err(database_error)?,
                row.get(2).map_err(database_error)?,
                row.get(3).map_err(database_error)?,
            ))
        },
    )?;
    let mut report = ChoiceMigrationReport::default();
    for (dataset, source, game, position) in closures {
        let native: Option<(String, usize)> = tx
            .query_row(
                "SELECT comparison_id,ply FROM game_choice_resolutions
             WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND position=?4",
                params![dataset, source, game, position],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        let parent = native
            .as_ref()
            .map(|(comparison, _)| parent_summary(tx, &dataset, comparison, &source, &game))
            .transpose()?
            .flatten();
        let mut proof: BTreeSet<_> = row_values::read_selected(
            tx,
            "SELECT target_id,version FROM game_failures
             WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND chosen_position=?4",
            params![dataset, source, game, position],
            |row| {
                Ok(ChoiceAttribution {
                    target: TargetId(row.get(0).map_err(database_error)?),
                    answer_version: ContentVersion(row_values::decode_counter(
                        row.get(1).map_err(database_error)?,
                    )?),
                })
            },
        )?;
        let mut contradictory = proof.len() > 1;
        for selected in std::mem::take(&mut proof) {
            let context = match &native {
                Some((_, ply)) => parent
                    .as_ref()
                    .is_some_and(|parent| saved_candidate(parent, *ply, &position, &selected)),
                None => true,
            };
            if context
                && stored_item(tx, &dataset, &selected, native.is_some())?.is_some_and(|item| {
                    attribution_matches(&item, &position, selected.answer_version)
                })
            {
                proof.insert(selected);
            } else {
                contradictory = true;
            }
        }
        if let Some((comparison, ply)) = &native
            && let Some(parent) = &parent
            // Schema 10's game-relevance wrapper carries per-book adherence.
            // Older aliases lack this field and cannot establish the wrapped child ID.
            && !parent.repertoires.is_empty()
            && let Some(choice) = parent.choices.iter().find(|choice| choice.ply == *ply && choice.position == position)
        {
            for candidate in &choice.candidates {
                let selected = ChoiceAttribution {
                    target: candidate.target_id.clone(),
                    answer_version: candidate.version,
                };
                let identity = serde_json::json!([comparison, ply, selected.target]);
                let child = blake3::hash(identity.to_string().as_bytes())
                    .to_hex()
                    .to_string();
                let observed: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM game_comparisons AS child
                     JOIN game_observations AS observed
                       ON (observed.dataset_id,observed.comparison_id)=(child.dataset_id,child.id)
                     WHERE child.dataset_id=?1 AND child.source=?2 AND child.game_id=?3
                       AND child.id=?4 AND json_extract(child.summary,'$.learner')=?5
                       AND observed.ply=?6 AND observed.target_id=?7 AND observed.version=?8
                       AND observed.played<>observed.prescribed)",
                        params![
                            dataset,
                            source,
                            game,
                            child,
                            parent.learner,
                            ply,
                            selected.target.0,
                            selected.answer_version.0.to_be_bytes().as_slice()
                        ],
                        |row| row.get(0),
                    )
                    .map_err(database_error)?;
                if observed {
                    if stored_item(tx, &dataset, &selected, true)?.is_some_and(|item| {
                        attribution_matches(&item, &position, selected.answer_version)
                    }) {
                        proof.insert(selected);
                    } else {
                        contradictory = true;
                    }
                }
            }
        }
        let attribution = if contradictory || proof.len() > 1 {
            report.conflicting += 1;
            None
        } else if let Some(selected) = proof.pop_first() {
            report.recovered += 1;
            Some(selected)
        } else {
            report.unattributed += 1;
            None
        };
        if native.is_some() {
            if let Some(selected) = attribution {
                tx.execute(
                    "UPDATE game_choice_resolutions SET selected_target_id=?5, selected_answer_version=?6
                     WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND position=?4",
                    params![dataset, source, game, position, selected.target.0,
                        selected.answer_version.0.to_be_bytes().as_slice()],
                ).map_err(database_error)?;
            }
        } else {
            tx.execute(
                "DELETE FROM restored_game_choices WHERE dataset_id=?1 AND source=?2 AND game_id=?3 AND position=?4",
                params![dataset, source, game, position],
            ).map_err(database_error)?;
            insert_restored(
                tx,
                &dataset,
                &ResolvedGameChoice {
                    source,
                    game_id: game,
                    position,
                    attribution,
                },
            )?;
        }
    }
    Ok(report)
}
