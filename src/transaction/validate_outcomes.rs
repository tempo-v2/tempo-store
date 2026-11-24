//! Checks for new assessments and receipts against the transaction's final state.
//! Historical outcomes are deliberately allowed to retain earlier values.
use crate::store_error::{conflict, invalid};
use crate::{AssessmentKey, AssessmentRecord, Receipt, ReceiptResult, Snapshot, StoreError};
use std::collections::BTreeSet;
use tempo_learning::AssessmentSource;

/// Validate only the newly appended outcomes against this transaction's final state.
/// Historical outcomes deliberately retain older content, attempt and learning values.
pub(super) fn validate_new_outcomes(
    previous: &Snapshot,
    staged: &Snapshot,
    assessments: &[AssessmentRecord],
    receipts: &[Receipt],
) -> Result<(), StoreError> {
    for (id, attempt) in &staged.attempts {
        let Some(acceptance) = &attempt.acceptance else {
            continue;
        };
        if previous
            .attempts
            .get(id)
            .and_then(|old| old.acceptance.as_ref())
            .is_some()
        {
            continue;
        }
        if attempt.mode.guided() {
            let prior = &previous.learning[&attempt.attempt.target];
            let current = &staged.learning[&attempt.attempt.target];
            if attempt.mode == crate::ActivityMode::Repair {
                let expected = if let Some(work) = &attempt.issued_work {
                    tempo_learning::DailyLearning::complete_human_correction(
                        prior, work.obligation.version, work.obligation.revision,
                    )
                } else {
                    tempo_learning::DailyLearning::repair(
                        prior, attempt.attempt.content_version, attempt.issued_miss_revision, acceptance.at,
                    ).map_err(|_| invalid("invalid guided repair"))?
                };
                if *current != expected {
                    return Err(invalid(
                        "guided repair must commit with its learning transition",
                    ));
                }
            } else if attempt.issued_miss_revision != prior.miss_revision {
                if current != prior {
                    return Err(invalid("an older lesson cannot change newer learning"));
                }
            } else if current.taught_at != Some(acceptance.at)
                || current.scheduler_memory.is_some()
                || current.last_review_at.is_some()
                || !current.introduced
            {
                return Err(invalid(
                    "teaching and its learning transition must commit together",
                ));
            }
        }
    }
    let mut targets = BTreeSet::new();
    for record in assessments {
        if !targets.insert(record.target.clone()) {
            return Err(invalid(
                "one assessment per target per transaction is supported",
            ));
        }
        let item = staged
            .catalog
            .items
            .get(&record.target)
            .ok_or_else(|| invalid("assessment target does not exist"))?;
        if record.dataset != staged.dataset || record.content_version != item.version {
            return Err(conflict("assessment does not address current content"));
        }
        if staged.learning.get(&record.target) != Some(&record.transition.state) {
            return Err(invalid(
                "assessment and learning transition must commit together",
            ));
        }
        match (&record.key, &record.input.source) {
            (AssessmentKey::Human(id), AssessmentSource::Human { attempt_id, .. })
                if id == attempt_id => {}
            (
                AssessmentKey::External { provider, event_id },
                AssessmentSource::External {
                    provider: source_provider,
                    event_id: source_event_id,
                    ..
                },
            ) if provider == source_provider && event_id == source_event_id => {}
            _ => return Err(invalid("assessment key and source disagree")),
        }
    }
    for receipt in receipts {
        // An alias may preserve an exact previously committed result. A historical
        // receipt must never be rewritten to the latest attempt/learning state.
        if previous
            .receipts
            .values()
            .any(|old| old.result == receipt.result)
        {
            continue;
        }
        match &receipt.result {
            ReceiptResult::Study(result) => {
                let stored = staged
                    .attempts
                    .get(&result.attempt.id)
                    .ok_or_else(|| invalid("study receipt has no committed attempt"))?;
                if stored.attempt != result.attempt || result.attempt.dataset != staged.dataset {
                    return Err(invalid(
                        "study receipt does not match the committed attempt",
                    ));
                }
                match (result.assessment, result.learning_effect) {
                    (None, None)
                        if !assessments.iter().any(|record| {
                            record.key == AssessmentKey::Human(result.attempt.id.clone())
                        }) => {}
                    (Some(grade), Some(effect))
                        if assessments.iter().any(|record| {
                            record.key == AssessmentKey::Human(result.attempt.id.clone())
                                && record.dataset == result.attempt.dataset
                                && record.input.grade == grade
                                && record.transition.effect == effect
                        }) => {}
                    _ => {
                        return Err(invalid(
                            "study receipt assessment has no matching learning outcome",
                        ));
                    }
                }
            }
            ReceiptResult::TacticsAllowance { .. } => {
                return Err(invalid(
                    "allowance receipts require a daily allowance transaction",
                ));
            }
            ReceiptResult::ResumeSkipped { .. } => {
                return Err(invalid("resume receipts require a day deferral transaction"));
            }
            ReceiptResult::External { target, transition } => {
                if !assessments.iter().any(|record| {
                    matches!(record.key, AssessmentKey::External { .. })
                        && &record.target == target
                        && &record.transition == transition
                }) {
                    return Err(invalid(
                        "external receipt has no matching committed assessment",
                    ));
                }
            }
        }
    }
    Ok(())
}
