//! Bounded metadata reads for the local tactics collection.
use serde::{Deserialize, Serialize};
use tempo_types::{StudyDay, TargetId};

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TacticFilter {
    #[default]
    All,
    Introduced,
    Unintroduced,
    Due,
}

impl TacticFilter {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Introduced => "introduced",
            Self::Unintroduced => "unintroduced",
            Self::Due => "due",
        }
    }
}

pub struct TacticPageQuery<'a> {
    pub search: &'a str,
    pub after: Option<&'a TargetId>,
    pub filter: TacticFilter,
    pub day: StudyDay,
}

pub const TACTIC_PAGE_SIZE: usize = 30;

#[derive(Debug, Serialize)]
pub struct TacticPage {
    pub rows: Vec<TacticListRow>,
    pub next: Option<TargetId>,
}

#[derive(Debug, Serialize)]
pub struct TacticListRow {
    pub target: TargetId,
    pub label: String,
    pub introduced: bool,
    pub due: bool,
    pub removed_from_study: bool,
}
