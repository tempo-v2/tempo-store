//! Project just the metadata for a stable keyset page.
use super::{SqliteStore, database_error};
use crate::{StoreError, TACTIC_PAGE_SIZE, TacticListRow, TacticPage, TacticPageQuery};
use rusqlite::params;
use tempo_types::TargetId;

impl SqliteStore {
    pub(super) fn read_tactic_page(
        &self,
        query: TacticPageQuery<'_>,
    ) -> Result<TacticPage, StoreError> {
        let _diagnostic = crate::diagnostics::Operation::new("sqlite.tactic_page.read_tactic_page");
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT c.target_id, json_extract(c.item, '$.label') AS label,
                json_extract(l.state, '$.first_introduced') IS NOT NULL AS introduced,
                (json_extract(c.item, '$.status') = 'active'
                    AND COALESCE(json_extract(l.state, '$.due') <= ?3, 0)) AS due
             FROM current_content c JOIN learning l USING (target_id)
             WHERE c.repertoire IS NULL AND c.target_id > ?1
                AND c.dataset_id = (SELECT dataset_id FROM store_state WHERE singleton=1)
                AND (?2 = '' OR instr(lower(label), lower(?2)) > 0
                    OR EXISTS (SELECT 1 FROM json_each(c.item, '$.provenance')
                        WHERE instr(lower(value), lower(?2)) > 0))
                AND (?4 = 'all' OR (?4 = 'introduced' AND introduced)
                     OR (?4 = 'unintroduced' AND NOT introduced) OR (?4 = 'due' AND due))
             ORDER BY c.target_id LIMIT ?5",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(
                params![
                    query.after.map_or("", |id| id.0.as_str()),
                    query.search,
                    query.day.0,
                    query.filter.key(),
                    TACTIC_PAGE_SIZE + 1
                ],
                |row| {
                    Ok(TacticListRow {
                        target: TargetId(row.get(0)?),
                        label: row.get(1)?,
                        introduced: row.get(2)?,
                        due: row.get(3)?,
                    })
                },
            )
            .map_err(database_error)?;
        let mut rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?;
        let next = if rows.len() > TACTIC_PAGE_SIZE {
            rows.pop();
            rows.last().map(|row| row.target.clone())
        } else {
            None
        };
        Ok(TacticPage { rows, next })
    }
}
