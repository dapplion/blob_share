use std::sync::Arc;

use ethers::{
    providers::{Middleware, StreamExt},
    types::{Block, TxHash},
};
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

    while let Some(block) = s.next().await {
        // Register gas prices, async just to grab the lock
        app_data.gas_tracker.new_head(&block).await?;

        // Run sync routine, may involve long network requests if there's a re-org
        match sync_block(app_data.clone(), &block).await {
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
                    error!(
                        "error syncing block {:?} {:?}: {:?}",
                        block.number, block.hash, e
                    );
                }
            }
            Ok(_) => {}
        }

        // Maybe compute new blob transactions
        app_data.notify.notify_one();
    }

    Ok(())
}

async fn sync_block(app_data: Arc<AppData>, block: &Block<TxHash>) -> Result<(), SyncBlockError> {
    let block_hash = block
        .hash
        .ok_or_else(|| eyre!("block has no hash {:?}", block.number))?;

    let block_with_txs = app_data
        .provider
        .get_block_with_txs(block_hash)
        .await
        .wrap_err(format!("error fetching block {}", block_hash))?
        .ok_or_else(|| eyre!("block with txs not available {}", block_hash))?;

    let outcome = BlockSync::sync_next_head(
        &app_data.sync,
        &app_data.provider,
        BlockWithTxs::from_ethers_block(block_with_txs)?,
    )
    .await?;

    info!(
        "synced block {:?} {:?}, outcome: {:?}",
        block.number, block.hash, outcome
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
