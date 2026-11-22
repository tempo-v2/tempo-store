//! Assemble a typed Store snapshot from all tables in one read transaction.

use super::database_error;
use super::row_values::{decode, decode_counter, read_rows, text_value};
use crate::{ContentHistory, Receipt, Snapshot, StoreError};
use rusqlite::Transaction;
use std::collections::BTreeMap;
use tempo_chess::{Catalog, RepertoireLine};
use tempo_types::{AttemptId, DatasetId, Revision, TargetId};

pub(super) fn load_snapshot(transaction: &Transaction<'_>) -> Result<Snapshot, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.load_snapshot.load_snapshot");
    let (dataset, revision, active, policy) = transaction
        .query_row(
            "SELECT dataset_id, revision, active_attempt_id, policy FROM store_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?)),
        )
        .map_err(database_error)?;

    Ok(Snapshot {
        // Retired run rows stay on disk and are outside the active snapshot.
        repertoire_run: None,
        dataset: DatasetId(dataset),
        revision: Revision(decode_counter(revision)?),
        active_attempt: active.map(AttemptId),
        policy: decode(&policy)?,
        known_datasets: read_rows(transaction, "SELECT id FROM datasets", |row| {
            row.get(0).map(DatasetId).map_err(database_error)
        })?,
        catalog: Catalog {
            items: read_rows(
                transaction,
                "SELECT target_id, item FROM current_content",
                |row| {
                    Ok((
                        TargetId(row.get(0).map_err(database_error)?),
                        text_value(row, 1)?,
                    ))
                },
            )?,
            lines: load_lines(transaction)?,
        },
        learning: read_rows(
            transaction,
            "SELECT target_id, state FROM learning",
            |row| {
                Ok((
                    TargetId(row.get(0).map_err(database_error)?),
                    text_value(row, 1)?,
                ))
            },
        )?,
        attempts: read_rows(transaction, "SELECT id, record FROM attempts", |row| {
            Ok((
                AttemptId(row.get(0).map_err(database_error)?),
                text_value(row, 1)?,
            ))
        })?,
        assessments: read_rows(
            transaction,
            "SELECT record FROM assessments ORDER BY ordinal",
            |row| text_value(row, 0),
        )?,
        receipts: read_rows(
            transaction,
            "SELECT id, command, result FROM receipts",
            |row| {
                let receipt = Receipt {
                    id: tempo_types::SubmissionId(row.get(0).map_err(database_error)?),
                    command: row.get(1).map_err(database_error)?,
                    result: text_value(row, 2)?,
                };
                Ok((receipt.id.clone(), receipt))
            },
        )?,
        content_history: read_rows(
            transaction,
            "SELECT dataset_id, item FROM content_history ORDER BY ordinal",
            |row| {
                Ok(ContentHistory {
                    dataset: DatasetId(row.get(0).map_err(database_error)?),
                    item: text_value(row, 1)?,
                })
            },
        )?,
    })
}

pub(super) fn load_lines(transaction: &Transaction<'_>) -> Result<Vec<RepertoireLine>, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.load_snapshot.load_lines");
    let mut lines: Vec<_> = read_rows(
        transaction,
        "SELECT id, repertoire FROM repertoire_lines ORDER BY ordinal",
        |row| {
            Ok(RepertoireLine {
                id: row.get(0).map_err(database_error)?,
                repertoire: row.get(1).map_err(database_error)?,
                targets: Vec::new(),
            })
        },
    )?;
    let occurrences: Vec<(String, TargetId)> = read_rows(
        transaction,
        "SELECT line_id, target_id FROM line_targets ORDER BY line_id, ordinal",
        |row| {
            Ok((
                row.get(0).map_err(database_error)?,
                TargetId(row.get(1).map_err(database_error)?),
            ))
        },
    )?;
    let mut targets = BTreeMap::<_, Vec<_>>::new();
    for (line, target) in occurrences {
        targets.entry(line).or_default().push(target);
    }
    for line in &mut lines {
        line.targets = targets.remove(&line.id).unwrap_or_default();
    }
    Ok(lines)
}
