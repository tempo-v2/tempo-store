//! Stage one typed write in a private snapshot; publication happens later.
use crate::store_error::{conflict, invalid};
use crate::{ContentHistory, Snapshot, StoreError, Write};
use tempo_study::AttemptStatus;
use tempo_types::Revision;

fn remember_catalog(state: &mut Snapshot) {
    let retained: std::collections::BTreeSet<_> = state
        .content_history
        .iter()
        .filter(|entry| entry.dataset == state.dataset)
        .map(|entry| (entry.item.target.clone(), entry.item.version))
        .collect();
    for item in state.catalog.items.values() {
        if !retained.contains(&(item.target.clone(), item.version)) {
            state.content_history.push(ContentHistory {
                dataset: state.dataset.clone(),
                item: item.clone(),
            });
        }
    }
}

pub(super) fn apply_write(state: &mut Snapshot, write: Write) -> Result<(), StoreError> {
    match write {
        Write::Issue(record) => {
            let attempt = &record.attempt;
            if state.attempts.contains_key(&attempt.id) {
                return Err(conflict("attempt ID already exists"));
            }
            let item = state
                .catalog
                .items
                .get(&attempt.target)
                .ok_or_else(|| invalid("unknown target"))?;
            let learning = state
                .learning
                .get(&attempt.target)
                .ok_or_else(|| invalid("target has no learning state"))?;
            if !item.active()
                || attempt.dataset != state.dataset
                || attempt.content_version != item.version
                || attempt.revision != Revision(0)
                || attempt.terminal.is_some()
                || record.acceptance.is_some()
                || attempt.status != AttemptStatus::Active
                || record.issued_miss_revision != learning.miss_revision
            {
                return Err(invalid("invalid issued attempt"));
            }
            state.attempts.insert(attempt.id.clone(), record);
        }
        Write::Progress(record) => {
            let next = &record.attempt;
            let previous = state
                .attempts
                .get(&next.id)
                .ok_or_else(|| invalid("unknown attempt"))?;
            let old = &previous.attempt;
            if next.dataset != state.dataset
                || next.dataset != old.dataset
                || next.target != old.target
                || next.content_version != old.content_version
                || record.issued_at != previous.issued_at
                || record.issued_miss_revision != previous.issued_miss_revision
                || record.mode != previous.mode
                || record.route != previous.route
                || record.first_introduction != previous.first_introduction
                || record.repertoire_name != previous.repertoire_name
                || record.lesson != previous.lesson
                || (previous.acceptance.is_some() && record.acceptance != previous.acceptance)
                || (record.mode == crate::ActivityMode::Teach && next.terminal.is_some())
                || old.status != AttemptStatus::Active
                || old.revision.0.checked_add(1) != Some(next.revision.0)
                || (old.terminal.is_some() && next.terminal != old.terminal)
            {
                return Err(conflict(
                    "attempt identity, revision or terminal outcome changed illegally",
                ));
            }
            if let Some(acceptance) = &record.acceptance {
                use crate::{AcceptanceKind, ActivityMode};
                use tempo_types::Grade;
                if acceptance.at.unix_ms < record.issued_at.unix_ms
                    || acceptance.at.day < record.issued_at.day
                {
                    return Err(invalid("activity acceptance precedes issuance"));
                }
                let coherent = match &acceptance.kind {
                    AcceptanceKind::Teaching => {
                        record.mode == ActivityMode::Teach
                            && next.status == AttemptStatus::Complete
                            && next.terminal.is_none()
                    }
                    AcceptanceKind::Recall => {
                        record.mode != ActivityMode::Teach && next.terminal == Some(Grade::Good)
                    }
                    AcceptanceKind::WrongMove { .. } | AcceptanceKind::Revealed { .. } => {
                        record.mode != ActivityMode::Teach && next.terminal == Some(Grade::Again)
                    }
                };
                if !coherent {
                    return Err(invalid(
                        "activity evidence is inconsistent with its outcome",
                    ));
                }
            }
            state.attempts.insert(next.id.clone(), record);
        }
        Write::Learning {
            target,
            state: value,
        } => {
            state.learning.insert(target, value);
        }
        Write::Assessment(record) => {
            if state
                .assessments
                .iter()
                .any(|old| old.dataset == record.dataset && old.key == record.key)
            {
                return Err(conflict(
                    "assessment already committed for this attempt/source event",
                ));
            }
            if record.dataset != state.dataset {
                return Err(conflict("assessment dataset changed"));
            }
            state.assessments.push(record);
        }
        Write::Receipt(receipt) => {
            if state.receipts.contains_key(&receipt.id) {
                return Err(conflict("submission ID already committed"));
            }
            state.receipts.insert(receipt.id.clone(), receipt);
        }
        Write::Catalog(catalog) => {
            catalog
                .validate()
                .map_err(|e| StoreError::Invalid(e.to_string()))?;
            for (id, old) in &state.catalog.items {
                let Some(new) = catalog.items.get(id) else {
                    return Err(invalid(
                        "retire content explicitly; ordinary import cannot delete it",
                    ));
                };
                if new.version < old.version
                    || (new.definition != old.definition && new.version == old.version)
                {
                    return Err(invalid("semantic content edits must increase version"));
                }
            }
            state.catalog = catalog;
            remember_catalog(state);
        }
        Write::Active(id) => {
            state.active_attempt = id;
        }
        Write::Restore {
            dataset,
            catalog,
            learning,
            policy,
        } => {
            if state.known_datasets.contains(&dataset) {
                return Err(conflict("restore needs a fresh dataset identity"));
            }
            catalog
                .validate()
                .map_err(|e| StoreError::Invalid(e.to_string()))?;
            state.known_datasets.insert(dataset.clone());
            state.dataset = dataset;
            state.catalog = catalog;
            state.learning = learning;
            state.policy = policy;
            state.active_attempt = None;
            remember_catalog(state);
        }
    }
    Ok(())
}
