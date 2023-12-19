use std::collections::HashMap;

use ethers::types::{Address, TxHash};
use eyre::{bail, eyre, Result};

use crate::{data_intent::DataIntentId, DataIntent};

#[derive(Default)]
pub struct DataIntentTracker {
    pending_intents: HashMap<DataIntentId, DataIntentItem>,
    included_intents: HashMap<TxHash, Vec<DataIntentId>>,
}

#[derive(Clone)]
pub enum DataIntentItem {
    // TODO: Evicted items are never pruned
    Evicted,
    Pending(DataIntent),
    Included(DataIntent, TxHash),
}

// TODO: Need to prune all items once included for long enough
impl DataIntentTracker {
    /// Returns the total sum of pending itents cost from `from`.
    pub fn pending_intents_total_cost(&self, from: &Address) -> u128 {
        self.pending_intents
            .values()
            .map(|item| match item {
                DataIntentItem::Pending(data_intent) => {
                    if data_intent.from() == from {
                        data_intent.max_cost()
                    } else {
                        0
                    }
                }
                DataIntentItem::Evicted | DataIntentItem::Included(_, _) => 0,
            })
            .sum()
    }

    /// Returns the max nonce out of all pending intents
    pub fn pending_nonces(&self, from: &Address) -> Vec<u64> {
        self.pending_intents
            .values()
            .filter_map(|item| match item {
                DataIntentItem::Pending(data_intent) => {
                    if data_intent.from() == from {
                        Some(data_intent.nonce())
                    } else {
                        None
                    }
                }
                DataIntentItem::Evicted | DataIntentItem::Included(_, _) => None,
            })
            .collect()
    }

    pub fn get_all_pending(&self) -> Vec<DataIntent> {
        self.pending_intents
            .values()
            // TODO: Do not clone here, the sum of all DataIntents can be big
            .filter_map(|item| match item {
                DataIntentItem::Evicted => None,
                DataIntentItem::Pending(data_intent) => Some(data_intent.clone()),
                DataIntentItem::Included(_, _) => None,
            })
            .collect()
    }

    pub fn add(&mut self, data_intent: DataIntent) -> Result<()> {
        let id = data_intent.id();

        match self.pending_intents.get(&id) {
            None => {}                          // Ok insert
            Some(DataIntentItem::Evicted) => {} // Allow to re-insert evicted intents
            // TODO: Handle bumping the registered max price
            Some(DataIntentItem::Pending(_)) | Some(DataIntentItem::Included(_, _)) => {
                bail!("data intent {id} already known")
            }
        };

        self.pending_intents
            .insert(id, DataIntentItem::Pending(data_intent));
        Ok(())
    }

    pub fn evict_underpriced_intent(&mut self, id: &DataIntentId) -> Result<()> {
        match self.pending_intents.get(id) {
            None => bail!("unknown intent {}", id),
            Some(DataIntentItem::Evicted) => bail!("intent already evicted {}", id),
            Some(DataIntentItem::Included(_, prev_tx_hash)) => {
                bail!("attempting to evict intent included in transaction {prev_tx_hash:?} {id}")
            }
            Some(DataIntentItem::Pending(_)) => {
                self.pending_intents.insert(*id, DataIntentItem::Evicted);
                Ok(())
            }
        }
    }

    pub fn include_in_blob_tx(&mut self, ids: &[DataIntentId], tx_hash: TxHash) -> Result<()> {
        for id in ids {
            match self.pending_intents.remove(id) {
                None => bail!("pending intent removed while moving into pending {}", id),
                Some(DataIntentItem::Evicted) => bail!("intent has been evicted {}", id),
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
                Some(DataIntentItem::Evicted) => bail!("item evicted {}", id),
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

    pub fn data_by_id(&self, id: &DataIntentId) -> Option<DataIntent> {
        match self.pending_intents.get(id) {
            None => None,
            Some(DataIntentItem::Evicted) => None,
            Some(DataIntentItem::Pending(data_intent)) => Some(data_intent.clone()),
            Some(DataIntentItem::Included(data_intent, _)) => Some(data_intent.clone()),
        }
    }

    pub fn status_by_id(&self, id: &DataIntentId) -> DataIntentItemStatus {
        match self.pending_intents.get(id) {
            None => DataIntentItemStatus::Unknown,
            Some(DataIntentItem::Evicted) => DataIntentItemStatus::Evicted,
            Some(DataIntentItem::Pending(_)) => DataIntentItemStatus::Pending,
            Some(DataIntentItem::Included(_, tx_hash)) => DataIntentItemStatus::Included(*tx_hash),
        }
    }
}

pub enum DataIntentItemStatus {
    Pending,
    Included(TxHash),
    Evicted,
    Unknown,
}
