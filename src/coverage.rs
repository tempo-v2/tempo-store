//! Store values for the Games coverage report and backlog.
use serde::{Deserialize, Serialize};
use tempo_chess::OpponentGap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CoverageSummary {
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
    pub gap: OpponentGap,
    pub probability: f64,
    pub stale: bool,
}

/// The durable report and whether the repertoire input has since changed.
pub struct SavedCoverage {
    pub summary: CoverageSummary,
    pub stale: bool,
}
