//! Stage typed writes and enforce Store invariants before either adapter publishes.
use crate::store_error::{conflict, invalid};
use crate::{Commit, ContentHistory, Snapshot, StoreError, Write};
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::Catalog;
use tempo_learning::{LearningState, SchedulerSettings};
use tempo_types::{DatasetId, Revision};

mod apply_write;
mod validate_outcomes;
mod validate_snapshot;

use apply_write::apply_write;
use validate_outcomes::validate_new_outcomes;
use validate_snapshot::validate_snapshot;

pub(super) fn initial_snapshot(
    dataset: DatasetId,
    catalog: Catalog,
    policy: SchedulerSettings,
) -> Result<Snapshot, StoreError> {
    catalog
        .validate()
        .map_err(|e| StoreError::Invalid(e.to_string()))?;
    let learning = catalog
        .items
        .keys()
        .map(|id| (id.clone(), LearningState::default()))
        .collect();
    let content_history = catalog
        .items
        .values()
        .cloned()
        .map(|item| ContentHistory {
            dataset: dataset.clone(),
            item,
        })
        .collect();
    let known_datasets = BTreeSet::from([dataset.clone()]);
    Ok(Snapshot {
        revision: Revision(0),
        dataset,
        known_datasets,
        catalog,
        learning,
        policy,
        attempts: BTreeMap::new(),
        active_attempt: None,
        repertoire_run: None,
        assessments: vec![],
        receipts: BTreeMap::new(),
        content_history,
    })
}

pub(super) fn stage_commit(previous: &Snapshot, commit: Commit) -> Result<Snapshot, StoreError> {
    if commit.expected_revision != previous.revision || commit.expected_dataset != previous.dataset
    {
        return Err(conflict("snapshot changed; reload before retrying"));
    }
    let mut staged = previous.clone();
    let assessments = commit
        .writes
        .iter()
        .filter_map(|write| match write {
            Write::Assessment(record) => Some(record.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let receipts = commit
        .writes
        .iter()
        .filter_map(|write| match write {
            Write::Receipt(receipt) => Some(receipt.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for write in commit.writes {
        apply_write(&mut staged, write)?;
    }
    validate_snapshot(&staged)?;
    validate_new_outcomes(previous, &staged, &assessments, &receipts)?;
    staged.revision = Revision(
        staged
            .revision
            .0
            .checked_add(1)
            .ok_or_else(|| invalid("store revision exhausted"))?,
    );
    Ok(staged)
}
