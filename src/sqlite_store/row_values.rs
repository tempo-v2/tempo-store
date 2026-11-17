//! Convert row payloads without changing the public domain or wire types.

use super::database_error;
use crate::{StoreError, store_error::invalid};
use rusqlite::{Row, Transaction};
use serde::{Serialize, de::DeserializeOwned};

pub(super) fn encode(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| invalid(&format!("encode SQLite value: {error}")))
}

pub(super) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|error| invalid(&format!("decode SQLite value: {error}")))
}

pub(super) fn decode_counter(value: Vec<u8>) -> Result<u64, StoreError> {
    let bytes = value
        .try_into()
        .map_err(|_| invalid("SQLite counter must contain eight bytes"))?;
    Ok(u64::from_be_bytes(bytes))
}

pub(super) fn row_index(value: usize) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| invalid("SQLite row order exceeds capacity"))
}

pub(super) fn read_rows<T, C: FromIterator<T>>(
    transaction: &Transaction<'_>,
    sql: &str,
    read: impl FnMut(&Row<'_>) -> Result<T, StoreError>,
) -> Result<C, StoreError> {
    let mut statement = transaction.prepare(sql).map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    let mut read = read;
    std::iter::from_fn(|| {
        rows.next()
            .transpose()
            .map(|row| row.map_err(database_error).and_then(&mut read))
    })
    .collect()
}

pub(super) fn text_value<T: DeserializeOwned>(
    row: &Row<'_>,
    column: usize,
) -> Result<T, StoreError> {
    decode(&row.get::<_, String>(column).map_err(database_error)?)
}
