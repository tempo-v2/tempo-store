//! ContentHistory and its usage scenarios.

use serde::{Deserialize, Serialize};
use tempo_chess::ContentItem;
use tempo_types::DatasetId;

/// A retained content definition/version associated with its dataset.
///
/// # When to use
///
/// Store records an entry when first seeing a `(dataset, target, content version)`
/// combination. The application can use it to interpret an old attempt or
/// assessment after a semantic content edit or restore.
///
/// This is a version archive, not a log of every metadata edit: changing a label
/// without changing the version does not append another entry. Current labels and
/// provenance are read from the snapshot's current catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentHistory {
    pub dataset: DatasetId,
    pub item: ContentItem,
}

#[cfg(test)]
mod tests {
    use crate::test_support::store;
    use crate::{Commit, Store, Write};
    use tempo_chess::{demo_catalog, plan_import};
    use tempo_learning::LearningState;
    use tempo_types::TargetId;

    #[test]
    fn semantic_edits_archive_both_definitions_while_metadata_edits_do_not_add_versions() {
        let mut store = store(demo_catalog());
        let before = store.snapshot().unwrap();
        let target = TargetId::from("r1:p1");
        let old_item = before.catalog.items[&target].clone();
        let mut incoming = before.catalog.clone();
        incoming
            .items
            .get_mut(&target)
            .unwrap()
            .definition
            .decisions[0]
            .expected = "d2d4".into();
        let plan = plan_import(&before.catalog, &incoming).unwrap();
        store
            .commit(Commit {
                expected_revision: before.revision,
                expected_dataset: before.dataset,
                writes: vec![
                    Write::Catalog(plan.catalog),
                    Write::Learning {
                        target: target.clone(),
                        state: LearningState::default(),
                    },
                ],
            })
            .unwrap();
        let edited = store.snapshot().unwrap();
        let archived = edited
            .content_history
            .iter()
            .filter(|row| row.item.target == target)
            .collect::<Vec<_>>();
        assert_eq!(archived.len(), 2);
        assert_eq!(archived[0].item, old_item);
        assert_eq!(
            archived[1].item.definition.decisions[0].expected.uci,
            "d2d4"
        );
        assert!(archived[1].item.version > archived[0].item.version);
        let mut metadata = edited.catalog.clone();
        metadata.items.get_mut(&target).unwrap().label = "An updated description".into();
        store
            .commit(Commit {
                expected_revision: edited.revision,
                expected_dataset: edited.dataset,
                writes: vec![Write::Catalog(metadata)],
            })
            .unwrap();
        let after = store.snapshot().unwrap();
        assert_eq!(after.content_history, edited.content_history);
        assert_eq!(after.catalog.items[&target].label, "An updated description");
    }
}
