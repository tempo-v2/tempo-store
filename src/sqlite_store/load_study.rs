//! Read live study facts and indexed rows for the current command.
use super::database_error;
use super::load_snapshot::load_lines;
use super::row_values::{decode, decode_counter, read_selected, text_value};
use crate::{Receipt, Snapshot, StoreError, StoredAttempt, StudyScope};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::Catalog;
use tempo_types::{AttemptId, DatasetId, Revision, TargetId};

pub(super) fn load_study(
    tx: &Transaction<'_>,
    scope: StudyScope<'_>,
) -> Result<Snapshot, StoreError> {
    let (dataset, revision, active, policy): (String, Vec<u8>, Option<String>, String) = tx.query_row(
        "SELECT dataset_id, revision, active_attempt_id, policy FROM store_state WHERE singleton=1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).map_err(database_error)?;
    let attempts: BTreeMap<AttemptId, StoredAttempt> = read_selected(
        tx,
        "SELECT id, record FROM attempts WHERE id IN (?1, ?2)",
        params![active, scope.attempt.map(|id| &id.0)],
        |row| {
            Ok((
                AttemptId(row.get(0).map_err(database_error)?),
                text_value(row, 1)?,
            ))
        },
    )?;
    let assessments = if scope.attempt.is_some() {
        read_selected(
            tx,
            "SELECT record FROM assessments WHERE dataset_id=?1 AND attempt_id IN (?2, ?3)",
            params![dataset, active, scope.attempt.map(|id| &id.0)],
            |row| text_value(row, 0),
        )?
    } else if let Some(active) = &active {
        read_selected(
            tx,
            "SELECT record FROM assessments WHERE dataset_id=?1 AND attempt_id=?2",
            params![dataset, active],
            |row| text_value(row, 0),
        )?
    } else {
        Vec::new()
    };

    let mut targets = attempts
        .values()
        .map(|row| &row.attempt.target.0)
        .collect::<BTreeSet<_>>();
    targets.extend(scope.target.map(|target| &target.0));
    let (items, learning) = if scope.catalog {
        (
            read_selected(
                tx,
                "SELECT target_id, item FROM current_content",
                [],
                |row| {
                    Ok((
                        TargetId(row.get(0).map_err(database_error)?),
                        text_value(row, 1)?,
                    ))
                },
            )?,
            read_selected(tx, "SELECT target_id, state FROM learning", [], |row| {
                Ok((
                    TargetId(row.get(0).map_err(database_error)?),
                    text_value(row, 1)?,
                ))
            })?,
        )
    } else {
        let mut items = BTreeMap::new();
        let mut learning = BTreeMap::new();
        let mut statement = tx.prepare_cached(
            "SELECT item, state FROM current_content JOIN learning USING (target_id) WHERE target_id=?1"
        ).map_err(database_error)?;
        for target in targets {
            let row: Option<(String, String)> = statement
                .query_row([target], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(database_error)?;
            if let Some((item, state)) = row {
                items.insert(TargetId(target.clone()), decode(&item)?);
                learning.insert(TargetId(target.clone()), decode(&state)?);
            }
        }
        (items, learning)
    };
    let receipts = match scope.submission {
        Some(id) => read_selected(
            tx,
            "SELECT command, result FROM receipts WHERE id=?1",
            [&id.0],
            |row| {
                Ok((
                    id.clone(),
                    Receipt {
                        id: id.clone(),
                        command: row.get(0).map_err(database_error)?,
                        result: text_value(row, 1)?,
                    },
                ))
            },
        )?,
        None => BTreeMap::new(),
    };
    Ok(Snapshot {
        dataset: DatasetId(dataset.clone()),
        revision: Revision(decode_counter(revision)?),
        active_attempt: active.map(AttemptId),
        policy: decode(&policy)?,
        known_datasets: BTreeSet::from([DatasetId(dataset)]),
        catalog: Catalog {
            items,
            lines: if scope.catalog {
                load_lines(tx)?
            } else {
                vec![]
            },
        },
        learning,
        attempts,
        assessments,
        receipts,
        content_history: vec![],
    })
}
