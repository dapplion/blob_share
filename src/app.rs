use ethers::{signers::LocalWallet, types::Address};
use eyre::{bail, eyre, Context, Result};
use num_traits::cast::{FromPrimitive, ToPrimitive};
use sqlx::{types::BigDecimal, MySqlPool};
use tokio::sync::{Notify, RwLock};

use crate::{
    client::{DataIntentDbRowFull, DataIntentId, DataIntentStatus, DataIntentSummary},
    data_intent_tracker::{
        fetch_data_intent_db_full, fetch_data_intent_db_summary, fetch_many_data_intent_db_full,
        store_data_intent, update_inclusion_tx_hashes, DataIntentTracker,
    },
    eth_provider::EthProvider,
    routes::SyncStatusBlock,
    sync::{BlockSync, BlockWithTxs, SyncBlockError, SyncBlockOutcome, TxInclusion},
    utils::{address_to_hex_lowercase, txhash_from_vec},
    AppConfig, BlobTxSummary, DataIntent,
};

pub(crate) struct AppData {
    pub config: AppConfig,
    pub kzg_settings: c_kzg::KzgSettings,
    pub provider: EthProvider,
    pub sender_wallet: LocalWallet,
    pub notify: Notify,
    pub chain_id: u64,
    // Private members, to ensure consistent manipulation
    data_intent_tracker: RwLock<DataIntentTracker>,
    sync: RwLock<BlockSync>,
    db_pool: MySqlPool,
}

impl AppData {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: AppConfig,
        kzg_settings: c_kzg::KzgSettings,
        db_pool: MySqlPool,
        provider: EthProvider,
        sender_wallet: LocalWallet,
        chain_id: u64,
        data_intent_tracker: DataIntentTracker,
        sync: BlockSync,
    ) -> Self {
        AppData {
            config,
            kzg_settings,
            db_pool,
            provider,
            sender_wallet,
            notify: <_>::default(),
            chain_id,
            data_intent_tracker: data_intent_tracker.into(),
            sync: sync.into(),
        }
    }

    #[tracing::instrument(skip(self, data_intent))]
    pub async fn atomic_update_post_data_on_unsafe_channel(
        &self,
        data_intent: DataIntent,
        nonce: u64,
    ) -> Result<DataIntentId> {
        let eth_address = address_to_hex_lowercase(*data_intent.from());

        let mut tx = self.db_pool.begin().await?;

        // Fetch user row, may not have any records yet
        let user_row = sqlx::query!(
            "SELECT post_data_nonce FROM users WHERE eth_address = ? FOR UPDATE",
            eth_address,
        )
        .fetch_optional(&mut *tx)
        .await?;

        // Check user balance
        let last_nonce = user_row.and_then(|row| row.post_data_nonce);

        // Check nonce is higher
        if let Some(last_nonce) = last_nonce {
            if nonce <= last_nonce.try_into()? {
                bail!("Nonce not new, replay protection");
            }
        }

        // Update balance and nonce
        // TODO: Should assert that 1 row was affected?
        sqlx::query!(
            "UPDATE users SET post_data_nonce = ? WHERE eth_address = ?",
            Some(nonce),
            eth_address,
        )
        .execute(&mut *tx)
        .await?;

        let id = store_data_intent(&mut tx, data_intent).await?;

        // Commit transaction
        tx.commit().await?;

        Ok(id)
    }

    pub async fn evict_underpriced_pending_txs(&self) -> Result<usize> {
        let underpriced_txs = { self.sync.write().await.evict_underpriced_pending_txs() };

        if !underpriced_txs.is_empty() {
            let mut data_intent_tracker = self.data_intent_tracker.write().await;
            for tx in &underpriced_txs {
                // TODO: should handle each individual error or abort iteration?
                data_intent_tracker.revert_item_to_pending(tx.tx_hash)?;
            }
        }

        Ok(underpriced_txs.len())
    }

    pub async fn maybe_advance_anchor_block(&self) -> Result<Option<(Vec<BlobTxSummary>, u64)>> {
        if let Some((finalized_txs, new_anchor_block_number)) =
            self.sync.write().await.maybe_advance_anchor_block()?
        {
            let mut data_intent_tracker = self.data_intent_tracker.write().await;
            for tx in &finalized_txs {
                data_intent_tracker.finalize_tx(tx.tx_hash);
            }

            Ok(Some((finalized_txs, new_anchor_block_number)))
        } else {
            Ok(None)
        }
    }

    pub async fn blob_gas_price_next_head_block(&self) -> u128 {
        self.sync
            .read()
            .await
            .get_head_gas()
            .blob_gas_price_next_block()
    }

    pub async fn register_sent_blob_tx(
        &self,
        data_intent_ids: &[DataIntentId],
        blob_tx: BlobTxSummary,
    ) -> Result<()> {
        update_inclusion_tx_hashes(&self.db_pool, data_intent_ids, blob_tx.tx_hash).await?;

        self.sync
            .write()
            .await
            .register_pending_blob_tx(blob_tx)
            .wrap_err("consistency error with blob_tx")?;

        Ok(())
    }

    pub async fn sync_data_intents(&self) -> Result<()> {
        self.data_intent_tracker
            .write()
            .await
            .sync_with_db(&self.db_pool)
            .await
    }

    pub async fn sync_next_head(
        &self,
        block: BlockWithTxs,
    ) -> Result<SyncBlockOutcome, SyncBlockError> {
        BlockSync::sync_next_head(&self.sync, &self.provider, block).await
    }

    pub async fn reserve_next_available_nonce(
        &self,
        sender_address: Address,
    ) -> Result<Option<u64>> {
        self.sync
            .write()
            .await
            .reserve_next_available_nonce(&self.provider, sender_address)
            .await
    }

    pub async fn unreserve_nonce(&self, sender_address: Address, nonce: u64) {
        self.sync
            .write()
            .await
            .unreserve_nonce(sender_address, nonce);
    }

    #[tracing::instrument(skip(self))]
    pub async fn pending_total_data_len(&self, address: &Address) -> usize {
        self.data_intent_tracker
            .read()
            .await
            .pending_intents_total_data_len(address)
            + self.sync.read().await.pending_txs_data_len(address)
    }

    #[tracing::instrument(skip(self))]
    pub async fn balance_of_user(&self, from: &Address) -> i128 {
        self.sync.read().await.balance_with_pending(from)
            - self
                .data_intent_tracker
                .read()
                .await
                .pending_intents_total_cost(from) as i128
    }

    #[tracing::instrument(skip(self))]
    pub async fn status_by_id(&self, id: &DataIntentId) -> Result<DataIntentStatus> {
        Ok(
            match fetch_data_intent_db_summary(&self.db_pool, id).await? {
                None => DataIntentStatus::Unknown,
                Some(data_intent) => {
                    match data_intent.inclusion_tx_hash {
                        None => DataIntentStatus::Pending,
                        Some(tx_hash) => {
                            let tx_hash = txhash_from_vec(tx_hash)?;
                            match self.sync.read().await.get_tx_status(tx_hash) {
                                Some(TxInclusion::Pending) => {
                                    DataIntentStatus::InPendingTx { tx_hash }
                                }
                                Some(TxInclusion::Included(block_hash)) => {
                                    DataIntentStatus::InConfirmedTx {
                                        tx_hash,
                                        block_hash,
                                    }
                                }
                                None => {
                                    // Should never happen, review this case
                                    DataIntentStatus::Unknown
                                }
                            }
                        }
                    }
                }
            },
        )
    }

    pub async fn data_intent_by_id(&self, id: &DataIntentId) -> Result<DataIntentDbRowFull> {
        fetch_data_intent_db_full(&self.db_pool, id).await
    }

    pub async fn data_intents_by_id(
        &self,
        ids: &[DataIntentId],
    ) -> Result<Vec<DataIntentDbRowFull>> {
        fetch_many_data_intent_db_full(&self.db_pool, ids).await
    }

    pub async fn get_all_pending(&self) -> Vec<DataIntentSummary> {
        self.data_intent_tracker.read().await.get_all_pending()
    }

    pub async fn get_sync(&self) -> (SyncStatusBlock, SyncStatusBlock) {
        (
            self.sync.read().await.get_anchor().into(),
            self.sync.read().await.get_head(),
        )
    }

    pub async fn serialize_anchor_block(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self.sync.read().await.get_anchor())
    }

    pub async fn collect_metrics(&self) {
        self.sync.read().await.collect_metrics();
        self.data_intent_tracker.read().await.collect_metrics();
    }
}
