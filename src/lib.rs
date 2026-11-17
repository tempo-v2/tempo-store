//! Typed persistence port and an atomic in-memory implementation.
//! No domain crate receives this store or its transaction mechanism.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};
use tempo_chess::{Catalog, ChessFeedback, ChessPrompt, ChessState, ContentItem};
use tempo_learning::{
    AssessmentInput, AssessmentSource, LearningEffect, LearningState, SchedulerSettings, Transition,
};
use tempo_study::{Attempt, AttemptStatus};
use tempo_types::{
    AttemptId, ContentVersion, DatasetId, EventId, Grade, MissRevision, Revision, StudyTime,
    SubmissionId, TargetId,
};

pub type ChessAttempt = Attempt<ChessState, ChessPrompt>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAttempt {
    pub attempt: ChessAttempt,
    pub issued_at: StudyTime,
    pub issued_miss_revision: MissRevision,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssessmentKey {
    Human(AttemptId),
    External { provider: String, event_id: EventId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessmentRecord {
    pub key: AssessmentKey,
    pub dataset: DatasetId,
    pub target: TargetId,
    pub content_version: ContentVersion,
    pub input: AssessmentInput,
    pub transition: Transition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerResult {
    pub attempt: ChessAttempt,
    pub feedback: Option<ChessFeedback>,
    pub assessment: Option<Grade>,
    pub learning_effect: Option<LearningEffect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptResult {
    Study(AnswerResult),
    External {
        target: TargetId,
        transition: Transition,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub id: SubmissionId,
    /// Canonical serialization of the typed command, excluding acceptance time.
    /// This is an equality token, never an executable/domain message.
    pub command: String,
    pub result: ReceiptResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHistory {
    pub dataset: DatasetId,
    pub item: ContentItem,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: Revision,
    pub dataset: DatasetId,
    /// Dataset identities are never reused, including those with empty catalogs.
    #[serde(default)]
    pub known_datasets: BTreeSet<DatasetId>,
    pub catalog: Catalog,
    pub learning: BTreeMap<TargetId, LearningState>,
    pub policy: SchedulerSettings,
    pub attempts: BTreeMap<AttemptId, StoredAttempt>,
    pub active_attempt: Option<AttemptId>,
    pub assessments: Vec<AssessmentRecord>,
    pub receipts: BTreeMap<SubmissionId, Receipt>,
    pub content_history: Vec<ContentHistory>,
}

#[derive(Clone, Debug)]
pub enum Write {
    Issue(StoredAttempt),
    Progress(StoredAttempt),
    Learning {
        target: TargetId,
        state: LearningState,
    },
    Assessment(AssessmentRecord),
    Receipt(Receipt),
    Catalog(Catalog),
    Active(Option<AttemptId>),
    Restore {
        dataset: DatasetId,
        catalog: Catalog,
        learning: BTreeMap<TargetId, LearningState>,
        policy: SchedulerSettings,
    },
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub expected_revision: Revision,
    pub expected_dataset: DatasetId,
    pub writes: Vec<Write>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Unavailable(String),
    Conflict(String),
    Invalid(String),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for StoreError {}

/// A snapshot is a consistent read. A batch commits completely, or not at all.
/// SQL implementations must run the same contract in one database transaction.
pub trait Store {
    fn snapshot(&self) -> Result<Snapshot, StoreError>;
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError>;
}

pub struct MemoryStore {
    state: Snapshot,
    fail_next_commit: bool,
}
impl MemoryStore {
    pub fn new(
        dataset: DatasetId,
        catalog: Catalog,
        policy: SchedulerSettings,
    ) -> Result<Self, StoreError> {
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
        Ok(Self {
            state: Snapshot {
                revision: Revision(0),
                dataset,
                known_datasets,
                catalog,
                learning,
                policy,
                attempts: BTreeMap::new(),
                active_attempt: None,
                assessments: vec![],
                receipts: BTreeMap::new(),
                content_history,
            },
            fail_next_commit: false,
        })
    }
    /// Fault injection after staging, before publishing, for transaction contract tests.
    pub fn fail_next_commit(&mut self) {
        self.fail_next_commit = true;
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}

fn remember_catalog(state: &mut Snapshot) {
    for item in state.catalog.items.values() {
        if !state.content_history.iter().any(|old| {
            old.dataset == state.dataset
                && old.item.target == item.target
                && old.item.version == item.version
        }) {
            state.content_history.push(ContentHistory {
                dataset: state.dataset.clone(),
                item: item.clone(),
            });
        }
    }
}

fn apply(state: &mut Snapshot, write: Write) -> Result<(), StoreError> {
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
                || old.status != AttemptStatus::Active
                || old.revision.0.checked_add(1) != Some(next.revision.0)
                || (old.terminal.is_some() && next.terminal != old.terminal)
            {
                return Err(conflict(
                    "attempt identity, revision or terminal outcome changed illegally",
                ));
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

fn validate(state: &Snapshot) -> Result<(), StoreError> {
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
        if row.attempt.dataset != state.dataset || row.attempt.status != AttemptStatus::Active {
            return Err(invalid(
                "active attempt must be open in the current dataset",
            ));
        }
    }
    for record in &state.assessments {
        if let AssessmentKey::Human(id) = &record.key {
            let issued = state
                .attempts
                .get(id)
                .ok_or_else(|| invalid("assessment has no attempt"))?;
            if issued.attempt.dataset != record.dataset
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
            && !state.assessments.iter().any(|a| {
                a.key == AssessmentKey::Human(record.attempt.id.clone())
                    && a.dataset == record.attempt.dataset
            })
        {
            return Err(invalid(
                "terminal attempt requires an assessment in the same transaction",
            ));
        }
    }
    Ok(())
}

/// Validate only the newly appended outcomes against this transaction's final state.
/// Historical outcomes deliberately retain older content, attempt and learning values.
fn validate_new_outcomes(
    previous: &Snapshot,
    staged: &Snapshot,
    assessments: &[AssessmentRecord],
    receipts: &[Receipt],
) -> Result<(), StoreError> {
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

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Snapshot, StoreError> {
        Ok(self.state.clone())
    }
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError> {
        if commit.expected_revision != self.state.revision
            || commit.expected_dataset != self.state.dataset
        {
            return Err(conflict("snapshot changed; reload before retrying"));
        }
        let mut staged = self.state.clone();
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
            apply(&mut staged, write)?;
        }
        validate(&staged)?;
        validate_new_outcomes(&self.state, &staged, &assessments, &receipts)?;
        staged.revision = Revision(
            staged
                .revision
                .0
                .checked_add(1)
                .ok_or_else(|| invalid("store revision exhausted"))?,
        );
        if std::mem::take(&mut self.fail_next_commit) {
            return Err(StoreError::Unavailable("injected commit failure".into()));
        }
        self.state = staged;
        Ok(self.state.revision)
    }
}

#[cfg(test)]
mod tests;
