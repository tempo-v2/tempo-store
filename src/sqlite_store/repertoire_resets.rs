//! Resolve a reset accepted by a previous release without writing Study state.

use super::database_error;
use crate::{PriorRepertoireReset, StoreError};
use rusqlite::{Connection, params};
use tempo_types::DatasetId;

pub(super) fn accepted(
    connection: &Connection,
    dataset: &DatasetId,
    repertoire: &str,
    command_id: &str,
) -> Result<PriorRepertoireReset, StoreError> {
    let (current, accepted_dataset, accepted_repertoire): (String, Option<String>, Option<String>) =
        connection
            .query_row(
                "SELECT s.dataset_id,r.dataset_id,r.repertoire_id
             FROM store_state s
             LEFT JOIN repertoire_resets r ON s.dataset_id=?1 AND r.command_id=?2
             WHERE s.singleton=1",
                params![dataset.0, command_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(database_error)?;
    if current != dataset.0 {
        return Ok(PriorRepertoireReset::StaleDataset);
    }
    Ok(
        match accepted_dataset
            .as_deref()
            .zip(accepted_repertoire.as_deref())
        {
            Some((saved_dataset, saved_repertoire))
                if saved_dataset == dataset.0 && saved_repertoire == repertoire =>
            {
                PriorRepertoireReset::Replayed
            }
            Some(_) => PriorRepertoireReset::CommandConflict,
            None => PriorRepertoireReset::NotAccepted,
        },
    )
}
