use std::sync::Arc;

use eyre::Result;
use tokio::time::{self, Duration};

use crate::{debug, error, metrics, AppData};

pub(crate) async fn remote_node_tracker_task(app_data: Arc<AppData>) -> Result<()> {
    debug!("starting remote node tracker task");

    let mut interval = time::interval(Duration::from_secs(12));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = interval.tick() => {}
        }

        match app_data.fetch_remote_node_latest_block_number().await {
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
