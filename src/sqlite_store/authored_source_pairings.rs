//! Record the source verified against the retained authored catalog.
use super::{database_error, row_values};
use crate::{StoreError, model_lines, model_targets};
use rusqlite::{Transaction, params};
use tempo_chess::Catalog;
use tempo_types::DatasetId;

pub(super) const SCHEMA: &str = include_str!("authored_source_pairings.sql");

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<(), StoreError> {
    tx.execute_batch(SCHEMA).map_err(database_error)?;
    // Before schema 27, verified inputs recorded the successful comparison
    // between authored PGN and the accepted compiled repertoire.
    tx.execute(
        "INSERT OR IGNORE INTO authored_source_pairings(dataset_id,repertoire_id,source_id)
         SELECT d.dataset_id,d.repertoire_id,d.source_id
           FROM repertoire_documents d JOIN game_model_inputs i USING(dataset_id,repertoire_id)
          WHERE i.state='verified' AND length(i.input_key)>0",
        [],
    ).map_err(database_error)?;
    Ok(())
}

pub(super) fn record(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    repertoire: &str,
    source: &str,
) -> Result<bool, StoreError> {
    let changed = tx.execute(
        "INSERT INTO authored_source_pairings(dataset_id,repertoire_id,source_id) VALUES(?1,?2,?3)
         ON CONFLICT(dataset_id,repertoire_id) DO UPDATE SET source_id=excluded.source_id
           WHERE source_id<>excluded.source_id",
        params![dataset.0, repertoire, source],
    ).map_err(database_error)?;
    Ok(changed != 0)
}

pub(super) fn invalidate_changed(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    previous: &Catalog,
    next: &Catalog,
) -> Result<(), StoreError> {
    let repertoires: Vec<String> = row_values::read_selected(
        tx,
        "SELECT repertoire_id FROM authored_source_pairings WHERE dataset_id=?1",
        [&dataset.0],
        |row| row.get(0).map_err(database_error),
    )?;
    for id in repertoires {
        if model_targets(previous, &id) != model_targets(next, &id)
            || model_lines(previous, &id) != model_lines(next, &id)
        {
            tx.execute(
                "DELETE FROM authored_source_pairings WHERE dataset_id=?1 AND repertoire_id=?2",
                params![dataset.0, id],
            ).map_err(database_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{SqliteStore, Store};
    use tempo_chess::demo_catalog;
    use tempo_learning::{DailyLearning, LearningPolicy};

    #[test]
    fn schema_27_admits_only_previous_verified_exact_pairings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pairings.sqlite");
        let store = SqliteStore::create(
            &path, "repertoires".into(), demo_catalog(), DailyLearning::default().settings(),
        ).unwrap();
        store.connection.execute_batch(
            "DROP TABLE authored_source_pairings;
             INSERT INTO content_sources(id,pgn) VALUES('source-one','1. e4 *'),('source-two','1. d4 *');
             INSERT INTO repertoire_documents VALUES
                 ('repertoires','r1','source-one'),('repertoires','unverified','source-two');
             INSERT INTO game_model_inputs(dataset_id,repertoire_id,state,input_key) VALUES
                 ('repertoires','r1','verified','old-witness'),('repertoires','unverified','needs_bootstrap',NULL);
             PRAGMA user_version=26;",
        ).unwrap();
        drop(store);

        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.repertoire_document("r1").unwrap().pgn.as_deref(), Some("1. e4 *"));
        assert_eq!(store.repertoire_document("unverified").unwrap().pgn, None);
        store.connection.execute(
            "UPDATE game_model_inputs SET state='unavailable', input_key=NULL WHERE repertoire_id='r1'", [],
        ).unwrap();
        assert_eq!(store.repertoire_document("r1").unwrap().pgn.as_deref(), Some("1. e4 *"));
    }
}
