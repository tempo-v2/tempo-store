//! Apply one repertoire's participation change and fence outstanding work atomically.

use super::{database_error, load_study, row_values, save_snapshot::save_snapshot};
use crate::{
    Commit, RepertoireActivity, RepertoireActivityResult, StoreError, StudyScope, Write,
    store_error::invalid,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;
use tempo_chess::ContentKind;
use tempo_types::DatasetId;

pub(super) fn read(db: &Connection, dataset: &str) -> Result<BTreeSet<String>, StoreError> {
    row_values::read_selected(
        db,
        "SELECT repertoire_id FROM repertoire_suspensions WHERE dataset_id=?1",
        [dataset],
        |row| row.get(0).map_err(database_error),
    )
}

pub(super) fn apply(
    db: &mut Connection,
    dataset: &DatasetId,
    repertoire: &str,
    command_id: &str,
    activity: RepertoireActivity,
) -> Result<RepertoireActivityResult, StoreError> {
    use RepertoireActivityResult::{Applied, CommandConflict, StaleDataset, UnknownRepertoire};

    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let state = load_study::load_study(&tx, StudyScope::default())?;
    if &state.dataset != dataset {
        return Ok(StaleDataset);
    }
    // This namespace is separate from signed PGN preview tokens in the same journal.
    let key = format!("repertoire-activity:{command_id}");
    let acknowledgment = row_values::encode(&(dataset, repertoire, activity))?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT reply FROM content_receipts WHERE request=?1",
            [&key],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    if let Some(previous) = previous {
        return Ok(if previous == acknowledgment {
            Applied
        } else {
            CommandConflict
        });
    }
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM current_content WHERE dataset_id=?1 AND repertoire=?2)",
            params![dataset.0, repertoire],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if !exists {
        return Ok(UnknownRepertoire);
    }
    let change = match activity {
        RepertoireActivity::Suspended => tx.execute(
            "INSERT INTO repertoire_suspensions VALUES (?1,?2) ON CONFLICT DO NOTHING",
            params![dataset.0, repertoire],
        ),
        RepertoireActivity::Active => tx.execute(
            "DELETE FROM repertoire_suspensions WHERE dataset_id=?1 AND repertoire_id=?2",
            params![dataset.0, repertoire],
        ),
    }
    .map_err(database_error)?;
    if change != 0 {
        let mut writes = Vec::new();
        if activity == RepertoireActivity::Suspended
            && let Some(id) = &state.active_attempt
        {
            let mut record = state.attempts[id].clone();
            let (item, _) = state
                .target(&record.attempt.target)?
                .ok_or_else(|| invalid("active repertoire target is missing"))?;
            if matches!(&item.definition.kind, ContentKind::Repertoire { repertoire: id, .. } if id == repertoire)
            {
                record.attempt = tempo_study::abandon(&record.attempt, record.attempt.revision)
                    .map_err(|error| invalid(&error.to_string()))?;
                writes.extend([Write::Progress(record), Write::Active(None)]);
            }
        }
        let commit = Commit {
            expected_dataset: dataset.clone(),
            expected_revision: state.revision,
            writes,
        };
        let previous = load_study::staging_image(state);
        let next = super::stage_commit(&previous, commit)?;
        save_snapshot(&tx, Some(&previous), &next)?;
        tx.execute(
            "UPDATE store_state SET content_revision=content_revision+1 WHERE singleton=1",
            [],
        )
        .map_err(database_error)?;
    }
    tx.execute(
        "INSERT INTO content_receipts VALUES (?1,?2)",
        params![key, acknowledgment],
    )
    .map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(Applied)
}
