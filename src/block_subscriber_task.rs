use std::sync::Arc;

use ethers::{providers::StreamExt, types::TxHash};
use eyre::{eyre, Context, Result};

use crate::{
    debug, error, info,
    sync::{BlockSync, BlockWithTxs, SyncBlockError},
    AppData,
};

pub(crate) async fn block_subscriber_task(app_data: Arc<AppData>) -> Result<()> {
    debug!("starting block subscriber task");

    // Subscribes to 'newHeads' which:
    // >  fires a notification each time a new header is appended to the chain, including chain reorganizations
    // Ref: https://github.com/gakonst/ethers-rs/blob/f0e5b194f09c533feb10d1a686ddb9e5946ec107/ethers-providers/src/rpc/provider.rs#L1066
    // Ref: https://www.quicknode.com/docs/ethereum/eth_subscribe
    let mut s = app_data.provider.subscribe_blocks().await?;

    while let Some(block_hash) = s.next().await {
        let block_hash = block_hash?;

        // Run sync routine, may involve long network requests if there's a re-org
        match sync_block(app_data.clone(), block_hash).await {
            Err(SyncBlockError::ReorgTooDeep {
                anchor_block_number,
            }) => {
                // Irrecoverable error, crash app
                return Err(eyre!(
                    "ReorgTooDeep anchor_block_number: {}",
                    anchor_block_number
                ));
            }
            Err(SyncBlockError::Other(e)) => {
                if app_data.config.panic_on_background_task_errors {
                    return Err(e);
                } else {
                    error!("error syncing block {:?}: {:?}", block_hash, e);
                }
            }
            Ok(_) => {}
        }

        // Maybe compute new blob transactions
        app_data.notify.notify_one();
    }

    Ok(())
}

async fn sync_block(app_data: Arc<AppData>, block_hash: TxHash) -> Result<(), SyncBlockError> {
    let block_with_txs = app_data
        .provider
        .get_block_with_txs(block_hash)
        .await
        .wrap_err(format!("error fetching block {}", block_hash))?
        .ok_or_else(|| eyre!("block with txs not available {}", block_hash))?;
    let block_number = block_with_txs.number;

    let outcome = BlockSync::sync_next_head(
        &app_data.sync,
        &app_data.provider,
        BlockWithTxs::from_ethers_block(block_with_txs)?,
    )
    .await?;

    info!(
        "synced block {:?} {:?}, outcome: {:?}",
        block_number, block_hash, outcome
    );

    // Check if any pending transactions need re-pricing
    let underpriced_txs = { app_data.sync.write().await.evict_underpriced_pending_txs() };

    if !underpriced_txs.is_empty() {
        {
            let mut data_intent_tracker = app_data.data_intent_tracker.write().await;
            for tx in &underpriced_txs {
                // TODO: should handle each individual error or abort iteration?
                data_intent_tracker.revert_item_to_pending(tx.tx_hash)?;
            }
        }

        // Potentially prepare new blob transactions with correct pricing
        app_data.notify.notify_one();
    }

    // Check if any intents are underpriced
    {
        let blob_gas_price_next_block = {
            app_data
                .sync
                .read()
                .await
                .get_head_gas()
                .blob_gas_price_next_block()
        };
        let mut data_intent_tracker = app_data.data_intent_tracker.write().await;
        let items = data_intent_tracker.get_all_pending();
        for item in items {
            if blob_gas_price_next_block > item.max_blob_gas_price {
                // Underpriced transaction, evict
                data_intent_tracker.evict_underpriced_intent(&item.id())?;
            }
        }
    }

    // Finalize transactions
    if let Some((finalized_txs, new_anchor_block_number)) =
        app_data.sync.write().await.maybe_advance_anchor_block()?
    {
        let finalized_tx_hashes = finalized_txs
            .iter()
            .map(|tx| tx.tx_hash)
            .collect::<Vec<_>>();
        info!(
            "Finalized transactions, new anchor block number {} {:?}",
            new_anchor_block_number, finalized_tx_hashes
        );

        let mut data_intent_tracker = app_data.data_intent_tracker.write().await;
        for tx in finalized_txs {
            data_intent_tracker.finalize_tx(tx.tx_hash);
        }
    }

    Ok(())
}
