use std::sync::Arc;

use eyre::Result;
use tokio::time;

use crate::{debug, error, info, metrics, AppData};

/// Check interval for stale intent eviction. We don't need to check very often since the
/// threshold is measured in hours.
const EVICTION_CHECK_INTERVAL_SECS: u64 = 300; // 5 minutes

pub(crate) async fn evict_stale_intents_task(app_data: Arc<AppData>) -> Result<()> {
    if app_data.config.evict_stale_intent_duration.is_zero() {
        debug!("stale intent eviction disabled");
        return Ok(());
    }

    debug!("starting stale intent eviction task");

    let max_age = chrono::Duration::from_std(app_data.config.evict_stale_intent_duration)
        .unwrap_or(chrono::Duration::max_value());

    let mut interval = time::interval(time::Duration::from_secs(EVICTION_CHECK_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = interval.tick() => {}
        }

        match app_data.evict_stale_underpriced_intents(max_age).await {
            Ok(count) => {
                if count > 0 {
                    info!("Evicted {count} stale underpriced intents");
                    metrics::EVICTED_STALE_INTENTS.inc_by(count as f64);
                }
            }
            Err(e) => {
                error!("Error evicting stale intents: {e:?}");
            }
        }
    }
}
