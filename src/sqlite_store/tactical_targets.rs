//! Indexed current-content basis and addressed writes for tactical acceptance.

use super::{
    database_error,
    row_values::{decode, encode, read_selected, text_value},
};
use crate::{
    StoreError, TacticalTargetChange,
    store_error::{conflict, invalid},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tempo_chess::{Catalog, ContentItem, TacticLookup};
use tempo_learning::LearningState;
use tempo_types::{ContentVersion, DatasetId, Revision, TargetId};

const MATCHES: &str = "WITH matched(target_id) AS (
         SELECT target_id FROM current_content WHERE target_id=?1
         UNION
         SELECT target_id FROM tactical_identities WHERE identity=?2
         UNION
         SELECT target_id FROM tactical_sources WHERE token IN (SELECT value FROM json_each(?3))
     )
     SELECT c.item FROM matched JOIN current_content c USING(target_id) WHERE c.dataset_id=?4";

pub(super) fn matches(
    tx: &Connection,
    dataset: &DatasetId,
    lookup: &TacticLookup,
) -> Result<Vec<ContentItem>, StoreError> {
    read_selected(
        tx,
        MATCHES,
        params![
            lookup.target.0,
            encode(&lookup.identity)?,
            encode(&lookup.source_tokens)?,
            dataset.0
        ],
        |row| text_value(row, 0),
    )
}

pub(super) fn learning(
    connection: &Connection,
    target: &TargetId,
) -> Result<Option<LearningState>, StoreError> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT state FROM learning WHERE target_id=?1",
            [&target.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    encoded.as_deref().map(decode).transpose()
}

fn current(
    connection: &Connection,
    dataset: &DatasetId,
    target: &TargetId,
) -> Result<Option<ContentItem>, StoreError> {
    let encoded: Option<String> = connection
        .query_row(
            "SELECT item FROM current_content WHERE target_id=?1 AND dataset_id=?2",
            params![target.0, dataset.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    encoded.as_deref().map(decode).transpose()
}

pub(super) fn revision(connection: &Connection) -> Result<(DatasetId, Revision), StoreError> {
    let (dataset, revision): (String, Vec<u8>) = connection
        .query_row(
            "SELECT dataset_id,revision FROM store_state WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(database_error)?;
    Ok((
        DatasetId(dataset),
        Revision(super::row_values::decode_counter(revision)?),
    ))
}

/// Rebuild the lookup on the schema migration; the ordinary writers maintain it.
pub(super) fn backfill(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let mut statement = tx
        .prepare("SELECT item FROM current_content WHERE repertoire IS NULL")
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        index_item(tx, &text_value(row, 0)?)?;
    }
    Ok(())
}

pub(super) fn index_item(tx: &Transaction<'_>, item: &ContentItem) -> Result<(), StoreError> {
    let Some(lookup) = TacticLookup::from_item(item) else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO tactical_identities(target_id,identity) VALUES (?1,?2)",
        params![item.target.0, encode(&lookup.identity)?],
    )
    .map_err(database_error)?;
    let mut sources = tx
        .prepare_cached("INSERT INTO tactical_sources(token,target_id) VALUES (?1,?2) ON CONFLICT(token,target_id) DO NOTHING")
        .map_err(database_error)?;
    for token in lookup.source_tokens {
        sources
            .execute(params![token, item.target.0])
            .map_err(database_error)?;
    }
    Ok(())
}

/// Only the target identities tied to new request effects may reach this writer.
pub(super) fn apply_change(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    target: &TargetId,
    change: &TacticalTargetChange,
) -> Result<bool, StoreError> {
    let previous = current(tx, dataset, target)?;
    if let Some(item) = &change.content {
        if item.target != *target || !item.active() || item.definition.is_repertoire() {
            return Err(invalid("tactical content effect needs an active tactic"));
        }
        Catalog {
            items: [(target.clone(), item.clone())].into(),
            lines: Vec::new(),
        }
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
        if let Some(previous) = &previous {
            if item.version != previous.version || item.definition != previous.definition {
                return Err(conflict("tactical acceptance cannot replace a definition"));
            }
            if item != previous {
                tx.execute(
                    "UPDATE current_content SET item=?1 WHERE target_id=?2 AND dataset_id=?3",
                    params![encode(item)?, target.0, dataset.0],
                )
                .map_err(database_error)?;
                tx.execute(
                    "DELETE FROM tactical_identities WHERE target_id=?1",
                    [&target.0],
                )
                .map_err(database_error)?;
                index_item(tx, item)?;
            }
        } else {
            if item.version != ContentVersion(1) {
                return Err(invalid("new tactical content requires an initial version"));
            }
            let payload = encode(item)?;
            tx.execute(
                "INSERT INTO content_history(ordinal,dataset_id,target_id,version,item)
                 VALUES((SELECT coalesce(max(ordinal)+1,0) FROM content_history),?1,?2,?3,?4)",
                params![
                    dataset.0,
                    target.0,
                    item.version.0.to_be_bytes().as_slice(),
                    payload
                ],
            )
            .map_err(database_error)?;
            tx.execute(
                "INSERT INTO current_content VALUES(?1,?2,?3,NULL,NULL,?4)",
                params![
                    target.0,
                    dataset.0,
                    item.version.0.to_be_bytes().as_slice(),
                    payload
                ],
            )
            .map_err(database_error)?;
            index_item(tx, item)?;
        }
    }
    if let Some(introduction) = &change.introduction {
        if !introduction.introduced || learning(tx, target)?.is_some_and(|prior| prior.introduced) {
            return Err(invalid(
                "tactical acceptance may only introduce an unintroduced target",
            ));
        }
        tx.execute(
            "INSERT INTO learning VALUES(?1,?2)
             ON CONFLICT(target_id) DO UPDATE SET state=excluded.state",
            params![target.0, encode(introduction)?],
        )
        .map_err(database_error)?;
    }
    Ok(change
        .content
        .as_ref()
        .is_some_and(|item| previous.as_ref() != Some(item)))
}

pub(super) fn require_target(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    target: &TargetId,
    version: ContentVersion,
) -> Result<(), StoreError> {
    if !current(tx, dataset, target)?.is_some_and(|item| {
        item.active() && !item.definition.is_repertoire() && item.version == version
    }) {
        return Err(invalid(
            "training request needs an active current-version tactic",
        ));
    }
    if !learning(tx, target)?.is_some_and(|state| state.introduced) {
        return Err(invalid(
            "training request requires Learning's introduction transition",
        ));
    }
    Ok(())
}
