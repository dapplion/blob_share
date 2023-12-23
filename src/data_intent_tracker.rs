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
//! dropped due to underpriced gas, those data intents may be reverted to pending.
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
use eyre::{bail, eyre, Context, Result};
use num_traits::cast::FromPrimitive;
use serde::{Deserialize, Serialize};
use serde_utils::hex_vec;
use sqlx::{types::BigDecimal, FromRow, MySql, MySqlPool, QueryBuilder};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    data_intent::{data_intent_max_cost, BlobGasPrice, DataIntentId},
    utils::{address_from_vec, address_to_hex_lowercase},
    DataIntent,
};

#[derive(Default)]
pub struct DataIntentTracker {
    pending_intents: HashMap<DataIntentId, DataIntentItem>,
    included_intents: HashMap<TxHash, Vec<DataIntentId>>,
}

#[derive(Clone)]
pub enum DataIntentItem {
    Pending(DataIntentSummary),
    Included(DataIntentSummary, TxHash),
}

// TODO: Need to prune all items once included for long enough
impl DataIntentTracker {
    /// Returns the total sum of pending itents cost from `from`.
    pub fn pending_intents_total_cost(&self, from: &Address) -> u128 {
        self.pending_intents
            .values()
            .map(|item| match item {
                DataIntentItem::Pending(data_intent) => {
                    if &data_intent.from == from {
                        data_intent_max_cost(data_intent.data_len, data_intent.max_blob_gas_price)
                    } else {
                        0
                    }
                }
                DataIntentItem::Included(_, _) => 0,
            })
            .sum()
    }

    pub fn get_all_pending(&self) -> Vec<DataIntentSummary> {
        self.pending_intents
            .values()
            // TODO: Do not clone here, the sum of all DataIntents can be big
            .filter_map(|item| match item {
                DataIntentItem::Pending(data_intent) => Some(data_intent.clone()),
                DataIntentItem::Included(_, _) => None,
            })
            .collect()
    }

    pub fn include_in_blob_tx(&mut self, ids: &[DataIntentId], tx_hash: TxHash) -> Result<()> {
        for id in ids {
            match self.pending_intents.remove(id) {
                None => bail!("pending intent removed while moving into pending {}", id),
                Some(DataIntentItem::Included(data_intent, prev_tx_hash)) => {
                    self.pending_intents
                        .insert(*id, DataIntentItem::Included(data_intent, prev_tx_hash));
                    bail!("pending item already included in transaction {:?} while moving into pending {}", prev_tx_hash, id)
                }
                Some(DataIntentItem::Pending(data_intent)) => {
                    self.pending_intents
                        .insert(*id, DataIntentItem::Included(data_intent, tx_hash));
                }
            }
        }

        // TODO: should handle double inclusion for same transaction hash
        self.included_intents.insert(tx_hash, ids.to_vec());

        Ok(())
    }

    pub fn revert_item_to_pending(&mut self, tx_hash: TxHash) -> Result<()> {
        let ids = self
            .included_intents
            .remove(&tx_hash)
            .ok_or_else(|| eyre!("items not known for tx_hash {}", tx_hash))?;

        for id in ids {
            match self.pending_intents.remove(&id) {
                None => bail!("pending intent removed while moving into pending {}", id),
                // TODO: Should check that the transaction is consistent?
                Some(DataIntentItem::Included(data_intent, _))
                | Some(DataIntentItem::Pending(data_intent)) => self
                    .pending_intents
                    .insert(id, DataIntentItem::Pending(data_intent)),
            };
        }
        Ok(())
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

#[derive(Debug, FromRow, Serialize)]
pub struct DataIntentDbRowFull {
    pub id: Uuid,
    pub eth_address: Vec<u8>,                 // BINARY(20)
    pub data: Vec<u8>,                        // MEDIUMBLOB
    pub data_len: u32,                        // INT
    pub data_hash: Vec<u8>,                   // BINARY(32)
    pub max_blob_gas_price: i64,              // BIGINT
    pub data_hash_signature: Option<Vec<u8>>, // BINARY(65), Optional
    pub inclusion_tx_hash: Option<Vec<u8>>,   // BINARY(32), Optional
    pub updated_at: DateTime<Utc>,            // TIMESTAMP(3)
}

#[derive(Debug, FromRow, Serialize)]
pub struct DataIntentDbRowSummary {
    pub id: Uuid,
    pub eth_address: Vec<u8>,                 // BINARY(20)
    pub data_len: u32,                        // INT
    pub data_hash: Vec<u8>,                   // BINARY(32)
    pub max_blob_gas_price: i64,              // BIGINT
    pub data_hash_signature: Option<Vec<u8>>, // BINARY(65), Optional
    pub inclusion_tx_hash: Option<Vec<u8>>,   // BINARY(32), Optional
    pub updated_at: DateTime<Utc>,            // TIMESTAMP(3)
}

pub(crate) async fn fetch_data_intent_db_full(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<DataIntentDbRowFull> {
    let data_intent = sqlx::query_as::<_, DataIntentDbRowFull>(
        "SELECT eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, inclusion_tx_hash, updated_at FROM data_intents WHERE id = ?")
        .bind(id)
        .fetch_one(db_pool)
        .await?;

    Ok(data_intent)
}

pub(crate) async fn fetch_many_data_intent_db_full(
    db_pool: &MySqlPool,
    ids: &[Uuid],
) -> Result<Vec<DataIntentDbRowFull>> {
    let mut query_builder: QueryBuilder<MySql> = QueryBuilder::new("SELECT eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature, inclusion_tx_hash, updated_at FROM data_intents WHERE id in");

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

pub(crate) async fn fetch_data_intent_db_summary(
    db_pool: &MySqlPool,
    id: &Uuid,
) -> Result<Option<DataIntentDbRowSummary>> {
    let data_intent = sqlx::query_as::<_, DataIntentDbRowSummary>(
        "SELECT id, eth_address, data_len, data_hash, max_blob_gas_price, data_hash_signature, inclusion_tx_hash, updated_at FROM data_intents WHERE id = ?")
        .bind(id)
        .fetch_optional(db_pool)
        .await?;

    Ok(data_intent)
}

/// Store data intent to SQL DB
pub(crate) async fn store_data_intent<'c>(
    db_tx: &mut sqlx::Transaction<'c, sqlx::MySql>,
    data_intent: DataIntent,
) -> Result<DataIntentId> {
    let id = Uuid::new_v4();
    let eth_address = address_to_hex_lowercase(*data_intent.from());
    let data = data_intent.data();
    let data_len = data.len() as u32;
    let data_hash = data_intent.data_hash().to_vec();
    let max_blob_gas_price = BigDecimal::from_u64(data_intent.max_blob_gas_price());
    let data_hash_signature = data_intent.data_hash_signature().map(|sig| sig.to_vec());

    // Persist data request
    sqlx::query!(
            "INSERT INTO data_intents (id, eth_address, data, data_len, data_hash, max_blob_gas_price, data_hash_signature) VALUES (?, ?, ?, ?, ?, ?, ?)",
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

impl TryFrom<DataIntentDbRowSummary> for DataIntentSummary {
    type Error = eyre::Report;

    fn try_from(value: DataIntentDbRowSummary) -> Result<Self, Self::Error> {
        Ok(DataIntentSummary {
            id: value.id,
            from: address_from_vec(value.eth_address)?,
            data_hash: value.data_hash,
            data_len: value.data_len.try_into()?,
            max_blob_gas_price: value.max_blob_gas_price.try_into()?,
            updated_at: value.updated_at,
        })
    }
}
