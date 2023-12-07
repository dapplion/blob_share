use std::sync::Arc;

use ethers::{
    providers::{Middleware, Provider, StreamExt, Ws},
    types::{Block, TxHash},
};
use eyre::{eyre, Result};

use crate::{
    debug, error, info,
    sync::{BlockSync, BlockWithTxs},
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
        if let Err(e) = sync_block(&app_data.provider, &app_data.sync, &block).await {
            if app_data.config.panic_on_background_task_errors {
                return Err(e);
            } else {
                error!(
                    "error syncing block {:?} {:?}: {:?}",
                    block.number, block.hash, e
                );
            }
        }

        // Maybe compute new blob transactions
        app_data.notify.notify_one();
    }

    Ok(())
}

async fn sync_block(
    provider: &Provider<Ws>,
    sync: &BlockSync,
    block: &Block<TxHash>,
) -> Result<()> {
    let block_hash = block
        .hash
        .ok_or_else(|| eyre!("block has no hash {:?}", block.number))?;

    let block_with_txs = provider
        .get_block_with_txs(block_hash)
        .await?
        .ok_or_else(|| eyre!("block with txs not available {}", block_hash))?;

    let sync_block_outcome = sync
        .sync_next_block(provider, BlockWithTxs::from_ethers_block(block_with_txs)?)
        .await?;

    info!(
        "synced block {:?} {:?} outcome {:?}",
        block.number, block.hash, sync_block_outcome
    );

    Ok(())
}
