//! Consistent inputs for current catalog changes.
use crate::{GameComparisonBasis, Snapshot, StoreError, StoredAttempt, StudyScope, StudyState};
use std::collections::{BTreeMap, BTreeSet};
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
    participation_revision: Revision,
    pub suspended_repertoires: BTreeSet<String>,
    pub excluded_tactics: BTreeSet<TargetId>,
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
        Self::from_study(
            state,
            GameComparisonBasis {
                content: content_revision,
                participation: Revision(0),
            },
        )
        .expect("complete catalog was requested")
    }

    pub fn game_comparison_basis(&self) -> GameComparisonBasis {
        GameComparisonBasis {
            content: self.content_revision,
            participation: self.participation_revision,
        }
    }

    pub(crate) fn from_study(
        state: StudyState,
        basis: GameComparisonBasis,
    ) -> Result<Self, StoreError> {
        state.complete_catalog()?;
        Ok(Self {
            dataset: state.dataset,
            revision: state.revision,
            content_revision: basis.content,
            participation_revision: basis.participation,
            suspended_repertoires: state.suspended_repertoires,
            excluded_tactics: state.excluded_tactics,
            catalog: state.catalog,
            learning: state.learning,
            active_attempt: state.active_attempt,
            attempts: state.attempts,
        })
    }
}
