//! Consistent inputs for current catalog changes.
use crate::{Snapshot, StoreError, StoredAttempt, StudyScope, StudyState};
use std::collections::BTreeMap;
use tempo_chess::Catalog;
use tempo_learning::LearningState;
use tempo_types::{AttemptId, DatasetId, Revision, TargetId};

/// Complete current content and Learning, with the active attempt context.
/// Store and content revisions come from the same read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentState {
    pub dataset: DatasetId,
    pub revision: Revision,
    pub content_revision: Revision,
    pub catalog: Catalog,
    pub learning: BTreeMap<TargetId, LearningState>,
    pub active_attempt: Option<AttemptId>,
    pub attempts: BTreeMap<AttemptId, StoredAttempt>,
}

impl ContentState {
    /// Project from a consistent snapshot and content revision held at that same read point.
    pub fn from_snapshot(snapshot: &Snapshot, content_revision: Revision) -> Self {
        let state = StudyState::from_snapshot(
            snapshot,
            StudyScope {
                catalog: true,
                ..Default::default()
            },
        );
        Self::from_study(state, content_revision).expect("complete catalog was requested")
    }

    pub(crate) fn from_study(
        state: StudyState,
        content_revision: Revision,
    ) -> Result<Self, StoreError> {
        state.complete_catalog()?;
        Ok(Self {
            dataset: state.dataset,
            revision: state.revision,
            content_revision,
            catalog: state.catalog,
            learning: state.learning,
            active_attempt: state.active_attempt,
            attempts: state.attempts,
        })
    }
}
