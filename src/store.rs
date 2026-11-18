//! Store and its usage scenarios.

use crate::{Commit, Receipt, Snapshot, StoreError, StoredAttempt, store_error::invalid};
use tempo_chess::ContentItem;
use tempo_types::{
    AttemptId, ContentVersion, DatasetId, Revision, StudyDay, SubmissionId, TargetId,
};

/// Current study reads exclude unrelated attempts and their outcomes.
#[derive(Clone, Copy, Default)]
pub struct StudyScope<'a> {
    pub catalog: bool,
    pub attempt: Option<&'a AttemptId>,
    pub target: Option<&'a TargetId>,
    pub submission: Option<&'a SubmissionId>,
}

pub struct ContentReceipt<'a> {
    pub request: &'a str,
    pub reply: &'a str,
}

pub struct ContentSource<'a> {
    pub id: &'a str,
    pub pgn: &'a str,
}

/// The consistent-read and atomic-write persistence boundary.
///
/// # When to use
///
/// Inject an implementation into the application composition layer. The application
/// reads a snapshot, asks domain components for pure results, and commits the typed
/// batch. Study, Chess, Queue and Learning do not import or receive this trait.
///
/// [`crate::MemoryStore`] implements the contract in process memory.
/// [`crate::SqliteStore`] uses the same validation and publishes each batch
/// inside one database transaction.
pub trait Store {
    /// Read a consistent owned view without changing stored state.
    fn snapshot(&self) -> Result<Snapshot, StoreError>;
    /// Read current study state with only the referenced outcome/receipt.
    fn study_state(&self, _scope: StudyScope<'_>) -> Result<Snapshot, StoreError> {
        self.snapshot()
    }

    /// Publish a study batch checked against the same current-state scope.
    fn commit_study(
        &mut self,
        commit: Commit,
        _scope: StudyScope<'_>,
    ) -> Result<Revision, StoreError> {
        self.commit(commit)
    }

    fn tactics_extra(&self, _dataset: &DatasetId, _day: StudyDay) -> Result<usize, StoreError> {
        Ok(0)
    }

    /// Persist a day-scoped tactics allowance and receipt in one transaction.
    fn extend_tactics_allowance(
        &mut self,
        _dataset: &DatasetId,
        _revision: Revision,
        _day: StudyDay,
        _increase: usize,
        _receipt: &Receipt,
    ) -> Result<(), StoreError> {
        Err(invalid(
            "this Store does not support daily tactics allowances",
        ))
    }

    fn content_state(&self) -> Result<(Snapshot, Revision), StoreError> {
        let state = self.snapshot()?;
        let revision = state.revision;
        Ok((state, revision))
    }

    fn content_receipt(&self, _request: &str) -> Result<Option<String>, StoreError> {
        Ok(None)
    }

    /// Pin sources and the application's reply in the content transaction.
    fn commit_content(
        &mut self,
        commit: Commit,
        _source: Option<ContentSource<'_>>,
        _receipt: Option<ContentReceipt<'_>>,
    ) -> Result<Revision, StoreError> {
        self.commit(commit)
    }

    fn stored_attempt(&self, id: &AttemptId) -> Result<Option<StoredAttempt>, StoreError> {
        Ok(self.snapshot()?.attempts.remove(id))
    }

    fn issued_item(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
    ) -> Result<Option<ContentItem>, StoreError> {
        Ok(self
            .snapshot()?
            .content_history
            .into_iter()
            .find(|entry| {
                &entry.dataset == dataset
                    && &entry.item.target == target
                    && entry.item.version == version
            })
            .map(|entry| entry.item))
    }

    /// Publish the whole batch and return its new store revision, or publish nothing.
    fn commit(&mut self, commit: Commit) -> Result<Revision, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{issued_record, policy};
    use crate::{MemoryStore, Write};
    use tempo_chess::demo_catalog;
    use tempo_learning::LearningPolicy;

    #[test]
    fn application_code_uses_the_port_without_knowing_the_memory_adapter() {
        let mut memory =
            MemoryStore::new("dataset-a".into(), demo_catalog(), policy().settings()).unwrap();
        let store: &mut dyn Store = &mut memory;
        let snapshot = store.snapshot().unwrap();
        let issued = issued_record(&snapshot);
        let expected_id = issued.attempt.id.clone();
        store
            .commit(Commit {
                expected_revision: snapshot.revision,
                expected_dataset: snapshot.dataset,
                writes: vec![
                    Write::Issue(issued),
                    Write::Active(Some(expected_id.clone())),
                ],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.active_attempt, Some(expected_id.clone()));
        assert!(after.attempts.contains_key(&expected_id));
    }
}
