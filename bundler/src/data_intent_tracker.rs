//! Data intent is a request from the user to get data bundled on a blob.
//!
//! # Cost
//!
//! Data intents have a max specified cost by the user. Data intents not included in blocks are
//! charged at their max cost. After block inclusion, they are charged at the cost of that block's
//! gas parameters.
//!
//! # Canonical storage
//!
//! There are two canonical sources of data for a blob share instance:
//! - Blockchain state of the target network (e.g. Ethereum mainnet)
//! - MySQL DB holding pending data (non-included data intents, non-included blob transactions)
//!
//! Data intents are stored in the data_intents table. A blob share instance may pack a number of
//! data intent items not yet included into a blob transaction. If the blob transaction is latter
//! replaced due to underpriced gas, those data intents may be re-included into a new transaction.
//!
//! Underpriced data intents will remain in the internal blob share pool consuming user balance. A
//! user can cancel data intents by ID at any point. Stale underpriced intents are automatically
//! evicted by the `evict_stale_intents_task` background task (configurable via
//! `--evict-stale-intent-hours`).
//!
//! # Cache
//!
//! A blob share instance must run the packing algorithm against all pending data intent summaries.
//! Quering the DB on each packing iteration may become a bottleneck. Thus keep a local in-memory
//! cache of data intent summaries. Before each packing run check if there are new records after
//! the last fetched timestamp.
//!

use bundler_client::types::{DataIntentFull, DataIntentId, DataIntentSummary};
use chrono::{DateTime, Utc};
use ethers::types::{Address, TxHash};
use eyre::{eyre, Context, Result};
use futures::StreamExt;
use num_traits::cast::FromPrimitive;
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use sqlx::{types::BigDecimal, FromRow, MySql, MySqlPool, QueryBuilder};
use std::{cmp, collections::HashMap};
use uuid::Uuid;

use crate::{
    metrics,
    utils::{address_from_vec, option_hex_vec, txhash_from_vec},
    BlobTxSummary, DataIntent,
};

#[derive(Default)]
pub struct DataIntentTracker {
    // DateTime default = NaiveDateTime default = timestamp(0)
    last_sync_table_data_intents: DateTime<Utc>,
    pending_intents: HashMap<DataIntentId, DataIntentSummary>,
    included_intents: HashMap<TxHash, Vec<DataIntentId>>,
    // An intent may be temporarily included in multiple transactions. The goal is to have a single
    // canonical inclusion, but tx repricing can result in > 1 inclusion.
    cache_intent_inclusions: HashMap<DataIntentId, Vec<TxHash>>,
}

// Pruning of finalized items is handled by `prune_finalized_intent_data` in app.rs
impl DataIntentTracker {
    pub fn collect_metrics(&self) {
        metrics::PENDING_INTENTS_CACHE.set(self.pending_intents.len() as f64);
        metrics::INCLUDED_INTENTS_CACHE.set(self.included_intents.len() as f64);
    }

    pub async fn sync_with_db(&mut self, db_pool: &MySqlPool) -> Result<usize> {
        let count_prev = self.pending_intents.len();
        let from = self.last_sync_table_data_intents;
        let to: DateTime<Utc> = Utc::now();

        let mut stream = sqlx::query(
            r#"
SELECT id, eth_address, data_len, data_hash, max_blob_gas_price, data_hash_signature, updated_at, inclusion_finalized
FROM data_intents
WHERE (inclusion_finalized = FALSE) AND (cancelled = FALSE) AND (updated_at BETWEEN ? AND ?)
ORDER BY updated_at ASC
        "#,
        )
        .bind(from)
        .bind(to)
        .fetch(db_pool);

        while let Some(row) = stream.next().await {
            let data_intent = DataIntentDbRowSummary::from_row(&row?)?;

            let updated_at = data_intent.updated_at;
            self.pending_intents
                .insert(data_intent.id, data_intent.try_into()?);
            self.last_sync_table_data_intents =
                cmp::max(self.last_sync_table_data_intents, updated_at);
        }

        Ok(self.pending_intents.len() - count_prev)
    }

    pub fn get_all_intents(&self) -> Vec<(&DataIntentSummary, Option<TxHash>, usize)> {
        self.pending_intents
            .values()
            .map(|item| {
                let tx_hashes = self.cache_intent_inclusions.get(&item.id);

                (
                    item,
                    tx_hashes.and_then(|tx_hashes| tx_hashes.last()).copied(),
                    tx_hashes.map(|tx_hashes| tx_hashes.len()).unwrap_or(0),
                )
            })
            .collect()
    }

    /// Returns the total sum of pending itents cost from `from`.
    pub fn non_included_intents_total_cost(&self, from: &Address) -> u128 {
        self.pending_intents
            .values()
            .map(|intent| {
                if &intent.from == from && !self.cache_intent_inclusions.contains_key(&intent.id) {
                    intent.max_cost()
                } else {
                    0
                }
            })
            .sum()
    }

    /// Returns the total sum of pending itents total length.
    pub fn non_included_intents_total_data_len(&self, from: &Address) -> usize {
        self.pending_intents
            .values()
            .map(|intent| {
                if &intent.from == from && !self.cache_intent_inclusions.contains_key(&intent.id) {
                    intent.data_len
                } else {
                    0
                }
            })
            .sum()
    }

    /// Drop inclusion records for an excluded (repriced) transaction. The intents themselves
    /// remain pending so they can be re-included in another transaction.
    pub fn drop_excluded_tx(&mut self, tx_hash: TxHash) {
        if let Some(ids) = self.included_intents.remove(&tx_hash) {
            for id in &ids {
                if let Some(tx_hashes) = self.cache_intent_inclusions.get_mut(id) {
                    tx_hashes.retain(|h| h != &tx_hash);
                    if tx_hashes.is_empty() {
                        self.cache_intent_inclusions.remove(id);
                    }
                }
            }
        }
    }

    /// Drops all intents associated with transaction. Returns the finalized intent IDs
    /// so the caller can mark them as finalized in the database.
    pub fn finalize_tx(&mut self, tx_hash: TxHash) -> Vec<DataIntentId> {
        if let Some(ids) = self.included_intents.remove(&tx_hash) {
            for id in &ids {
                self.pending_intents.remove(id);
                self.cache_intent_inclusions.remove(id);
            }
            ids
        } else {
            vec![]
        }
    }

    /// Returns true if the intent has been included in any transaction (pending or confirmed).
    pub fn is_intent_included(&self, id: &DataIntentId) -> bool {
        self.cache_intent_inclusions.contains_key(id)
    }

    /// Remove a pending intent from the in-memory tracker. Returns the removed summary if found.
    /// Only removes from `pending_intents`; the caller should ensure the intent is not included
    /// in any transaction before calling this.
    pub fn remove_pending_intent(&mut self, id: &DataIntentId) -> Option<DataIntentSummary> {
        self.pending_intents.remove(id)
    }

    /// Returns IDs of pending intents that are stale (older than `max_age`) and underpriced
    /// (max_blob_gas_price below `current_blob_gas_price`). Only considers intents that are
    /// not currently included in any transaction.
    pub fn find_stale_underpriced_intents(
        &self,
        current_blob_gas_price: u64,
        max_age: chrono::Duration,
    ) -> Vec<DataIntentId> {
        let cutoff = Utc::now() - max_age;
        self.pending_intents
            .values()
            .filter(|intent| {
                !self.cache_intent_inclusions.contains_key(&intent.id)
                    && intent.updated_at < cutoff
                    && intent.max_blob_gas_price < current_blob_gas_price
            })
            .map(|intent| intent.id)
            .collect()
    }

    pub async fn insert_many_intent_tx_inclusions(
        &mut self,
        db_pool: &MySqlPool,
        data_intent_ids: &[DataIntentId],
        blob_tx: &BlobTxSummary,
    ) -> Result<()> {
        insert_many_intent_tx_inclusions(db_pool, data_intent_ids, blob_tx).await?;

        // Include in-memory cache after successful DB update
        for id in data_intent_ids {
            self.cache_intent_inclusions
                .entry(*id)
                .or_default()
                .push(blob_tx.tx_hash);
        }

        // Populate reverse mapping (tx_hash -> intent IDs) for finalize_tx lookups
        self.included_intents
            .entry(blob_tx.tx_hash)
            .or_default()
            .extend(data_intent_ids.iter().copied());

        Ok(())
    }
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct DataIntentDbRowFull {
    pub id: Uuid,
    #[serde(with = "hex_vec")]
    pub eth_address: Vec<u8>, // BINARY(20)
    #[serde(with = "option_hex_vec")]
    pub data: Option<Vec<u8>>, // MEDIUMBLOB, NULL after pruning
    pub data_len: u32, // INT
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>, // BINARY(32)
    pub max_blob_gas_price: u64, // BIGINT
    #[serde(with = "option_hex_vec")]
    pub data_hash_signature: Option<Vec<u8>>, // BINARY(65), Optional
    pub updated_at: DateTime<Utc>, // TIMESTAMP(3)
}

#[derive(Debug, FromRow, Serialize)]
pub struct DataIntentDbRowSummary {
    pub id: Uuid,
    pub eth_address: Vec<u8>,                 // BINARY(20)
    pub data_len: u32,                        // INT
    pub data_hash: Vec<u8>,                   // BINARY(32)
    pub max_blob_gas_price: u64,              // BIGINT
    pub data_hash_signature: Option<Vec<u8>>, // BINARY(65), Optional
    pub updated_at: DateTime<Utc>,            // TIMESTAMP(3)
}

pub(crate) async fn fetch_data_intent_db_full(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<DataIntentFull> {
    let data_intent = sqlx::query_as::<_, DataIntentDbRowFull>(
        r#"
SELECT id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, updated_at
FROM data_intents
WHERE id = ?
        "#)
        .bind(id)
        .fetch_one(db_pool)
        .await?;

    Ok(DataIntentFull {
        id: data_intent.id,
        eth_address: data_intent.eth_address,
        data: data_intent.data.unwrap_or_default(),
        data_len: data_intent.data_len,
        data_hash: data_intent.data_hash,
        max_blob_gas_price: data_intent.max_blob_gas_price,
        data_hash_signature: data_intent.data_hash_signature,
        updated_at: data_intent.updated_at,
    })
}

pub(crate) async fn fetch_many_data_intent_db_full(
    db_pool: &MySqlPool,
    ids: &[Uuid],
) -> Result<Vec<DataIntentDbRowFull>> {
    let mut query_builder: QueryBuilder<MySql> = QueryBuilder::new(
        r#"
SELECT id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, updated_at
FROM data_intents
WHERE id in
    "#,
    );

    // TODO: limit the amount of ids to not reach a limit
    // TODO: try to use different API than `.push_tuples` since you only query by id
    query_builder.push_tuples(ids.iter(), |mut b, id| {
        b.push_bind(id);
    });

    let rows = query_builder.build().fetch_all(db_pool).await?;

    rows.iter()
        .map(|row| DataIntentDbRowFull::from_row(row).wrap_err("error decoding data_intent DB row"))
        .collect::<Result<Vec<_>>>()
}

pub(crate) async fn fetch_data_intent_db_is_known(db_pool: &MySqlPool, id: &Uuid) -> Result<bool> {
    let row = sqlx::query!(
        r#"
SELECT id 
FROM data_intents
WHERE id = ?
        "#,
        id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(row.is_some())
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug)]
pub(crate) struct IntentInclusion {
    pub tx_hash: TxHash,
    pub nonce: u64,
    pub updated_at: DateTime<Utc>,
}

pub(crate) async fn fetch_data_intent_inclusion(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<Vec<IntentInclusion>> {
    let rows = sqlx::query!(
        r#"
SELECT id, tx_hash, sender_address, nonce, updated_at
FROM intent_inclusions
WHERE id = ?
ORDER BY nonce ASC, updated_at ASC
        "#,
        id
    )
    .fetch_all(db_pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(IntentInclusion {
                tx_hash: txhash_from_vec(&row.tx_hash)?,
                //  address_from_vec(&row.sender_address)?,
                nonce: row.nonce as u64,
                updated_at: row
                    .updated_at
                    .ok_or_else(|| eyre!("no updated_at column"))?,
            })
        })
        .collect::<Result<Vec<_>>>()
}

/// Store data intent to SQL DB
pub(crate) async fn store_data_intent<'c>(
    db_tx: &mut sqlx::Transaction<'c, sqlx::MySql>,
    data_intent: DataIntent,
) -> Result<DataIntentId> {
    let id = Uuid::new_v4();
    let eth_address = data_intent.from().to_fixed_bytes().to_vec();
    let data = data_intent.data();
    let data_len = data_intent.data_len() as u32;
    let data_hash = data_intent.data_hash().to_vec();
    let max_blob_gas_price = BigDecimal::from_u64(data_intent.max_blob_gas_price());
    let data_hash_signature = data_intent.data_hash_signature().map(|sig| sig.to_vec());

    // Persist data request
    sqlx::query!(
            r#"
INSERT INTO data_intents (id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature)
VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
            id,
            eth_address,
            data,
            data_len,
            data_hash,
            max_blob_gas_price,
            data_hash_signature
        )
        .execute(&mut **db_tx)
        .await?;

    // TODO: Prevent inserting duplicates

    //        match self.pending_intents.get(&id) {
    //            None => {}                          // Ok insert
    //            // TODO: Handle bumping the registered max price
    //            Some(DataIntentItem::Pending(_)) | Some(DataIntentItem::Included(_, _)) => {
    //                bail!("data intent {id} already known")
    //            }
    //        };

    Ok(id)
}

pub(crate) async fn fetch_all_intents_with_inclusion_not_finalized(
    db_pool: &MySqlPool,
) -> Result<Vec<(DataIntentId, TxHash)>> {
    let rows = sqlx::query!(
        r#"
SELECT data_intents.id, intent_inclusions.tx_hash
FROM data_intents
INNER JOIN intent_inclusions ON data_intents.id = intent_inclusions.id
WHERE data_intents.inclusion_finalized = FALSE;
"#
    )
    .fetch_all(db_pool)
    .await?;

    rows.iter()
        .map(|row| Ok((Uuid::from_slice(&row.id)?, txhash_from_vec(&row.tx_hash)?)))
        .collect::<Result<Vec<_>>>()
}

pub(crate) async fn mark_data_intents_as_inclusion_finalized(
    db_pool: &MySqlPool,
    ids: &[Uuid],
) -> Result<()> {
    // Query builder below does not handle empty ids slice well
    if ids.is_empty() {
        return Ok(());
    }

    // Bulk fetch all rows
    let mut query_builder: QueryBuilder<MySql> = QueryBuilder::new(
        r#"
UPDATE data_intents
SET inclusion_finalized = TRUE
WHERE id IN
    "#,
    );

    // TODO: limit the amount of ids to not reach a limit
    // TODO: try to use different API than `.push_tuples` since you only query by id
    query_builder
        .push_tuples(ids.iter(), |mut b, id| {
            b.push_bind(id);
        })
        .build()
        .fetch_all(db_pool)
        .await?;

    Ok(())
}

/// Mark a data intent as cancelled in the database.
pub(crate) async fn mark_data_intent_cancelled(db_pool: &MySqlPool, id: &Uuid) -> Result<()> {
    sqlx::query("UPDATE data_intents SET cancelled = TRUE WHERE id = ?")
        .bind(id)
        .execute(db_pool)
        .await?;
    Ok(())
}

/// Fetch the owner address of a data intent. Returns None if the intent does not exist.
pub(crate) async fn fetch_data_intent_owner(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<Option<Address>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("SELECT eth_address FROM data_intents WHERE id = ?")
            .bind(id)
            .fetch_optional(db_pool)
            .await?;

    match row {
        Some((eth_address,)) => Ok(Some(address_from_vec(&eth_address)?)),
        None => Ok(None),
    }
}

/// Check if a data intent is already cancelled in the database.
pub(crate) async fn fetch_data_intent_is_cancelled(db_pool: &MySqlPool, id: &Uuid) -> Result<bool> {
    let row: Option<(bool,)> = sqlx::query_as("SELECT cancelled FROM data_intents WHERE id = ?")
        .bind(id)
        .fetch_optional(db_pool)
        .await?;

    Ok(row.map(|(cancelled,)| cancelled).unwrap_or(false))
}

/// Delete all intent_inclusions rows for a given transaction hash.
/// Called when a repriced transaction is excluded during finalization.
pub(crate) async fn delete_intent_inclusions_by_tx_hash(
    db_pool: &MySqlPool,
    tx_hash: TxHash,
) -> Result<()> {
    let tx_hash_bytes = tx_hash.to_fixed_bytes().to_vec();
    sqlx::query("DELETE FROM intent_inclusions WHERE tx_hash = ?")
        .bind(tx_hash_bytes)
        .execute(db_pool)
        .await?;
    Ok(())
}

/// Mark finalized data intents with their finalization block number.
/// Called during finalization so we can later prune intents that are old enough.
pub(crate) async fn set_finalized_block_number(
    db_pool: &MySqlPool,
    ids: &[Uuid],
    block_number: u64,
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let block_number = block_number as u32;
    let mut query_builder: QueryBuilder<MySql> =
        QueryBuilder::new("UPDATE data_intents SET finalized_block_number = ");
    query_builder.push_bind(block_number);
    query_builder.push(" WHERE id IN ");
    query_builder.push_tuples(ids.iter(), |mut b, id| {
        b.push_bind(id);
    });
    query_builder.build().execute(db_pool).await?;

    Ok(())
}

/// Prune raw data from finalized intents that are old enough.
/// Sets `data = NULL` for intents where `inclusion_finalized = TRUE`
/// and `finalized_block_number + prune_after_blocks <= current_anchor_block_number`.
/// Returns the number of pruned rows.
pub(crate) async fn prune_finalized_intent_data(
    db_pool: &MySqlPool,
    current_anchor_block_number: u64,
    prune_after_blocks: u64,
) -> Result<u64> {
    // Only prune if the anchor is far enough ahead
    let threshold = current_anchor_block_number.saturating_sub(prune_after_blocks) as u32;

    let result = sqlx::query(
        r#"
UPDATE data_intents
SET data = NULL
WHERE inclusion_finalized = TRUE
  AND data IS NOT NULL
  AND finalized_block_number IS NOT NULL
  AND finalized_block_number <= ?
        "#,
    )
    .bind(threshold)
    .execute(db_pool)
    .await?;

    Ok(result.rows_affected())
}

/// Row returned by the history query: intent metadata + optional inclusion tx_hash.
#[derive(Debug, FromRow)]
pub(crate) struct HistoryDbRow {
    pub id: Uuid,
    pub data_len: u32,
    pub data_hash: Vec<u8>,
    pub max_blob_gas_price: u64,
    pub cancelled: bool,
    pub inclusion_finalized: bool,
    pub updated_at: DateTime<Utc>,
    pub tx_hash: Option<Vec<u8>>,
}

/// Fetch paginated history for an address. Returns rows ordered by `updated_at DESC`.
/// Each intent appears once; the most-recent inclusion tx_hash (if any) is joined via a
/// lateral / correlated subquery so the result set stays one-row-per-intent.
pub(crate) async fn fetch_history_for_address(
    db_pool: &MySqlPool,
    address: &[u8],
    limit: u32,
    offset: u32,
) -> Result<Vec<HistoryDbRow>> {
    let rows = sqlx::query_as::<_, HistoryDbRow>(
        r#"
SELECT
    di.id,
    di.data_len,
    di.data_hash,
    di.max_blob_gas_price,
    di.cancelled,
    di.inclusion_finalized,
    di.updated_at,
    (
        SELECT ii.tx_hash
        FROM intent_inclusions ii
        WHERE ii.id = di.id
        ORDER BY ii.updated_at DESC
        LIMIT 1
    ) AS tx_hash
FROM data_intents di
WHERE di.eth_address = ?
ORDER BY di.updated_at DESC
LIMIT ? OFFSET ?
        "#,
    )
    .bind(address)
    .bind(limit)
    .bind(offset)
    .fetch_all(db_pool)
    .await?;

    Ok(rows)
}

/// Count total intents for an address (for pagination metadata).
pub(crate) async fn count_history_for_address(db_pool: &MySqlPool, address: &[u8]) -> Result<u64> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM data_intents WHERE eth_address = ?")
        .bind(address)
        .fetch_one(db_pool)
        .await?;
    Ok(row.0 as u64)
}

// Private fn to ensure data consistency with local cache
async fn insert_many_intent_tx_inclusions(
    db_pool: &MySqlPool,
    ids: &[Uuid],
    blob_tx: &BlobTxSummary,
) -> Result<()> {
    let mut query_builder =
        QueryBuilder::new("INSERT INTO intent_inclusions (id, tx_hash, sender_address, nonce) ");

    query_builder.push_values(ids, |mut b, id| {
        b.push_bind(id)
            .push_bind(blob_tx.tx_hash.to_fixed_bytes().to_vec())
            .push_bind(blob_tx.from.to_fixed_bytes().to_vec())
            .push_bind(blob_tx.nonce);
    });

    let query = query_builder.build();

    query.execute(db_pool).await?;

    Ok(())
}

impl TryFrom<DataIntentDbRowSummary> for DataIntentSummary {
    type Error = eyre::Report;

    fn try_from(value: DataIntentDbRowSummary) -> Result<Self, Self::Error> {
        Ok(DataIntentSummary {
            id: value.id,
            from: address_from_vec(&value.eth_address)?,
            data_hash: value.data_hash,
            data_len: value.data_len.try_into()?,
            max_blob_gas_price: value.max_blob_gas_price,
            updated_at: value.updated_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bundler_client::types::DataIntentSummary;
    use chrono::{DateTime, Utc};
    use ethers::types::{Address, H256};
    use uuid::Uuid;

    use super::{DataIntentDbRowFull, DataIntentTracker};

    fn make_summary(id: Uuid) -> DataIntentSummary {
        DataIntentSummary {
            id,
            from: Address::random(),
            data_hash: vec![0xcc; 32],
            data_len: 100,
            max_blob_gas_price: 1000,
            updated_at: DateTime::from_str("2024-01-01T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn serde_data_intent_db_row_full() {
        let item = DataIntentDbRowFull {
            id: Uuid::from_str("1bcb4515-8c91-456c-a87d-7c4f5f3f0d9e").unwrap(),
            eth_address: vec![0xaa; 20],
            data: Some(vec![0xbb; 10]),
            data_len: 10,
            data_hash: vec![0xcc; 32],
            data_hash_signature: Some(vec![0xee; 32]),
            max_blob_gas_price: 100000000,
            updated_at: DateTime::from_str("2023-01-01T12:12:12.202889Z").unwrap(),
        };

        let expected_item_str = "{\"id\":\"1bcb4515-8c91-456c-a87d-7c4f5f3f0d9e\",\"eth_address\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"data\":\"0xbbbbbbbbbbbbbbbbbbbb\",\"data_len\":10,\"data_hash\":\"0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\",\"max_blob_gas_price\":100000000,\"data_hash_signature\":\"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\",\"updated_at\":\"2023-01-01T12:12:12.202889Z\"}";

        assert_eq!(serde_json::to_string(&item).unwrap(), expected_item_str);
        let item_recv: DataIntentDbRowFull = serde_json::from_str(expected_item_str).unwrap();
        // test eq of dedicated serde fiels with Option<Vec<u8>>
        assert_eq!(item_recv.data_hash_signature, item.data_hash_signature);
    }

    #[test]
    fn finalize_tx_returns_ids_and_cleans_caches() {
        let mut tracker = DataIntentTracker::default();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let tx_hash = H256::random();

        // Populate pending intents
        tracker.pending_intents.insert(id1, make_summary(id1));
        tracker.pending_intents.insert(id2, make_summary(id2));

        // Populate inclusion maps (as insert_many_intent_tx_inclusions would)
        tracker.included_intents.insert(tx_hash, vec![id1, id2]);
        tracker.cache_intent_inclusions.insert(id1, vec![tx_hash]);
        tracker.cache_intent_inclusions.insert(id2, vec![tx_hash]);

        let finalized = tracker.finalize_tx(tx_hash);

        assert_eq!(finalized.len(), 2);
        assert!(finalized.contains(&id1));
        assert!(finalized.contains(&id2));
        assert!(tracker.pending_intents.is_empty());
        assert!(tracker.included_intents.is_empty());
        assert!(tracker.cache_intent_inclusions.is_empty());
    }

    #[test]
    fn finalize_tx_unknown_hash_returns_empty() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        tracker.pending_intents.insert(id, make_summary(id));

        let finalized = tracker.finalize_tx(H256::random());

        assert!(finalized.is_empty());
        // Pending intents are untouched
        assert_eq!(tracker.pending_intents.len(), 1);
    }

    #[test]
    fn finalize_tx_only_removes_intents_in_that_tx() {
        let mut tracker = DataIntentTracker::default();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let tx_hash_a = H256::random();
        let tx_hash_b = H256::random();

        tracker.pending_intents.insert(id1, make_summary(id1));
        tracker.pending_intents.insert(id2, make_summary(id2));

        tracker.included_intents.insert(tx_hash_a, vec![id1]);
        tracker.included_intents.insert(tx_hash_b, vec![id2]);
        tracker.cache_intent_inclusions.insert(id1, vec![tx_hash_a]);
        tracker.cache_intent_inclusions.insert(id2, vec![tx_hash_b]);

        let finalized = tracker.finalize_tx(tx_hash_a);

        assert_eq!(finalized, vec![id1]);
        // id2 remains pending
        assert_eq!(tracker.pending_intents.len(), 1);
        assert!(tracker.pending_intents.contains_key(&id2));
        // tx_hash_b still tracked
        assert!(tracker.included_intents.contains_key(&tx_hash_b));
        assert!(tracker.cache_intent_inclusions.contains_key(&id2));
    }

    #[test]
    fn drop_excluded_tx_removes_inclusion_keeps_intents_pending() {
        let mut tracker = DataIntentTracker::default();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let tx_hash = H256::random();

        tracker.pending_intents.insert(id1, make_summary(id1));
        tracker.pending_intents.insert(id2, make_summary(id2));
        tracker.included_intents.insert(tx_hash, vec![id1, id2]);
        tracker.cache_intent_inclusions.insert(id1, vec![tx_hash]);
        tracker.cache_intent_inclusions.insert(id2, vec![tx_hash]);

        tracker.drop_excluded_tx(tx_hash);

        // Intents remain pending (they can be re-included in another tx)
        assert_eq!(tracker.pending_intents.len(), 2);
        assert!(tracker.pending_intents.contains_key(&id1));
        assert!(tracker.pending_intents.contains_key(&id2));
        // Inclusion mappings are cleaned up
        assert!(!tracker.included_intents.contains_key(&tx_hash));
        assert!(tracker.cache_intent_inclusions.is_empty());
    }

    #[test]
    fn drop_excluded_tx_preserves_other_inclusions() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        let tx_hash_old = H256::random();
        let tx_hash_new = H256::random();

        tracker.pending_intents.insert(id, make_summary(id));
        // Intent is included in both old and new tx (repricing scenario)
        tracker.included_intents.insert(tx_hash_old, vec![id]);
        tracker.included_intents.insert(tx_hash_new, vec![id]);
        tracker
            .cache_intent_inclusions
            .insert(id, vec![tx_hash_old, tx_hash_new]);

        // Drop only the old tx
        tracker.drop_excluded_tx(tx_hash_old);

        // Old tx inclusion removed
        assert!(!tracker.included_intents.contains_key(&tx_hash_old));
        // New tx inclusion preserved
        assert!(tracker.included_intents.contains_key(&tx_hash_new));
        // Intent still has the new tx in its cache
        assert_eq!(
            tracker.cache_intent_inclusions.get(&id).unwrap(),
            &vec![tx_hash_new]
        );
        // Intent still pending
        assert!(tracker.pending_intents.contains_key(&id));
    }

    #[test]
    fn drop_excluded_tx_unknown_hash_is_noop() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        let tx_hash = H256::random();
        let unknown_hash = H256::random();

        tracker.pending_intents.insert(id, make_summary(id));
        tracker.included_intents.insert(tx_hash, vec![id]);
        tracker.cache_intent_inclusions.insert(id, vec![tx_hash]);

        tracker.drop_excluded_tx(unknown_hash);

        // Everything unchanged
        assert_eq!(tracker.pending_intents.len(), 1);
        assert_eq!(tracker.included_intents.len(), 1);
        assert_eq!(tracker.cache_intent_inclusions.len(), 1);
    }

    #[test]
    fn finalize_tx_metrics_reflect_state() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        let tx_hash = H256::random();

        tracker.pending_intents.insert(id, make_summary(id));
        tracker.included_intents.insert(tx_hash, vec![id]);
        tracker.cache_intent_inclusions.insert(id, vec![tx_hash]);

        // Before finalization: 1 pending, 1 included
        assert_eq!(tracker.pending_intents.len(), 1);
        assert_eq!(tracker.included_intents.len(), 1);

        tracker.finalize_tx(tx_hash);

        // After: empty
        assert_eq!(tracker.pending_intents.len(), 0);
        assert_eq!(tracker.included_intents.len(), 0);
        assert_eq!(tracker.cache_intent_inclusions.len(), 0);
    }

    #[test]
    fn remove_pending_intent_returns_summary() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        let summary = make_summary(id);
        let from = summary.from;
        tracker.pending_intents.insert(id, summary);

        let removed = tracker.remove_pending_intent(&id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().from, from);
        assert!(tracker.pending_intents.is_empty());
    }

    #[test]
    fn remove_pending_intent_unknown_returns_none() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        tracker.pending_intents.insert(id, make_summary(id));

        let removed = tracker.remove_pending_intent(&Uuid::new_v4());
        assert!(removed.is_none());
        // Original intent untouched
        assert_eq!(tracker.pending_intents.len(), 1);
    }

    #[test]
    fn is_intent_included_returns_correct_state() {
        let mut tracker = DataIntentTracker::default();
        let id_included = Uuid::new_v4();
        let id_pending = Uuid::new_v4();
        let tx_hash = H256::random();

        tracker
            .pending_intents
            .insert(id_included, make_summary(id_included));
        tracker
            .pending_intents
            .insert(id_pending, make_summary(id_pending));
        tracker
            .cache_intent_inclusions
            .insert(id_included, vec![tx_hash]);

        assert!(tracker.is_intent_included(&id_included));
        assert!(!tracker.is_intent_included(&id_pending));
    }

    #[test]
    fn is_intent_included_not_included_for_pending_only() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        tracker.pending_intents.insert(id, make_summary(id));

        // Pending-only intent is not considered "included"
        assert!(!tracker.is_intent_included(&id));
        // Unknown ID is also not included
        assert!(!tracker.is_intent_included(&Uuid::new_v4()));
    }

    #[test]
    fn serde_data_intent_db_row_full_pruned_data() {
        let item = DataIntentDbRowFull {
            id: Uuid::from_str("1bcb4515-8c91-456c-a87d-7c4f5f3f0d9e").unwrap(),
            eth_address: vec![0xaa; 20],
            data: None, // Pruned
            data_len: 10,
            data_hash: vec![0xcc; 32],
            data_hash_signature: None,
            max_blob_gas_price: 100000000,
            updated_at: DateTime::from_str("2023-01-01T12:12:12.202889Z").unwrap(),
        };

        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"data\":null"));
        assert!(json.contains("\"data_hash_signature\":null"));

        let deserialized: DataIntentDbRowFull = serde_json::from_str(&json).unwrap();
        assert!(deserialized.data.is_none());
        assert!(deserialized.data_hash_signature.is_none());
        assert_eq!(deserialized.data_len, 10);
    }

    fn make_summary_with_gas_and_time(
        id: Uuid,
        max_blob_gas_price: u64,
        updated_at: DateTime<Utc>,
    ) -> DataIntentSummary {
        DataIntentSummary {
            id,
            from: Address::random(),
            data_hash: vec![0xcc; 32],
            data_len: 100,
            max_blob_gas_price,
            updated_at,
        }
    }

    #[test]
    fn find_stale_underpriced_returns_matching_intents() {
        let mut tracker = DataIntentTracker::default();
        let old_time = Utc::now() - chrono::Duration::hours(48);
        let recent_time = Utc::now() - chrono::Duration::hours(1);

        // Old + underpriced -> should be evicted
        let id_stale = Uuid::new_v4();
        tracker.pending_intents.insert(
            id_stale,
            make_summary_with_gas_and_time(id_stale, 100, old_time),
        );

        // Old + adequately priced -> should NOT be evicted
        let id_old_good_price = Uuid::new_v4();
        tracker.pending_intents.insert(
            id_old_good_price,
            make_summary_with_gas_and_time(id_old_good_price, 2000, old_time),
        );

        // Recent + underpriced -> should NOT be evicted (not old enough)
        let id_recent = Uuid::new_v4();
        tracker.pending_intents.insert(
            id_recent,
            make_summary_with_gas_and_time(id_recent, 100, recent_time),
        );

        let current_blob_gas_price = 1000;
        let max_age = chrono::Duration::hours(24);
        let stale = tracker.find_stale_underpriced_intents(current_blob_gas_price, max_age);

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], id_stale);
    }

    #[test]
    fn find_stale_underpriced_skips_included_intents() {
        let mut tracker = DataIntentTracker::default();
        let old_time = Utc::now() - chrono::Duration::hours(48);
        let tx_hash = H256::random();

        // Old + underpriced + included in tx -> should NOT be evicted
        let id_included = Uuid::new_v4();
        tracker.pending_intents.insert(
            id_included,
            make_summary_with_gas_and_time(id_included, 100, old_time),
        );
        tracker
            .cache_intent_inclusions
            .insert(id_included, vec![tx_hash]);

        // Old + underpriced + not included -> should be evicted
        let id_not_included = Uuid::new_v4();
        tracker.pending_intents.insert(
            id_not_included,
            make_summary_with_gas_and_time(id_not_included, 100, old_time),
        );

        let stale = tracker.find_stale_underpriced_intents(1000, chrono::Duration::hours(24));

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], id_not_included);
    }

    #[test]
    fn find_stale_underpriced_empty_when_all_priced_ok() {
        let mut tracker = DataIntentTracker::default();
        let old_time = Utc::now() - chrono::Duration::hours(48);

        let id = Uuid::new_v4();
        tracker
            .pending_intents
            .insert(id, make_summary_with_gas_and_time(id, 2000, old_time));

        let stale = tracker.find_stale_underpriced_intents(1000, chrono::Duration::hours(24));
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_underpriced_empty_when_all_recent() {
        let mut tracker = DataIntentTracker::default();
        let recent_time = Utc::now() - chrono::Duration::minutes(30);

        let id = Uuid::new_v4();
        tracker
            .pending_intents
            .insert(id, make_summary_with_gas_and_time(id, 100, recent_time));

        let stale = tracker.find_stale_underpriced_intents(1000, chrono::Duration::hours(24));
        assert!(stale.is_empty());
    }

    #[test]
    fn find_stale_underpriced_boundary_price_equal_not_evicted() {
        // Intent whose max_blob_gas_price == current price should NOT be evicted
        // (only strictly less than is underpriced)
        let mut tracker = DataIntentTracker::default();
        let old_time = Utc::now() - chrono::Duration::hours(48);

        let id = Uuid::new_v4();
        tracker
            .pending_intents
            .insert(id, make_summary_with_gas_and_time(id, 1000, old_time));

        let stale = tracker.find_stale_underpriced_intents(1000, chrono::Duration::hours(24));
        assert!(stale.is_empty());
    }

    #[test]
    fn remove_pending_intent_restores_balance_calculation() {
        let mut tracker = DataIntentTracker::default();
        let id = Uuid::new_v4();
        let summary = make_summary(id);
        let from = summary.from;
        let cost = summary.max_cost();
        tracker.pending_intents.insert(id, summary);

        // Before cancel: cost is counted
        assert_eq!(tracker.non_included_intents_total_cost(&from), cost);

        // After cancel: cost is no longer counted
        tracker.remove_pending_intent(&id);
        assert_eq!(tracker.non_included_intents_total_cost(&from), 0);
    }
}
