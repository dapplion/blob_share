use std::sync::Arc;

use ethers::types::BlockNumber;
use eyre::{eyre, Result};
use tokio::time::{self, Duration};

use crate::eth_provider::EthProvider;
use crate::{debug, error, metrics, AppData};

pub(crate) async fn remote_node_tracker_task(app_data: Arc<AppData>) -> Result<()> {
    debug!("starting remote node tracker task");

    let mut interval = time::interval(Duration::from_secs(12));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = interval.tick() => {}
        }

        match fetch_latest_block_number(&app_data.provider).await {
            Ok(block_number) => {
                debug!("Remote node head block number {block_number}");
                metrics::REMOTE_NODE_HEAD_BLOCK_NUMBER.set(block_number as f64);
            }
            Err(e) => {
                error!("Remote node fetch head block error {e:?}");
                metrics::REMOTE_NODE_HEAD_BLOCK_FETCH_ERRORS.inc();
            }
        }
    }
}

async fn fetch_latest_block_number(provider: &EthProvider) -> Result<u64> {
    let block = provider
        .get_block(BlockNumber::Latest)
        .await?
        .ok_or_else(|| eyre!("no latest block"))?;
    Ok(block
        .number
        .ok_or_else(|| eyre!("block has no number"))?
        .as_u64())
}
