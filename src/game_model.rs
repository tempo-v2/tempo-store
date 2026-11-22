//! Durable model input and projection records. The application computes semantic keys.
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use tempo_chess::{Catalog, ChessDefinition, ContentKind};
use tempo_types::{ContentVersion, DatasetId, TargetId};

/// Projection identity is independent of the raw Maia inference cache.
pub const REACH_MODEL_KEY: &str = "maia2-0.11.0:65aae8465eed5e65df66a24ea7370715579f9e5435098d06fe18bdb1e267e997:rapid:1500:1500:cpu:first-arrival-v2";

pub type ModelTargets = BTreeMap<TargetId, (ContentVersion, ChessDefinition)>;

/// Owner-verified semantic input for the final accepted active target facts.
pub struct ModelInputStamp {
    pub key: String,
    pub targets: ModelTargets,
}

pub fn model_targets(catalog: &Catalog, repertoire_id: &str) -> ModelTargets {
    catalog
        .items
        .values()
        .filter(|item| {
            item.active()
                && matches!(&item.definition.kind,
        ContentKind::Repertoire { repertoire, .. } if repertoire == repertoire_id)
        })
        .map(|item| (item.target.clone(), (item.version, item.definition.clone())))
        .collect()
}

/// Opaque authored route identities and their ordered learner targets.
pub fn model_lines<'a>(catalog: &'a Catalog, id: &str) -> BTreeSet<(&'a str, &'a [TargetId])> {
    catalog
        .lines
        .iter()
        .filter(|line| line.repertoire == id)
        .map(|line| (line.id.as_str(), line.targets.as_slice()))
        .collect()
}

pub struct ProjectionBook {
    pub repertoire_id: String,
    pub pgn: String,
    pub input_key: String,
    pub targets: ModelTargets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionScope {
    /// Verified books without a complete projection for their current input and algorithm.
    MissingOrPartial,
    /// Every verified book, regardless of projection state.
    AllVerified,
}

pub struct BookProjection<'a> {
    pub repertoire_id: &'a str,
    pub input_key: &'a str,
    pub model: &'a str,
    pub complete: bool,
    pub reach: &'a BTreeMap<TargetId, u32>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ProjectionStatus {
    pub dataset_id: DatasetId,
    pub complete_current: usize,
    pub partial_current: usize,
    pub unavailable: usize,
    pub pending: usize,
}
