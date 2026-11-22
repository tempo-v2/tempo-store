//! Store values for the Games coverage report and backlog.
use serde::{Deserialize, Serialize};
use tempo_chess::OpponentGap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CoverageSummary {
    #[serde(default)]
    pub starting_line: CoverageLineSetting,
    #[serde(default)]
    pub starting_san: Vec<String>,
    pub request_id: String,
    pub through_move: u8,
    pub target_percent: u8,
    pub covered: f64,
    pub unresolved: f64,
    pub positions: usize,
    pub explorer_positions: usize,
    pub maia_positions: usize,
    pub scanned_at_ms: u64,
}

pub struct ForecastGap {
    pub starting_line: CoverageLineSetting,
    pub gap: OpponentGap,
    pub probability: f64,
    pub report_id: String,
    pub through_move: u8,
    pub stale: bool,
}

/// The durable report and whether the repertoire input has since changed.
pub struct SavedCoverage {
    pub summary: CoverageSummary,
    pub stale: bool,
}

/// Exact moves and revision of the saved per-repertoire coverage input.
/// Revision zero describes the original initial-position scan contract.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CoverageLineSetting {
    pub revision: u64,
    pub moves: Vec<String>,
}
