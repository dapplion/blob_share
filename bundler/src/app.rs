use std::collections::{hash_map::Entry, HashMap};

use bundler_client::types::{
    BlockGasSummary, DataIntentFull, DataIntentId, DataIntentStatus, DataIntentSummary,
    SyncStatusBlock,
};
use ethers::{
    signers::LocalWallet,
    types::{Address, BlockNumber, H256},
};
use eyre::{bail, eyre, Result};
use futures::{stream, StreamExt, TryStreamExt};
use handlebars::Handlebars;
use sqlx::MySqlPool;
use tokio::sync::{Notify, RwLock};

use crate::{
    anchor_block::persist_anchor_block_to_db,
    data_intent_tracker::{
        fetch_all_intents_with_inclusion_not_finalized, fetch_data_intent_db_full,
        fetch_data_intent_db_is_known, fetch_data_intent_inclusion, fetch_many_data_intent_db_full,
        mark_data_intents_as_inclusion_finalized, store_data_intent, DataIntentDbRowFull,
        DataIntentTracker,
    },
    eth_provider::EthProvider,
    info,
    sync::{BlockSync, BlockWithTxs, NonceStatus, SyncBlockError, SyncBlockOutcome, TxInclusion},
    utils::address_to_hex_lowercase,
    warn, AppConfig, BlobGasPrice, BlobTxSummary, DataIntent,
};

pub(crate) const PERSIST_ANCHOR_BLOCK_INITIAL_SYNC_INTERVAL: u64 = 32;
pub(crate) const MAX_DISTANCE_SYNC: u64 = 8;
/// Limit the maximum number of times a data intent included in a previous transaction can be
/// included again in a new transaction.
const MAX_PREVIOUS_INCLUSIONS: usize = 2;

pub(crate) struct AppData {
    pub handlebars: Handlebars<'static>,
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
        handlebars: Handlebars<'static>,
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
            handlebars,
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

    pub async fn maybe_advance_anchor_block(&self) -> Result<Option<(Vec<BlobTxSummary>, u64)>> {
        let finalize_result = { self.sync.write().await.maybe_advance_anchor_block()? };

        if let Some(finalized_result) = finalize_result {
            let mut data_intent_tracker = self.data_intent_tracker.write().await;
            for tx in &finalized_result.finalized_included_txs {
                data_intent_tracker.finalize_tx(tx.tx_hash);
            }

            // TODO: Mark intents as finalzed

            // Forget about excluded transactions
            for _excluded_tx in finalized_result.finalized_excluded_txs {
                // TODO: Drop inclusions for excluded transactions
                // data_intent_tracker.drop_excluded_tx(excluded_tx.tx_hash);
            }

            // TODO: Persist anchor block to DB less often
            persist_anchor_block_to_db(&self.db_pool, self.sync.read().await.get_anchor()).await?;

            Ok(Some((
                finalized_result.finalized_included_txs,
                finalized_result.new_anchor_block_number,
            )))
        } else {
            Ok(None)
        }
    }

    /// Mark data intents included in finalized blocks as such, to prevent re-fetching them during
    /// the packing phase.
    pub async fn initial_consistency_check_intents_with_inclusion_finalized(
        &self,
    ) -> Result<Vec<DataIntentId>> {
        let anchor_block_number = { self.sync.read().await.get_anchor().number };
        let intents = fetch_all_intents_with_inclusion_not_finalized(&self.db_pool).await?;

        // Cache to prevent fetching the same transaction for the same intent
        let mut tx_cache: HashMap<H256, Option<u64>> = <_>::default();
        let mut ids_with_inclusion_finalized = vec![];

        for (id, tx_hash) in intents {
            // TODO: cache fetch of the same transaction

            if let Entry::Vacant(e) = tx_cache.entry(tx_hash) {
                let tx = self.provider.get_transaction(tx_hash).await?;
                e.insert(tx.and_then(|tx| tx.block_number.map(|n| n.as_u64())));
            }

            if let Some(Some(inclusion_block_number)) = tx_cache.get(&tx_hash) {
                if inclusion_block_number <= &anchor_block_number {
                    // intent was included in a block equal or ancestor of finalized anchor
                    // block
                    ids_with_inclusion_finalized.push(id);
                }
            }
        }

        mark_data_intents_as_inclusion_finalized(&self.db_pool, &ids_with_inclusion_finalized)
            .await?;

        Ok(ids_with_inclusion_finalized)
    }

    pub async fn blob_gas_price_next_head_block(&self) -> u128 {
        self.sync
            .read()
            .await
            .get_head_gas()
            .blob_gas_price_next_block()
    }

    /// Register valid accepted blob transaction by the EL node
    pub async fn register_sent_blob_tx(
        &self,
        data_intent_ids: &[DataIntentId],
        blob_tx: BlobTxSummary,
    ) -> Result<()> {
        // TODO: do not grab the data_intent_tracker lock for so long here
        self.data_intent_tracker
            .write()
            .await
            .insert_many_intent_tx_inclusions(&self.db_pool, data_intent_ids, &blob_tx)
            .await?;

        self.sync.write().await.register_sent_blob_tx(blob_tx);

        Ok(())
    }

    pub async fn sync_data_intents(&self) -> Result<usize> {
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

    pub async fn get_next_available_nonce(&self, sender_address: Address) -> Result<NonceStatus> {
        self.sync
            .read()
            .await
            .get_next_available_nonce(&self.provider, sender_address)
            .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn pending_total_data_len(&self, address: &Address) -> usize {
        self.data_intent_tracker
            .read()
            .await
            .non_included_intents_total_data_len(address)
            + self.sync.read().await.pending_txs_data_len(address)
    }

    #[tracing::instrument(skip(self))]
    pub async fn balance_of_user(&self, from: &Address) -> i128 {
        // sync tracks the balance of anything that has been included in transaction. Compliment
        // that balance with never included data intents from the data intent tracker
        self.sync.read().await.balance_with_pending(from)
            - self
                .data_intent_tracker
                .read()
                .await
                .non_included_intents_total_cost(from) as i128
    }

    #[tracing::instrument(skip(self))]
    pub async fn status_by_id(&self, id: &DataIntentId) -> Result<DataIntentStatus> {
        let inclusions = fetch_data_intent_inclusion(&self.db_pool, id).await?;

        let status = if let Some(inclusion) = inclusions.last().copied() {
            let tx_hash = inclusion.tx_hash;
            // Check status of transaction against EL.
            // `eth_getTransactionByHash` returns the transaction both if included or if in the pool
            // Ref: https://www.quicknode.com/docs/ethereum/eth_getTransactionByHash
            if let Some(tx) = self.provider.get_transaction(tx_hash).await? {
                match tx.block_hash {
                    None => DataIntentStatus::InPendingTx { tx_hash },
                    Some(block_hash) => DataIntentStatus::InConfirmedTx {
                        tx_hash,
                        block_hash,
                    },
                }
            } else {
                DataIntentStatus::InPendingTx { tx_hash }
            }
        } else {
            // Check if intent is known
            // TODO: Should check only in-memory?
            if fetch_data_intent_db_is_known(&self.db_pool, id).await? {
                DataIntentStatus::Pending
            } else {
                DataIntentStatus::Unknown
            }
        };
        Ok(status)
    }

    pub async fn data_intent_by_id(&self, id: &DataIntentId) -> Result<DataIntentFull> {
        fetch_data_intent_db_full(&self.db_pool, id).await
    }

    pub async fn data_intents_by_id(
        &self,
        ids: &[DataIntentId],
    ) -> Result<Vec<DataIntentDbRowFull>> {
        fetch_many_data_intent_db_full(&self.db_pool, ids).await
    }

    pub async fn get_all_intents_available_for_packing(
        &self,
        min_blob_gas_fee: BlobGasPrice,
    ) -> (Vec<DataIntentSummary>, usize) {
        let sync = self.sync.read().await;
        let data_intent_tracker = self.data_intent_tracker.read().await;

        let mut items_from_previous_inclusions = 0;

        let data_intents = data_intent_tracker
            .get_all_intents()
            .into_iter()
            .filter_map(|(intent, tx_hash, previous_inclusions)| {
                // Ignore items that can't pay the minimum blob base fee
                let should_include = intent.max_blob_gas_price >= min_blob_gas_fee
                    && if let Some(tx_hash) = tx_hash {
                        match sync.get_tx_status(tx_hash) {
                            // This should never happen, the intent is marked as part of a bundle but
                            // the sync does not know about it. To be safe, do not include in bundles
                            None => false,
                            // If transaction is pending, only re-bundle if it's underpriced
                            Some(TxInclusion::Pending { tx_gas }) => {
                                if previous_inclusions < MAX_PREVIOUS_INCLUSIONS
                                    && tx_gas.is_underpriced(sync.get_head_gas())
                                {
                                    items_from_previous_inclusions += 1;
                                    true
                                } else {
                                    false
                                }
                            }
                            // Do not re-bundle intents part of a block
                            Some(TxInclusion::Included { .. }) => false,
                        }
                    } else {
                        // Always include intents not part of any transaction
                        true
                    };

                if should_include {
                    // TODO: prevent having to clone here
                    Some(intent.clone())
                } else {
                    None
                }
            })
            .collect();

        (data_intents, items_from_previous_inclusions)
    }

    /// Do initial blocking sync to get to the remote node head before starting the API and
    /// potentially building blob transactions.
    pub async fn initial_block_sync(&self) -> Result<()> {
        loop {
            let remote_node_head_block = self.fetch_remote_node_latest_block_number().await?;
            let head_block = self.sync.read().await.get_head().number;

            // Every sync iteration get closer to the remote head until being close enough
            if head_block < remote_node_head_block + MAX_DISTANCE_SYNC {
                break;
            }

            stream::iter(head_block + 1..remote_node_head_block)
                .map(|block_number| self.fetch_block(block_number))
                .buffered(16)
                .try_for_each(|block| async {
                    let block_number = block.number;
                    let outcome = self.sync_next_head(block).await?;

                    if let SyncBlockOutcome::BlockKnown = outcome {
                        warn!("initial sync imported a known block {block_number}");
                    }

                    if block_number % PERSIST_ANCHOR_BLOCK_INITIAL_SYNC_INTERVAL == 0 {
                        self.maybe_advance_anchor_block().await?;
                        info!(
                            "initial sync progress {block_number}/{remote_node_head_block} {} left",
                            remote_node_head_block - block_number
                        )
                    }
                    Ok(())
                })
                .await?;
        }

        Ok(())
    }

    /// Helper for `self.initial_block_sync`
    async fn fetch_block(&self, block_number: u64) -> Result<BlockWithTxs> {
        let block = self
            .provider
            .get_block_with_txs(block_number)
            .await?
            .ok_or_else(|| eyre!(format!("no block for number {block_number}")))?;
        BlockWithTxs::from_ethers_block(block)
    }

    pub async fn get_sync(&self) -> (SyncStatusBlock, SyncStatusBlock) {
        (
            self.sync.read().await.get_anchor().into(),
            self.sync.read().await.get_head(),
        )
    }

    pub async fn get_head_gas(&self) -> BlockGasSummary {
        *self.sync.read().await.get_head_gas()
    }

    pub async fn fetch_remote_node_latest_block_number(&self) -> Result<u64> {
        let block = self
            .provider
            .get_block(BlockNumber::Latest)
            .await?
            .ok_or_else(|| eyre!("no latest block"))?;
        Ok(block
            .number
            .ok_or_else(|| eyre!("block has no number"))?
            .as_u64())
    }

    pub async fn assert_node_synced(&self) -> Result<()> {
        let remote_node_head = self.fetch_remote_node_latest_block_number().await?;
        let head_number = self.sync.read().await.get_head().number;
        if remote_node_head > head_number + MAX_DISTANCE_SYNC {
            bail!("Local head number {head_number} not synced with remote node {remote_node_head}");
        } else {
            Ok(())
        }
    }

    /// Verify that critical dependencies (database, eth provider) are reachable.
    pub async fn health_check(&self) -> Result<()> {
        // Verify database connectivity
        sqlx::query("SELECT 1")
            .execute(&self.db_pool)
            .await
            .map_err(|e| eyre!("database health check failed: {e}"))?;
        // Verify eth provider connectivity
        self.provider
            .get_block_number()
            .await
            .map_err(|e| eyre!("eth provider health check failed: {e}"))?;
        Ok(())
    }

    pub async fn collect_metrics(&self) {
        self.sync.read().await.collect_metrics();
        self.data_intent_tracker.read().await.collect_metrics();
    }
}
