//! Invariants that must hold across both current and historical records.
use crate::store_error::invalid;
use crate::{AssessmentKey, Snapshot, StoreError};
use tempo_learning::AssessmentSource;
use tempo_study::AttemptStatus;
use tempo_types::Grade;

pub(super) fn validate_snapshot(state: &Snapshot) -> Result<(), StoreError> {
    if !state.known_datasets.contains(&state.dataset) {
        return Err(invalid("current dataset identity must be retained"));
    }
    if state.learning.len() != state.catalog.items.len()
        || state
            .catalog
            .items
            .keys()
            .any(|id| !state.learning.contains_key(id))
    {
        return Err(invalid(
            "learning targets must exactly match current content",
        ));
    }
    if let Some(id) = &state.active_attempt {
        let row = state
            .attempts
            .get(id)
            .ok_or_else(|| invalid("active attempt does not exist"))?;
        if row.attempt.dataset != state.dataset
            || row.attempt.status != AttemptStatus::Active
            || state
                .catalog
                .items
                .get(&row.attempt.target)
                .is_none_or(|item| !item.active() || item.version != row.attempt.content_version)
        {
            return Err(invalid(
                "active attempt must be open in the current dataset",
            ));
        }
    }
    let mut assessed = std::collections::BTreeSet::new();
    for record in &state.assessments {
        if let AssessmentKey::Human(id) = &record.key {
            assessed.insert((record.dataset.clone(), id.clone()));
            let issued = state
                .attempts
                .get(id)
                .ok_or_else(|| invalid("assessment has no attempt"))?;
            if issued.mode != crate::ActivityMode::Test
                || issued.attempt.dataset != record.dataset
                || issued.attempt.target != record.target
                || issued.attempt.content_version != record.content_version
                || issued.attempt.terminal != Some(record.input.grade)
                || !matches!(&record.input.source, AssessmentSource::Human { attempt_id, issued_miss_revision }
                    if attempt_id == id && *issued_miss_revision == issued.issued_miss_revision)
            {
                return Err(invalid("assessment does not match its attempt"));
            }
        }
    }
    for record in state.attempts.values() {
        let attempt = &record.attempt;
        if (attempt.status == AttemptStatus::Active) != attempt.prompt.is_some()
            || (attempt.terminal == Some(Grade::Good) && attempt.status != AttemptStatus::Complete)
        {
            return Err(invalid(
                "attempt presentation and terminal state are inconsistent",
            ));
        }
        if record.attempt.terminal.is_some()
            && record.mode == crate::ActivityMode::Test
            && !assessed.contains(&(attempt.dataset.clone(), attempt.id.clone()))
        {
            return Err(invalid(
                "terminal attempt requires an assessment in the same transaction",
            ));
        }
    }
    Ok(())
}
