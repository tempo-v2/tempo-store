//! Address and outcomes for changing a repertoire's participation.

use crate::ChessAttempt;
use serde::{Deserialize, Serialize};
use tempo_types::{DatasetId, Revision};

/// Dataset-local participation in Daily Study and current game comparisons.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepertoireActivity {
    #[default]
    Active,
    Suspended,
}

/// The immutable command identity and desired participation.
pub struct RepertoireActivityCommand<'a> {
    pub dataset: &'a DatasetId,
    pub repertoire: &'a str,
    pub command_id: &'a str,
    pub activity: RepertoireActivity,
}

pub enum RepertoireActivityResult {
    Applied,
    StaleDataset,
    UnknownRepertoire,
    CommandConflict,
}

/// A settled delivery or the current basis for Tempo to prepare a write.
/// An attempt is included only if it is current and belongs to this repertoire.
pub enum RepertoireActivityRead {
    Settled(RepertoireActivityResult),
    Pending {
        revision: Revision,
        active: Option<Box<ChessAttempt>>,
    },
}
