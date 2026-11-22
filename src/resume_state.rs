//! Addressed reads for explicit Resume and the active attempt in Next.

use crate::{StoreError, StoredAttempt};
use tempo_chess::ContentStatus;
use tempo_types::{AttemptId, ContentVersion, DatasetId};

/// Current compatibility facts for one issued attempt. This read carries no
/// Learning state, assessment history, receipts, or selection capability.
pub trait AddressedResume {
    fn resume_state(&self, id: &AttemptId) -> Result<ResumeState, StoreError>;
    /// Read the active pointer, its issued attempt and current compatibility together.
    fn active_resume_state(&self) -> Result<ResumeState, StoreError>;
}

pub struct ResumeState {
    pub dataset: DatasetId,
    pub active_attempt: Option<AttemptId>,
    pub record: Option<StoredAttempt>,
    pub target: Option<ResumeTarget>,
}

pub struct ResumeTarget {
    pub version: ContentVersion,
    pub status: ContentStatus,
    pub excluded: bool,
    pub suspended: bool,
}
