//! The application-selected steps for one in-flight repertoire run.
use crate::{ActivityMode, StoreError, StoredAttempt, store_error::invalid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tempo_chess::OpponentLeadIn;
use tempo_types::{AttemptId, ContentVersion, TargetId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepertoireRun {
    pub repertoire: String,
    pub source_id: String,
    pub steps: Vec<RepertoireRunStep>,
    pub skipped_warmup: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepertoireRunStep {
    pub target: TargetId,
    pub version: ContentVersion,
    pub lead_in: Option<OpponentLeadIn>,
    /// Practice is the optional first warm-up. Every other step is a Test.
    pub mode: ActivityMode,
    pub issued: Option<AttemptId>,
}

impl RepertoireRun {
    pub fn next(&self) -> Option<&RepertoireRunStep> {
        self.steps.iter().find(|step| step.issued.is_none())
    }

    pub fn head(&self) -> Option<&AttemptId> {
        self.steps
            .iter()
            .rev()
            .find_map(|step| step.issued.as_ref())
    }

    pub(crate) fn validate_issue(&self, record: &StoredAttempt) -> Result<(), StoreError> {
        let mut unissued = false;
        let mut targets = BTreeSet::new();
        let mut tests = 0;
        let mut head = None;
        for (i, step) in self.steps.iter().enumerate() {
            match step.mode {
                ActivityMode::Test => tests += 1,
                ActivityMode::Practice if i == 0 => {}
                _ => return Err(invalid("invalid repertoire run activity")),
            }
            if !targets.insert(&step.target) || (i > 0 && step.lead_in.is_none()) {
                return Err(invalid(
                    "repertoire run requires distinct connected targets",
                ));
            }
            if step.issued.is_none() {
                unissued = true;
            } else if unissued {
                return Err(invalid("repertoire run steps must be issued in order"));
            } else {
                head = Some(step);
            }
        }
        let Some(head) = head else {
            return Err(invalid("repertoire run has no issued step"));
        };
        if !(1..=3).contains(&tests)
            || self.steps.len() < 2
            || self.repertoire.is_empty()
            || self.source_id.is_empty()
            || head.issued.as_ref() != Some(&record.attempt.id)
            || head.target != record.attempt.target
            || head.version != record.attempt.content_version
            || head.mode != record.mode
            || (head.mode == ActivityMode::Practice) != record.repertoire_warmup
        {
            return Err(invalid("repertoire run does not match the issued step"));
        }
        Ok(())
    }
}
