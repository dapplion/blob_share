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
    pending_intents: HashMap<DataIntentId, DataIntentItem>,
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

    pub fn mark_items_as_pending(&mut self, ids: &[DataIntentId], tx_hash: TxHash) -> Result<()> {
        for id in ids {
            match self.pending_intents.remove(id) {
                None => {
                    bail!("pending intent removed while moving into pending {:?}", id)
                }
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
        Ok(())
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
