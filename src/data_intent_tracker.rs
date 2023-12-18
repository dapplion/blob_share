use std::collections::{hash_map::Entry, HashMap, HashSet};

use ethers::types::TxHash;
use eyre::{bail, eyre, Result};

use crate::{data_intent::DataIntentId, DataIntent};

#[derive(Default)]
pub struct DataIntentTracker {
    pending_intents: HashMap<DataIntentId, DataIntentItem>,
    included_intents: HashMap<TxHash, Vec<DataIntentId>>,
    // TODO: move to DB persistance, this set is not bounded
    evicted_underpriced_intents: HashSet<DataIntentId>,
}

#[derive(Clone)]
pub enum DataIntentItem {
    Pending(DataIntent),
    Included(DataIntent, TxHash),
}

// TODO: Need to prune all items once included for long enough
impl DataIntentTracker {
    pub fn get_all_pending(&self) -> Vec<DataIntent> {
        self.pending_intents
            .values()
            // TODO: Do not clone here, the sum of all DataIntents can be big
            .filter_map(|item| match item {
                DataIntentItem::Pending(data_intent) => Some(data_intent.clone()),
                DataIntentItem::Included(_, _) => None,
            })
            .collect()
    }

    pub fn add(&mut self, data_intent: DataIntent) -> Result<DataIntentId> {
        let id = data_intent.id();

        match self.pending_intents.entry(id) {
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

    pub fn evict_underpriced_intent(&mut self, id: &DataIntentId) -> Result<()> {
        match self.pending_intents.remove(id) {
            None => bail!("unknown intent {:?}", id),
            Some(DataIntentItem::Included(data_intent, prev_tx_hash)) => {
                self.pending_intents
                    .insert(*id, DataIntentItem::Included(data_intent, prev_tx_hash));
                bail!("attempting to evict intent included in transaction {prev_tx_hash:?} {id:?}")
            }
            Some(DataIntentItem::Pending(_)) => {
                self.evicted_underpriced_intents.insert(*id);
                Ok(())
            }
        }
    }

    pub fn mark_items_as_pending(&mut self, ids: &[DataIntentId], tx_hash: TxHash) -> Result<()> {
        for id in ids {
            match self.pending_intents.remove(id) {
                None => bail!("pending intent removed while moving into pending {:?}", id),
                Some(DataIntentItem::Included(data_intent, prev_tx_hash)) => {
                    self.pending_intents
                        .insert(*id, DataIntentItem::Included(data_intent, prev_tx_hash));
                    bail!("pending item already included in transaction {:?} while moving into pending {:?}", prev_tx_hash, id)
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
                None => bail!("pending intent removed while moving into pending {:?}", id),
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
        self.pending_intents.get(id).map(|item| match item {
            DataIntentItem::Pending(data_intent) => data_intent.clone(),
            DataIntentItem::Included(data_intent, _) => data_intent.clone(),
        })
    }

    pub fn status_by_id(&self, id: &DataIntentId) -> DataIntentItemStatus {
        match self.pending_intents.get(id) {
            Some(DataIntentItem::Pending(_)) => DataIntentItemStatus::Pending,
            Some(DataIntentItem::Included(_, tx_hash)) => DataIntentItemStatus::Included(*tx_hash),
            None => DataIntentItemStatus::Unknown,
        }
    }
}

pub enum DataIntentItemStatus {
    Pending,
    Included(TxHash),
    Unknown,
}
