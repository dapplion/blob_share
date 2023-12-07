use std::sync::Arc;

use ethers::providers::Middleware;
use eyre::{Context, Result};

use crate::{
    debug, error,
    kzg::{construct_blob_tx, TxParams},
    warn, AppData, DataIntent, MAX_USABLE_BLOB_DATA_LEN, MIN_BLOB_DATA_TO_PUBLISH,
};

pub(crate) async fn blob_sender_task(app_data: Arc<AppData>) -> Result<()> {
    loop {
        app_data.notify.notified().await;
        if let Err(e) = maybe_send_blob_tx(app_data.clone()).await {
            if app_data.config.panic_on_background_task_errors {
                return Err(e);
            } else {
                error!("error sending blob tx {e:?}");
            }
        }
    }
}

pub(crate) async fn maybe_send_blob_tx(app_data: Arc<AppData>) -> Result<()> {
    let wei_per_byte = 1; // TODO: Fetch from chain

    let next_blob_items = {
        let mut items = app_data.data_intent_tracker.get_all_pending().await;
        if let Some(next_blob_items) = select_next_blob_items(items.as_mut_slice(), wei_per_byte) {
            next_blob_items
        } else {
            debug!("no viable set of items for blob");
            return Ok(());
        }
    };

    let data_intent_ids = next_blob_items
        .iter()
        .map(|item| item.id())
        .collect::<Vec<_>>();

    debug!(
        "selected {} items for blob tx: {:?}",
        next_blob_items.len(),
        data_intent_ids
    );

    let gas_config = app_data.gas_tracker.estimate(&app_data.provider).await?;

    let tx_params = TxParams {
        chain_id: app_data.chain_id,
        nonce: 0,
    };

    let blob_tx = construct_blob_tx(
        &app_data.kzg_settings,
        &app_data.publish_config,
        &gas_config,
        &tx_params,
        &app_data.sender_wallet,
        next_blob_items,
    )?;

    // Declare items as pending on the computed tx_hash
    app_data
        .data_intent_tracker
        .mark_items_as_pending(&data_intent_ids, blob_tx.tx_hash)
        .await
        .wrap_err("consistency error with blob_tx intents")?;

    // TODO: do not await here, spawn another task
    // TODO: monitor transaction, if gas is insufficient return data intents to the pool
    let tx = app_data
        .provider
        .send_raw_transaction(blob_tx.blob_tx_networking.clone())
        .await
        .wrap_err_with(|| {
            if let Err(e) = std::fs::write(
                format!(
                    "invalid_tx_blob_networking_{}.rlp",
                    hex::encode(blob_tx.tx_hash)
                ),
                blob_tx.blob_tx_networking,
            ) {
                warn!(
                    "error persisting invalid tx {} {e:?}",
                    hex::encode(blob_tx.tx_hash)
                );
            }

            format!("error sending blob tx {}", hex::encode(blob_tx.tx_hash))
        })?;

    // Sanity check on correct hash
    if blob_tx.tx_hash != tx.tx_hash() {
        warn!(
            "internally computed transaction hash {} does not match returned hash {}",
            blob_tx.tx_hash,
            tx.tx_hash()
        );
    }

    app_data
        .sync
        .register_pending_blob_tx(blob_tx.tx_summary)
        .await;

    Ok(())
}

// TODO: write optimizer algo to find a better distribution
// TODO: is ok to represent wei units as usize?
fn select_next_blob_items(
    items: &mut [DataIntent],
    _blob_gas_price: u128,
) -> Option<Vec<DataIntent>> {
    // Sort items by data length
    items.sort_by(|a, b| {
        a.data
            .len()
            .cmp(&b.data.len())
            .then_with(|| b.max_cost_wei.cmp(&a.max_cost_wei))
    });

    let mut intents_for_blob: Vec<&DataIntent> = vec![];
    let mut used_len = 0;

    for item in items {
        if used_len + item.data.len() > MAX_USABLE_BLOB_DATA_LEN {
            break;
        }

        used_len += item.data.len();
        intents_for_blob.push(item);
    }

    if used_len < MIN_BLOB_DATA_TO_PUBLISH {
        return None;
    }

    Some(intents_for_blob.into_iter().cloned().collect())
}

#[cfg(test)]
mod tests {
    use ethers::types::{Signature, H160};

    use crate::{DataIntent, MAX_USABLE_BLOB_DATA_LEN};

    use super::select_next_blob_items;

    #[test]
    fn select_next_blob_items_case_no_items() {
        run_select_next_blob_items_test(&[], 1, None);
    }

    #[test]
    fn select_next_blob_items_case_one_small() {
        run_select_next_blob_items_test(&[(MAX_USABLE_BLOB_DATA_LEN / 4, 1)], 1, None);
    }

    #[test]
    fn select_next_blob_items_case_one_big() {
        run_select_next_blob_items_test(
            &[(MAX_USABLE_BLOB_DATA_LEN, 1)],
            1,
            Some(&[(MAX_USABLE_BLOB_DATA_LEN, 1)]),
        );
    }

    #[test]
    fn select_next_blob_items_case_multiple_small() {
        run_select_next_blob_items_test(
            &[
                (MAX_USABLE_BLOB_DATA_LEN / 4, 1),
                (MAX_USABLE_BLOB_DATA_LEN / 4, 2),
                (MAX_USABLE_BLOB_DATA_LEN / 2, 3),
                (MAX_USABLE_BLOB_DATA_LEN / 2, 4),
            ],
            1,
            Some(&[
                (MAX_USABLE_BLOB_DATA_LEN / 4, 2),
                (MAX_USABLE_BLOB_DATA_LEN / 4, 1),
                (MAX_USABLE_BLOB_DATA_LEN / 2, 4),
            ]),
        );
    }

    fn run_select_next_blob_items_test(
        all_items: &[(usize, u128)],
        blob_gas_price: u128,
        selected_items: Option<&[(usize, u128)]>,
    ) {
        let mut all_items = generate_data_intents(all_items);
        let selected_items = selected_items.map(|items| generate_data_intents(items));

        assert_eq!(
            items_to_summary(select_next_blob_items(
                all_items.as_mut_slice(),
                blob_gas_price
            )),
            items_to_summary(selected_items)
        )
    }

    fn items_to_summary(items: Option<Vec<DataIntent>>) -> Option<Vec<String>> {
        items.map(|items| {
            items
                .iter()
                .map(|d| {
                    format!(
                        "(MAX / {}, {})",
                        MAX_USABLE_BLOB_DATA_LEN / d.data.len(),
                        d.max_cost_wei
                    )
                })
                .collect()
        })
    }

    fn generate_data_intents(items: &[(usize, u128)]) -> Vec<DataIntent> {
        items
            .iter()
            .map(|(data_len, max_cost_wei)| generate_data_intent(*data_len, *max_cost_wei))
            .collect()
    }

    fn generate_data_intent(data_len: usize, max_cost_wei: u128) -> DataIntent {
        DataIntent {
            from: H160([0xff; 20]),
            data: vec![0xbb; data_len],
            data_hash: [0xaa; 32].into(),
            signature: Signature {
                r: 0.into(),
                s: 0.into(),
                v: 0,
            },
            max_cost_wei,
        }
    }
}
