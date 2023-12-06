use std::collections::{hash_map::Entry, HashMap};

use ethers::types::{TxHash, H256};
use eyre::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    data_intent::DataIntentId,
    sync::{BlockSync, TxInclusion},
    DataIntent,
};

#[derive(Default)]
pub struct DataIntentTracker {
    pending_intents: RwLock<HashMap<DataIntentId, DataIntentItem>>,
}

#[derive(Clone)]
pub enum DataIntentItem {
    Pending(DataIntent),
    Included(DataIntent, TxHash),
}

#[derive(Serialize, Deserialize, Debug)]
pub enum DataIntentStatus {
    Unknown,
    Pending,
    InPendingTx { tx_hash: TxHash },
    InConfirmedTx { tx_hash: TxHash, block_hash: H256 },
}

// TODO: Need to prune all items once included for long enough
impl DataIntentTracker {
    pub async fn get_all_pending(&self) -> Vec<DataIntent> {
        self.pending_intents
            .read()
            .await
            .values()
            // TODO: Do not clone here, the sum of all DataIntents can be big
            .filter_map(|item| match item {
                DataIntentItem::Pending(data_intent) => Some(data_intent.clone()),
                DataIntentItem::Included(_, _) => None,
            })
            .collect()
    }

    pub async fn add(&self, data_intent: DataIntent) -> Result<DataIntentId> {
        let id = data_intent.id();

        match self.pending_intents.write().await.entry(id) {
            Entry::Vacant(entry) => entry.insert(DataIntentItem::Pending(data_intent)),
            Entry::Occupied(_) => {
                // TODO: Handle bumping the registered max price
                bail!(
                    "data intent with data hash {} already known",
                    data_intent.data_hash
                );
            }
        };
        Ok(id)
    }

    pub async fn mark_items_as_pending(&self, ids: &[DataIntentId], tx_hash: TxHash) -> Result<()> {
        let mut items = self.pending_intents.write().await;
        for id in ids {
            match items.remove(id) {
                None => {
                    bail!("pending intent removed while moving into pending {:?}", id)
                }
                Some(DataIntentItem::Included(data_intent, prev_tx_hash)) => {
                    items.insert(*id, DataIntentItem::Included(data_intent, prev_tx_hash));
                    bail!("pending item already included in transaction {:?} while moving into pending {:?}", prev_tx_hash, id)
                }
                Some(DataIntentItem::Pending(data_intent)) => {
                    items.insert(*id, DataIntentItem::Included(data_intent, tx_hash));
                }
            }
        }
        Ok(())
    }

    pub async fn data_by_id(&self, id: &DataIntentId) -> Option<DataIntent> {
        self.pending_intents
            .read()
            .await
            .get(id)
            .map(|item| match item {
                DataIntentItem::Pending(data_intent) => data_intent.clone(),
                DataIntentItem::Included(data_intent, _) => data_intent.clone(),
            })
    }

    pub async fn status_by_id(&self, sync: &BlockSync, id: &DataIntentId) -> DataIntentStatus {
        match self.pending_intents.read().await.get(id) {
            Some(DataIntentItem::Pending(_)) => DataIntentStatus::Pending,
            Some(DataIntentItem::Included(_, tx_hash)) => {
                match sync.get_tx_status(*tx_hash).await {
                    Some(TxInclusion::Pending) => {
                        DataIntentStatus::InPendingTx { tx_hash: *tx_hash }
                    }
                    Some(TxInclusion::Included(block_hash)) => DataIntentStatus::InConfirmedTx {
                        tx_hash: *tx_hash,
                        block_hash,
                    },
                    None => {
                        // Should never happen, review this case
                        DataIntentStatus::Unknown
                    }
                }
            }
            None => DataIntentStatus::Unknown,
        }
    }
}
