//! AnswerResult and its usage scenarios.

use crate::ChessAttempt;
use serde::{Deserialize, Serialize};
use tempo_chess::ChessFeedback;
use tempo_learning::LearningEffect;
use tempo_types::Grade;

/// The application-visible result retained for a Study command.
///
/// # When to use
///
/// The application assembles this after progressing or abandoning an attempt, then
/// stores it inside [`crate::ReceiptResult::Study`]. Retrying that command can return
/// the same result even after the attempt progresses again.
///
/// `assessment` and `learning_effect` describe a **new** assessment from this
/// command and are present together. They can both be absent while
/// `attempt.terminal` remains `Some(Again)` during correction. `feedback` is optional
/// because some commands, such as abandoning, have no grader response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerResult {
    pub attempt: ChessAttempt,
    pub feedback: Option<ChessFeedback>,
    pub assessment: Option<Grade>,
    pub learning_effect: Option<LearningEffect>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::test_support::{issued_record, store};
    use tempo_chess::{ExactSequenceGrader, demo_catalog};
    use tempo_study::AttemptStatus;

    #[test]
    fn a_correction_result_keeps_the_terminal_miss_but_reports_no_new_learning_effect() {
        let store = store(demo_catalog());
        let snapshot = store.snapshot().unwrap();
        let issued = issued_record(&snapshot);
        let definition = &snapshot.catalog.items[&issued.attempt.target].definition;
        let miss = tempo_study::submit(
            &ExactSequenceGrader,
            definition,
            &issued.attempt,
            issued.attempt.revision,
            &"d2d4".into(),
        )
        .unwrap();
        assert_eq!(miss.assessment, Some(Grade::Again));
        let correction = tempo_study::submit(
            &ExactSequenceGrader,
            definition,
            &miss.attempt,
            miss.attempt.revision,
            &"e2e4".into(),
        )
        .unwrap();
        let result = AnswerResult {
            attempt: correction.attempt,
            feedback: Some(correction.feedback),
            assessment: correction.assessment,
            learning_effect: None,
        };
        assert!(result.feedback.as_ref().unwrap().correct);
        assert_eq!(result.attempt.terminal, Some(Grade::Again));
        assert_eq!(result.attempt.status, AttemptStatus::Complete);
        assert_eq!(result.assessment, None);
        assert_eq!(result.learning_effect, None);
    }
}
