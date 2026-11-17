//! Store and its usage scenarios.

use crate::{Commit, Snapshot, StoreError};
use tempo_types::Revision;

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
