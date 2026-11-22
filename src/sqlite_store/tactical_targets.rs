//! Indexed current-content basis and addressed writes for tactical acceptance.

use super::{
    database_error,
    row_values::{decode, encode, read_selected, text_value},
};
use crate::{
    StoreError, TacticalTargetChange,
    diagnostics::Operation,
    store_error::{conflict, invalid},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tempo_chess::{Catalog, ContentItem, TacticLookup};
use tempo_learning::LearningState;
use tempo_types::{ContentVersion, DatasetId, Revision, TargetId};

pub(super) const MATCHES: &str = "WITH matched(target_id) AS (
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
    let _trace = Operation::new("sqlite.tactical_targets.matches");
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
    let _trace = Operation::new("sqlite.tactical_targets.learning");
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
                let _trace = Operation::new("sqlite.tactical_targets.update_content");
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
                release_independent_tactic(tx, dataset, item)?;
            }
        } else {
            let _trace = Operation::new("sqlite.tactical_targets.insert_content");
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
        let _trace = Operation::new("sqlite.tactical_targets.introduce_learning");
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

/// Outcome of publishing a verified continuation against its captured basis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TacticPayoffPublication {
    Published,
    Deferred,
    Obsolete,
    ConflictingDefinition,
}

/// Exact import deduplication can give a generated target independent
/// ownership. Its authored copy no longer depends on engine qualification.
pub(super) fn release_independent_tactic(
    tx: &Transaction<'_>,
    dataset: &DatasetId,
    item: &ContentItem,
) -> Result<(), StoreError> {
    let gated: bool = tx.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM tactical_qualifications WHERE dataset_id=?1 AND target_id=?2)",
    ).map_err(database_error)?.query_row(params![dataset.0,item.target.0], |row| row.get(0))
        .map_err(database_error)?;
    if gated && !tempo_chess::game_generated_definition(item) {
        tx.execute(
            "DELETE FROM tactical_qualifications WHERE dataset_id=?1 AND target_id=?2",
            params![dataset.0, item.target.0],
        )
        .map_err(database_error)?;
    }
    Ok(())
}

/// Install the quality gate only for verified game-owned content with a retained
/// request. A game reference to an independent import does not transfer ownership.
pub(super) fn initialize_qualifications(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let mut query = tx.prepare(
        "SELECT c.dataset_id,c.item FROM current_content c WHERE EXISTS
        (SELECT 1 FROM tactical_requests r WHERE (r.dataset_id,r.target_id)=(c.dataset_id,c.target_id))",
    ).map_err(database_error)?;
    let mut rows = query.query([]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let item: ContentItem = text_value(row, 1)?;
        if tempo_chess::game_generated_definition(&item) {
            let dataset: String = row.get(0).map_err(database_error)?;
            tx.execute(
                "INSERT INTO tactical_qualifications VALUES(?1,?2,?3,'pending')",
                params![
                    dataset,
                    item.target.0,
                    item.version.0.to_be_bytes().as_slice()
                ],
            )
            .map_err(database_error)?;
        }
    }
    Ok(())
}

/// Apply the Chess verdict to previously qualified game-owned cards.
/// Unfinished qualifications already use the same rule in the normal worker.
pub(super) fn withhold_ambiguous_captures(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let items: Vec<ContentItem> = read_selected(
        tx,
        "SELECT c.item FROM current_content c LEFT JOIN tactical_qualifications q
         USING(dataset_id,target_id,version)
         WHERE (q.result IS NULL OR q.result IN ('kept','shortened','extended','revised'))
           AND EXISTS (SELECT 1 FROM tactical_requests r
                       WHERE (r.dataset_id,r.target_id)=(c.dataset_id,c.target_id))",
        [],
        |row| text_value(row, 0),
    )?;
    let mut withheld = 0;
    for item in items {
        let ambiguous =
            tempo_chess::ambiguous_hanging_material(&item.definition, &item.verified_tactic)
                .map_err(|error| invalid(&error.to_string()))?;
        if ambiguous && tempo_chess::game_generated_definition(&item) {
            withheld += tx.execute(
                "INSERT INTO tactical_qualifications(dataset_id,target_id,version,result)
                 SELECT dataset_id,target_id,version,'withheld' FROM current_content
                 WHERE target_id=?1
                 ON CONFLICT(dataset_id,target_id) DO UPDATE
                 SET version=excluded.version,result=excluded.result",
                [&item.target.0],
            )
            .map_err(database_error)?;
        }
    }
    if withheld > 0 {
        tx.execute(
            "UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1",
            [],
        )
        .map_err(database_error)?;
    }
    Ok(())
}

impl super::SqliteStore {
    /// Pending work is keyed to the current definition, even when the
    /// original request points to an older version.
    pub fn pending_game_tactic_qualifications(
        &self,
        dataset: &DatasetId,
        after: &str,
    ) -> Result<Vec<ContentItem>, StoreError> {
        read_selected(
            &self.connection,
            "SELECT c.item FROM tactical_qualifications q JOIN current_content c
             USING(dataset_id,target_id,version)
             WHERE q.dataset_id=?1 AND q.target_id>?2 AND q.result='pending'
             ORDER BY q.target_id LIMIT 32",
            params![dataset.0, after],
            |row| text_value(row, 0),
        )
    }

    pub fn game_tactic_qualification_held(
        &self,
        dataset: &DatasetId,
        item: &ContentItem,
    ) -> Result<bool, StoreError> {
        qualification_held(&self.connection, dataset, item)
    }

    /// Compare and commit Chess's result. A queued request can follow the new
    /// version. A request already attached to an attempt retains its evidence.
    pub fn publish_game_tactic_qualification(
        &self,
        dataset: &DatasetId,
        basis: &ContentItem,
        qualification: tempo_chess::GameTacticQualification,
    ) -> Result<TacticPayoffPublication, StoreError> {
        use tempo_chess::GameTacticQualification;
        let tx =
            Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let (actual, rev) = revision(&tx)?;
        if &actual != dataset || current(&tx, dataset, &basis.target)?.as_ref() != Some(basis) {
            return Ok(TacticPayoffPublication::Obsolete);
        }
        let version = basis.version.0.to_be_bytes();
        let pending: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tactical_qualifications
             WHERE (dataset_id,target_id,version)=(?1,?2,?3) AND result='pending')",
                params![dataset.0, basis.target.0, version.as_slice()],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if !pending {
            return Ok(TacticPayoffPublication::Obsolete);
        }
        if qualification_held(&tx, dataset, basis)? {
            return Ok(TacticPayoffPublication::Deferred);
        }
        let mut new_version = version;
        let mut outcome = TacticPayoffPublication::Published;
        let result = match qualification {
            GameTacticQualification::Kept => "kept",
            GameTacticQualification::Withheld => "withheld",
            GameTacticQualification::Revised(verified) => {
                let next = verified.replacement();
                if verified.prior() != basis {
                    return Err(invalid("invalid generated tactic revision"));
                }
                let lookup =
                    TacticLookup::from_item(next).ok_or_else(|| invalid("expected tactic"))?;
                if matches(&tx, dataset, &lookup)?
                    .iter()
                    .any(|item| item.target != basis.target)
                {
                    outcome = TacticPayoffPublication::ConflictingDefinition;
                    "withheld"
                } else {
                    new_version = next.version.0.to_be_bytes();
                    let payload = encode(next)?;
                    tx.execute(
                        "INSERT INTO content_history(ordinal,dataset_id,target_id,version,item)
                         VALUES((SELECT coalesce(max(ordinal)+1,0) FROM content_history),?1,?2,?3,?4)",
                        params![dataset.0,next.target.0,new_version.as_slice(),payload]
                    ).map_err(database_error)?;
                    tx.execute(
                        "UPDATE current_content SET version=?1,item=?2 WHERE target_id=?3",
                        params![new_version.as_slice(), payload, next.target.0],
                    )
                    .map_err(database_error)?;
                    tx.execute(
                        "DELETE FROM tactical_identities WHERE target_id=?1",
                        [&next.target.0],
                    )
                    .map_err(database_error)?;
                    index_item(&tx, next)?;
                    tx.execute(
                        "UPDATE tactical_requests SET version=?1 WHERE (dataset_id,target_id)=(?2,?3)
                         AND attempt_id IS NULL AND fulfilled=0 AND cancelled=0",
                        params![new_version.as_slice(),dataset.0,next.target.0],
                    ).map_err(database_error)?;
                    match next
                        .definition
                        .decisions
                        .len()
                        .cmp(&basis.definition.decisions.len())
                    {
                        std::cmp::Ordering::Less => "shortened",
                        std::cmp::Ordering::Greater => "extended",
                        std::cmp::Ordering::Equal if next.definition == basis.definition => "kept",
                        std::cmp::Ordering::Equal => "revised",
                    }
                }
            }
        };
        tx.execute(
            "UPDATE tactical_qualifications SET version=?1,result=?2
                    WHERE (dataset_id,target_id)=(?3,?4)",
            params![new_version.as_slice(), result, dataset.0, basis.target.0],
        )
        .map_err(database_error)?;
        let next_revision = rev
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("store revision exhausted"))?;
        tx.execute("UPDATE store_state SET revision=?1,content_revision=content_revision+?2 WHERE singleton=1",
            params![next_revision.to_be_bytes(),i32::from(new_version != version)],
        ).map_err(database_error)?;
        tx.execute(
            "UPDATE tactical_report_generation SET evidence=evidence+1 WHERE singleton=1",
            [],
        )
        .map_err(database_error)?;
        tx.commit().map_err(database_error)?;
        Ok(outcome)
    }
}

fn qualification_held(
    connection: &Connection,
    dataset: &DatasetId,
    item: &ContentItem,
) -> Result<bool, StoreError> {
    let issued: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM store_state JOIN attempts ON attempts.id=active_attempt_id
                       WHERE attempts.dataset_id=?1 AND attempts.target_id=?2)
         OR EXISTS(SELECT 1 FROM tactical_requests
                   WHERE dataset_id=?1 AND target_id=?2 AND fulfilled=0 AND cancelled=0 AND attempt_id IS NOT NULL)",
         params![dataset.0,item.target.0], |row| row.get(0),
    ).map_err(database_error)?;
    Ok(issued
        || learning(connection, &item.target)?.is_some_and(|state| {
            state
                .correction
                .is_some_and(|c| c.answer_version == item.version)
        }))
}
