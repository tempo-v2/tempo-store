//! ChessAttempt and its usage scenarios.

use tempo_chess::{ChessPrompt, ChessState};
use tempo_study::Attempt;

/// Study's attempt specialized to the chess state and presentation types.
///
/// # When to use
///
/// This alias preserves the concrete attempt shape used by persistence records and
/// application replies. Study still owns progression; Store merely stores the
/// result. The alias is the same type as [`tempo_chess::ChessAttempt`].
///
/// Use [`crate::StoredAttempt`] when persistence also needs issuance time and the
/// learning miss revision captured at issuance.
pub type ChessAttempt = Attempt<ChessState, ChessPrompt>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::test_support::{issued_record, store};
    use tempo_chess::{ExactSequenceGrader, demo_catalog};
    use tempo_types::Grade;

    #[test]
    fn the_stored_alias_is_the_same_attempt_that_study_can_advance() {
        let store = store(demo_catalog());
        let snapshot = store.snapshot().unwrap();
        let stored = issued_record(&snapshot);
        let attempt: ChessAttempt = stored.attempt;
        let definition = &snapshot.catalog.items[&attempt.target].definition;
        let progress = tempo_study::submit(
            &ExactSequenceGrader,
            definition,
            &attempt,
            attempt.revision,
            &"e2e4".into(),
        )
        .unwrap();
        assert_eq!(progress.assessment, Some(Grade::Good));
        assert_eq!(progress.attempt.id, attempt.id);
        assert_eq!(progress.attempt.content_version, attempt.content_version);
    }
}
