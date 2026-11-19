//! StoreError and its usage scenarios.

use std::{error::Error, fmt};

/// An explicit persistence availability, concurrency or consistency failure.
///
/// # When to use
///
/// Application commands handle this after snapshot or commit. `Unavailable` reports
/// an adapter failure, `Conflict` means assumptions or uniqueness no longer hold,
/// and `Invalid` means the proposed records violate the persistence contract.
///
/// An unsuccessful commit publishes no partial state. The application uses receipt
/// lookup and a fresh snapshot to decide whether replay, retry or a user-facing
/// conflict is appropriate. Errors never masquerade as a learner's grade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Unavailable(String),
    Conflict(String),
    Invalid(String),
    /// The selected adapter does not implement this operation.
    Unsupported(&'static str),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for StoreError {}

pub(crate) fn invalid(message: &str) -> StoreError {
    StoreError::Invalid(message.into())
}
pub(crate) fn conflict(message: &str) -> StoreError {
    StoreError::Conflict(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{issued_record, store};
    use crate::{Commit, Store, Write};
    use tempo_chess::demo_catalog;

    #[test]
    fn a_stale_snapshot_is_a_conflict_and_does_not_replace_the_newer_result() {
        let mut store = store(demo_catalog());
        let stale = store.snapshot().unwrap();
        store
            .commit(Commit {
                expected_revision: stale.revision,
                expected_dataset: stale.dataset.clone(),
                writes: vec![
                    Write::Issue(issued_record(&stale)),
                    Write::Active(Some("attempt-1".into())),
                ],
            })
            .unwrap();
        let committed = store.snapshot().unwrap();
        let error = store
            .commit(Commit {
                expected_revision: stale.revision,
                expected_dataset: stale.dataset,
                writes: vec![Write::Active(None)],
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Conflict(_)));
        assert_eq!(store.snapshot().unwrap(), committed);
    }
}
