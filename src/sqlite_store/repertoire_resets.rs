//! Delete Study records in foreign-key order under one write lock.

use super::{database_error, row_values};
use crate::{Receipt, RepertoireReset, StoreError, store_error::invalid};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tempo_learning::LearningState;
use tempo_types::{DatasetId, EventId, TargetId};

pub(super) fn apply(
    connection: &mut Connection,
    dataset: &DatasetId,
    repertoire: &str,
    command_id: &str,
    at_ms: u64,
    fresh: &LearningState,
) -> Result<RepertoireReset, StoreError> {
    let _diagnostic = crate::diagnostics::Operation::new("sqlite.repertoire_resets.apply");
    let tx = crate::diagnostics::timed("sqlite.begin", || {
        connection.transaction_with_behavior(TransactionBehavior::Immediate)
    })
    .map_err(database_error)?;
    let (active_dataset, revision): (String, Vec<u8>) = tx
        .query_row(
            "SELECT dataset_id, revision FROM store_state WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(database_error)?;
    if active_dataset != dataset.0 {
        return Ok(RepertoireReset::StaleDataset);
    }
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT dataset_id, repertoire_id FROM repertoire_resets WHERE command_id=?1",
            [command_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    if let Some((previous_dataset, previous_repertoire)) = previous {
        return Ok(
            if previous_dataset == dataset.0 && previous_repertoire == repertoire {
                RepertoireReset::Replayed
            } else {
                RepertoireReset::CommandConflict
            },
        );
    }

    let targets: Vec<TargetId> = row_values::read_selected(
        &tx,
        "SELECT target_id FROM current_content WHERE dataset_id=?1 AND repertoire=?2",
        params![dataset.0, repertoire],
        |row| Ok(TargetId(row.get(0).map_err(database_error)?)),
    )?;
    if targets.is_empty() {
        return Ok(RepertoireReset::UnknownRepertoire);
    }
    let next_revision = row_values::decode_counter(revision)?
        .checked_add(1)
        .ok_or_else(|| invalid("store revision exhausted"))?;
    let at_ms = i64::try_from(at_ms).map_err(|_| invalid("reset time is out of range"))?;
    let state = row_values::encode(fresh)?;
    super::repertoire_runs::clear_repertoire(&tx, dataset, repertoire)?;

    // Each target remains in current_content even when its lines retire.
    for target in &targets {
        let key = params![dataset.0, target.0];
        tx.execute(
            "UPDATE store_state SET active_attempt_id=NULL
             WHERE singleton=1 AND active_attempt_id IN
               (SELECT id FROM attempts WHERE dataset_id=?1 AND target_id=?2)",
            key,
        )
        .map_err(database_error)?;
        tx.execute(
            "DELETE FROM receipts WHERE attempt_id IN
               (SELECT id FROM attempts WHERE dataset_id=?1 AND target_id=?2)",
            key,
        )
        .map_err(database_error)?;
        let external: Vec<(String, String)> = row_values::read_selected(
            &tx,
            "SELECT provider, event_id FROM assessments
             WHERE dataset_id=?1 AND target_id=?2 AND attempt_id IS NULL",
            key,
            |row| {
                Ok((
                    row.get(0).map_err(database_error)?,
                    row.get(1).map_err(database_error)?,
                ))
            },
        )?;
        for (provider, event) in external {
            let receipt = Receipt::external_id(dataset, &provider, &EventId(event));
            tx.execute("DELETE FROM receipts WHERE id=?1", [receipt.0])
                .map_err(database_error)?;
        }
        tx.execute(
            "DELETE FROM assessments WHERE dataset_id=?1 AND target_id=?2",
            key,
        )
        .map_err(database_error)?;
        tx.execute(
            "DELETE FROM attempts WHERE dataset_id=?1 AND target_id=?2",
            key,
        )
        .map_err(database_error)?;
        tx.execute(
            "UPDATE learning SET state=?2 WHERE target_id=?1",
            params![target.0, state],
        )
        .map_err(database_error)?;
    }
    tx.execute(
        "UPDATE store_state SET revision=?1 WHERE singleton=1",
        [next_revision.to_be_bytes().as_slice()],
    )
    .map_err(database_error)?;
    tx.execute(
        "INSERT INTO repertoire_resets VALUES (?1,?2,?3,?4)",
        params![command_id, dataset.0, repertoire, at_ms],
    )
    .map_err(database_error)?;
    crate::diagnostics::timed("sqlite.commit", || tx.commit()).map_err(database_error)?;
    Ok(RepertoireReset::Applied(targets))
}
