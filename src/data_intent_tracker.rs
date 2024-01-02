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
//! user can cancel data intents by ID at any point.
//!
//! TODO: consider evicting underpriced data intents after some time.
//!
//! # Cache
//!
//! A blob share instance must run the packing algorithm against all pending data intent summaries.
//! Quering the DB on each packing iteration may become a bottleneck. Thus keep a local in-memory
//! cache of data intent summaries. Before each packing run check if there are new records after
//! the last fetched timestamp.
//!

use chrono::{DateTime, Utc};
use ethers::types::{Address, TxHash};
use eyre::{bail, Context, Result};
use futures::StreamExt;
use num_traits::cast::FromPrimitive;
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use sqlx::{types::BigDecimal, FromRow, MySql, MySqlPool, QueryBuilder};
use std::{cmp, collections::HashMap};
use uuid::Uuid;

use crate::{
    data_intent::{data_intent_max_cost, BlobGasPrice, DataIntentId},
    metrics,
    utils::{address_from_vec, option_hex_vec, txhash_from_vec},
    DataIntent,
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

// TODO: Need to prune all items once included for long enough
impl DataIntentTracker {
    pub fn collect_metrics(&self) {
        metrics::PENDING_INTENTS_CACHE.set(self.pending_intents.len() as f64);
        metrics::INCLUDED_INTENTS_CACHE.set(self.included_intents.len() as f64);
    }

    pub async fn sync_with_db(&mut self, db_pool: &MySqlPool) -> Result<()> {
        let from = self.last_sync_table_data_intents;
        let to: DateTime<Utc> = Utc::now();

        let mut stream = sqlx::query(
            r#"
SELECT id, eth_address, data_len, data_hash, max_blob_gas_price, data_hash_signature, updated_at
FROM data_intents
WHERE updated_at BETWEEN ? AND ?
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

        Ok(())
    }

    pub fn get_all_intents(&self) -> Vec<(&DataIntentSummary, Option<TxHash>)> {
        self.pending_intents
            .values()
            .map(|item| {
                (
                    item,
                    self.cache_intent_inclusions
                        .get(&item.id)
                        .and_then(|tx_hashes| tx_hashes.last())
                        .copied(),
                )
            })
            .collect()
    }

    /// Returns the total sum of pending itents cost from `from`.
    pub fn non_included_intents_total_cost(&self, from: &Address) -> u128 {
        self.pending_intents
            .values()
            .map(|intent| {
                if &intent.from == from && self.cache_intent_inclusions.get(&intent.id).is_none() {
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
                if &intent.from == from && self.cache_intent_inclusions.get(&intent.id).is_none() {
                    intent.data_len
                } else {
                    0
                }
            })
            .sum()
    }

    /// Drops all intents associated with transaction. Does not error if items not found
    pub fn finalize_tx(&mut self, tx_hash: TxHash) {
        if let Some(ids) = self.included_intents.remove(&tx_hash) {
            for id in ids {
                self.pending_intents.remove(&id);
            }
        }
    }
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct DataIntentDbRowFull {
    pub id: Uuid,
    #[serde(with = "hex_vec")]
    pub eth_address: Vec<u8>, // BINARY(20)
    #[serde(with = "hex_vec")]
    pub data: Vec<u8>, // MEDIUMBLOB
    pub data_len: u32, // INT
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>, // BINARY(32)
    pub max_blob_gas_price: u64, // BIGINT
    #[serde(with = "option_hex_vec")]
    pub data_hash_signature: Option<Vec<u8>>, // BINARY(65), Optional
    #[serde(with = "option_hex_vec")]
    pub inclusion_tx_hash: Option<Vec<u8>>, // BINARY(32), Optional
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
) -> Result<DataIntentDbRowFull> {
    let data_intent = sqlx::query_as::<_, DataIntentDbRowFull>(
        r#"
SELECT id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, inclusion_tx_hash, updated_at
FROM data_intents
WHERE id = ?
        "#)
        .bind(id)
        .fetch_one(db_pool)
        .await?;

    Ok(data_intent)
}

pub(crate) async fn fetch_many_data_intent_db_full(
    db_pool: &MySqlPool,
    ids: &[Uuid],
) -> Result<Vec<DataIntentDbRowFull>> {
    let mut query_builder: QueryBuilder<MySql> = QueryBuilder::new(
        r#"
SELECT id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, inclusion_tx_hash, updated_at
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

pub(crate) async fn fetch_data_intent_inclusion(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<Option<(TxHash, Address, u64)>> {
    let row = sqlx::query!(
        r#"
SELECT id, tx_hash, sender_address, nonce
FROM intent_inclusions
WHERE id = ?
        "#,
        id
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(match row {
        Some(row) => Some((
            txhash_from_vec(&row.tx_hash)?,
            address_from_vec(row.sender_address)?,
            row.nonce as u64,
        )),
        None => None,
    })
}

/// Store data intent to SQL DB
pub(crate) async fn store_data_intent<'c>(
    db_tx: &mut sqlx::Transaction<'c, sqlx::MySql>,
    data_intent: DataIntent,
) -> Result<DataIntentId> {
    let id = Uuid::new_v4();
    let eth_address = data_intent.from().to_fixed_bytes().to_vec();
    let data = data_intent.data();
    let data_len = data.len() as u32;
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

    Ok(rows
        .iter()
        .map(|row| Ok((Uuid::from_slice(&row.id)?, txhash_from_vec(&row.tx_hash)?)))
        .collect::<Result<Vec<_>>>()?)
}

pub(crate) async fn mark_data_intents_as_inclusion_finalized(
    db_pool: &MySqlPool,
    ids: &[Uuid],
) -> Result<()> {
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

pub(crate) async fn update_inclusion_tx_hashes(
    db_pool: &MySqlPool,
    ids: &[Uuid],
    new_inclusion_tx_hash: TxHash,
) -> Result<()> {
    let mut tx = db_pool.begin().await?;

    #[derive(Debug, FromRow, Serialize)]
    struct Row {
        id: Uuid,
        inclusion_tx_hash: Option<Vec<u8>>,
    }

    // Bulk fetch all rows
    let mut query_builder: QueryBuilder<MySql> = QueryBuilder::new(
        r#"
SELECT id, inclusion_tx_hash
FROM data_intents
WHERE id IN
    "#,
    );

    // TODO: limit the amount of ids to not reach a limit
    // TODO: try to use different API than `.push_tuples` since you only query by id
    query_builder.push_tuples(ids.iter(), |mut b, id| {
        b.push_bind(id);
    });

    let rows = query_builder.build().fetch_all(&mut *tx).await?;

    // Filter IDs where inclusion_tx_hash is not set
    for row in rows {
        let row = Row::from_row(&row)?;
        if let Some(tx_hash) = row.inclusion_tx_hash {
            bail!(
                "data_intent {} is already included in a tx {}",
                row.id,
                hex::encode(tx_hash)
            );
        }
    }

    // Batch update the filtered IDs
    let new_inclusion_tx_hash = new_inclusion_tx_hash.to_fixed_bytes().to_vec();
    for id in ids {
        sqlx::query("UPDATE data_intents SET inclusion_tx_hash = ? WHERE id = ?")
            .bind(&new_inclusion_tx_hash)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    // Commit transaction
    tx.commit().await?;

    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DataIntentSummary {
    pub id: DataIntentId,
    pub from: Address,
    #[serde(with = "hex_vec")]
    pub data_hash: Vec<u8>,
    pub data_len: usize,
    pub max_blob_gas_price: BlobGasPrice,
    pub updated_at: DateTime<Utc>,
}

impl DataIntentSummary {
    pub fn max_cost(&self) -> u128 {
        data_intent_max_cost(self.data_len, self.max_blob_gas_price)
    }
}

impl TryFrom<DataIntentDbRowSummary> for DataIntentSummary {
    type Error = eyre::Report;

    fn try_from(value: DataIntentDbRowSummary) -> Result<Self, Self::Error> {
        Ok(DataIntentSummary {
            id: value.id,
            from: address_from_vec(value.eth_address)?,
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

    use chrono::DateTime;
    use uuid::Uuid;

    use super::DataIntentDbRowFull;

    #[test]
    fn serde_data_intent_db_row_full() {
        let item = DataIntentDbRowFull {
            id: Uuid::from_str("1bcb4515-8c91-456c-a87d-7c4f5f3f0d9e").unwrap(),
            eth_address: vec![0xaa; 20],
            data: vec![0xbb; 10],
            data_len: 10,
            data_hash: vec![0xcc; 32],
            data_hash_signature: None,
            max_blob_gas_price: 100000000,
            inclusion_tx_hash: Some(vec![0xee; 32]),
            updated_at: DateTime::from_str("2023-01-01T12:12:12.202889Z").unwrap(),
        };

        let expected_item_str = "{\"id\":\"1bcb4515-8c91-456c-a87d-7c4f5f3f0d9e\",\"eth_address\":\"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"data\":\"0xbbbbbbbbbbbbbbbbbbbb\",\"data_len\":10,\"data_hash\":\"0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\",\"max_blob_gas_price\":100000000,\"data_hash_signature\":null,\"inclusion_tx_hash\":\"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\",\"updated_at\":\"2023-01-01T12:12:12.202889Z\"}";

        assert_eq!(serde_json::to_string(&item).unwrap(), expected_item_str);
        let item_recv: DataIntentDbRowFull = serde_json::from_str(expected_item_str).unwrap();
        // test eq of dedicated serde fiels with Option<Vec<u8>>
        assert_eq!(item_recv.data_hash_signature, item.data_hash_signature);
        assert_eq!(item_recv.inclusion_tx_hash, item.inclusion_tx_hash);
    }
}
