//! Compact selection and addressed playable Study content.

use crate::StoreError;
use tempo_chess::{ChessDefinition, ContentStatus, RepertoireLesson, TacticPolicyFacts};
use tempo_study::Exercise;
use tempo_types::{ContentVersion, DatasetId, TargetId};

#[derive(Debug, PartialEq, Eq)]
pub enum StudyContentFamily {
    Repertoire {
        repertoire_id: String,
    },
    Tactic {
        membership: Option<TacticPolicyFacts>,
    },
}

/// Current authoritative facts needed to check a candidate's content.
/// Learning owns schedule and obligation checks. Queue owns ordering.
#[derive(Debug, PartialEq, Eq)]
pub struct StudyContentFacts {
    pub target: TargetId,
    pub version: ContentVersion,
    pub family: StudyContentFamily,
    pub status: ContentStatus,
    pub suspended: bool,
    pub excluded: bool,
    pub qualification_held: bool,
}

/// The caller selects lesson access from the issued activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudyLessonRead {
    Guided,
    Omit,
}

/// The exact current answer plus the presentation facts an issuer can pin.
/// Lesson data is read for guided work only.
#[derive(Debug, PartialEq, Eq)]
pub struct CurrentStudyDefinition {
    pub exercise: Exercise<ChessDefinition>,
    pub repertoire_label: Option<String>,
    pub lesson: Option<RepertoireLesson>,
}

/// The exact retained answer. Older attempts may rely on its archived lesson
/// when they were issued without a pinned lesson. Blind work omits it.
#[derive(Debug, PartialEq, Eq)]
pub struct IssuedStudyDefinition {
    pub exercise: Exercise<ChessDefinition>,
    pub legacy_lesson: Option<RepertoireLesson>,
}

/// Addressed content reads for Study. Each operation reads at most one content
/// row. A missing address returns `None`; unreadable required data is an error.
pub trait StudyContentRead {
    /// Project facts for one current target without decoding its definition.
    fn study_content_facts(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
    ) -> Result<Option<StudyContentFacts>, StoreError>;

    /// Hydrate the requested version if it is still the current one.
    /// Availability is checked separately at issuance.
    fn current_study_definition(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
        lesson: StudyLessonRead,
    ) -> Result<Option<CurrentStudyDefinition>, StoreError>;

    /// Resolve the original issued version, independent of current content.
    fn issued_study_definition(
        &self,
        dataset: &DatasetId,
        target: &TargetId,
        version: ContentVersion,
        lesson: StudyLessonRead,
    ) -> Result<Option<IssuedStudyDefinition>, StoreError>;
}
