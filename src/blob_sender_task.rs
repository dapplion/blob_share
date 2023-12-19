use std::sync::Arc;

use ethers::signers::Signer;
use eyre::{Context, Result};

use crate::{
    debug,
    gas::GasConfig,
    kzg::{construct_blob_tx, BlobTx, TxParams},
    metrics,
    packing::{pack_items, Item},
    warn, AppData, DataIntent, MAX_USABLE_BLOB_DATA_LEN,
};

pub(crate) async fn blob_sender_task(app_data: Arc<AppData>) -> Result<()> {
    let mut id = 0_u64;

    loop {
        // Race a notify signal with an interrupt from the OS
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = app_data.notify.notified() => {}
        }

        loop {
            id += 1;

            match maybe_send_blob_tx(app_data.clone(), id).await {
                Ok(outcome) => {
                    match outcome {
                        // Loop again to try to create another blob transaction
                        SendResult::SentBlobTx => continue,
                        // Break out of inner loop and wait for new notification
                        SendResult::NoViableSet | SendResult::NoNonceAvailable => break,
                    }
                }
                Err(e) => {
                    if app_data.config.panic_on_background_task_errors {
                        return Err(e);
                    } else {
                        metrics::BLOB_SENDER_TASK_ERRORS.inc();
                        // TODO: Review if breaking out of the inner loop is the best outcome
                        break;
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum SendResult {
    NoViableSet,
    SentBlobTx,
    NoNonceAvailable,
}

#[tracing::instrument(ret, err, skip(app_data), fields(id = _id))]
// `_id` argument is used by tracing to track all internal log lines
pub(crate) async fn maybe_send_blob_tx(app_data: Arc<AppData>, _id: u64) -> Result<SendResult> {
    let _timer = metrics::BLOB_SENDER_TASK_TIMES.start_timer();

    let max_fee_per_blob_gas = app_data
        .sync
        .read()
        .await
        .get_head_gas()
        .blob_gas_price_next_block();

    let next_blob_items = {
        let items = app_data.data_intent_tracker.read().await.get_all_pending();
        debug!(
            "attempting to pack valid blob, max_fee_per_blob_gas {} items {}",
            max_fee_per_blob_gas,
            items.len()
        );
        let _timer_pck = metrics::PACKING_TIMES.start_timer();

        if let Some(next_blob_items) = select_next_blob_items(&items, max_fee_per_blob_gas) {
            next_blob_items
        } else {
            return Ok(SendResult::NoViableSet);
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

    // TODO: Check if it's necessary to do a round-trip to the EL to estimate gas
    let (max_fee_per_gas, max_priority_fee_per_gas) =
        app_data.provider.estimate_eip1559_fees().await?;
    let gas_config = GasConfig {
        max_fee_per_gas: max_fee_per_gas.as_u128(),
        max_priority_fee_per_gas: max_priority_fee_per_gas.as_u128(),
        max_fee_per_blob_gas,
    };

    let sender_address = app_data.sender_wallet.address();

    // Make getting the nonce reliable + heing able to send multiple txs at once
    let nonce = if let Some(nonce) = app_data
        .sync
        .write()
        .await
        .reserve_next_available_nonce(&app_data.provider, sender_address)
        .await?
    {
        nonce
    } else {
        return Ok(SendResult::NoNonceAvailable);
    };

    let blob_tx =
        match construct_and_send_tx(app_data.clone(), nonce, &gas_config, next_blob_items).await {
            Ok(blob_tx) => blob_tx,
            Err(e) => {
                app_data
                    .sync
                    .write()
                    .await
                    .unreserve_nonce(sender_address, nonce);
                return Err(e);
            }
        };

    // TODO: Assumes this function is never called concurrently. Therefore it can afford to
    // register the transaction when it has been accepted by the execution node. If this function
    // can be called concurrently it should mark the items as 'Pending' before sending the
    // transaction to the execution client, and remove them if there's an error.
    //
    // Declare items as pending on the computed tx_hash
    {
        // Grab the lock of both data_intent_tracker and sync at once to ensure data intent status
        // is consistent in both structs
        let mut data_intent_tracker = app_data.data_intent_tracker.write().await;
        let mut sync = app_data.sync.write().await;
        data_intent_tracker
            .include_in_blob_tx(&data_intent_ids, blob_tx.tx_hash)
            .wrap_err("consistency error with blob_tx intents")?;
        sync.register_pending_blob_tx(blob_tx.tx_summary)
            .wrap_err("consistency error with blob_tx")?;
    }

    Ok(SendResult::SentBlobTx)
}

#[tracing::instrument(skip(app_data, gas_config, next_blob_items))]
async fn construct_and_send_tx(
    app_data: Arc<AppData>,
    nonce: u64,
    gas_config: &GasConfig,
    next_blob_items: Vec<DataIntent>,
) -> Result<BlobTx> {
    let intent_ids = next_blob_items.iter().map(|i| i.id()).collect::<Vec<_>>();

    let tx_params = TxParams {
        chain_id: app_data.chain_id,
        nonce,
    };

    let blob_tx = construct_blob_tx(
        &app_data.kzg_settings,
        &app_data.publish_config,
        gas_config,
        &tx_params,
        &app_data.sender_wallet,
        next_blob_items,
    )?;

    metrics::PACKED_BLOB_USED_LEN.observe(blob_tx.tx_summary.used_bytes as f64);
    metrics::PACKED_BLOB_ITEMS.observe(blob_tx.tx_summary.participants.len() as f64);

    debug!(
        "sending blob transaction {} with intents {:?}: {:?}",
        blob_tx.tx_hash, intent_ids, blob_tx.tx_summary
    );

    // TODO: do not await here, spawn another task
    // TODO: monitor transaction, if gas is insufficient return data intents to the pool
    let tx_hash = app_data
        .provider
        .send_raw_transaction(blob_tx.blob_tx_networking.clone())
        .await
        .wrap_err_with(|| {
            if let Err(e) = std::fs::write(
                format!(
                    "invalid_tx_blob_networking_{}.rlp",
                    hex::encode(blob_tx.tx_hash)
                ),
                blob_tx.blob_tx_networking.clone(),
            ) {
                warn!(
                    "error persisting invalid tx {} {e:?}",
                    hex::encode(blob_tx.tx_hash)
                );
            }

            format!(
                "error sending blob tx {}: {:?}",
                hex::encode(blob_tx.tx_hash),
                blob_tx.tx_summary
            )
        })?;

    // Sanity check on correct hash
    if blob_tx.tx_hash != tx_hash {
        warn!(
            "internally computed transaction hash {} does not match returned hash {}",
            blob_tx.tx_hash, tx_hash
        );
    }

    Ok(blob_tx)
}

// TODO: write optimizer algo to find a better distribution
// TODO: is ok to represent wei units as usize?
#[tracing::instrument(skip(data_intents))]
fn select_next_blob_items(
    data_intents: &[DataIntent],
    blob_gas_price: u128,
) -> Option<Vec<DataIntent>> {
    let items: Vec<Item> = data_intents
        .iter()
        .map(|e| (e.data().len(), e.max_blob_gas_price()))
        .collect::<Vec<_>>();

    pack_items(&items, MAX_USABLE_BLOB_DATA_LEN, blob_gas_price).map(|selected_indexes| {
        selected_indexes
            .iter()
            // TODO: do not copy data
            .map(|i| data_intents[*i].clone())
            .collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use ethers::types::H160;

    use crate::{data_intent::DataIntentNoSignature, DataIntent, MAX_USABLE_BLOB_DATA_LEN};

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
                (MAX_USABLE_BLOB_DATA_LEN / 2, 3),
            ]),
        );
    }

    fn run_select_next_blob_items_test(
        all_items: &[(usize, u128)],
        blob_gas_price: u128,
        expected_selected_items: Option<&[(usize, u128)]>,
    ) {
        let mut all_items = generate_data_intents(all_items);
        let expected_selected_items =
            expected_selected_items.map(|items| generate_data_intents(items));

        let selected_items = select_next_blob_items(all_items.as_mut_slice(), blob_gas_price);

        assert_eq!(
            items_to_summary(selected_items),
            items_to_summary(expected_selected_items)
        )
    }

    fn items_to_summary(items: Option<Vec<DataIntent>>) -> Option<Vec<String>> {
        items.map(|mut items| {
            // Sort for stable comparision
            items.sort_by(|a, b| {
                a.data_len()
                    .cmp(&b.data_len())
                    .then_with(|| b.max_blob_gas_price().cmp(&a.max_blob_gas_price()))
            });

            items
                .iter()
                .map(|d| {
                    format!(
                        "(MAX / {}, {})",
                        MAX_USABLE_BLOB_DATA_LEN / d.data_len(),
                        d.max_blob_gas_price()
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

    fn generate_data_intent(data_len: usize, max_blob_gas_price: u128) -> DataIntent {
        DataIntent::NoSignature(DataIntentNoSignature {
            from: H160([0xff; 20]),
            data: vec![0xbb; data_len],
            data_hash: [0xaa; 32].into(),
            max_blob_gas_price,
        })
    }
}
