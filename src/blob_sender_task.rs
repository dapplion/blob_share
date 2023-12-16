use std::sync::Arc;

use ethers::{providers::Middleware, signers::Signer};
use eyre::{Context, Result};

use crate::{
    debug, error,
    kzg::{construct_blob_tx, TxParams},
    packing::{pack_items, Item},
    warn, AppData, DataIntent, MAX_USABLE_BLOB_DATA_LEN,
};

pub(crate) async fn blob_sender_task(app_data: Arc<AppData>) -> Result<()> {
    loop {
        app_data.notify.notified().await;

        loop {
            match maybe_send_blob_tx(app_data.clone()).await {
                // Loop again to try to create another blob transaction
                Ok(SendResult::SentBlobTx) => continue,
                // Break out of inner loop and wait for new notification
                Ok(SendResult::NoViableSet) => break,
                Err(e) => {
                    if app_data.config.panic_on_background_task_errors {
                        return Err(e);
                    } else {
                        error!("error sending blob tx {e:?}");
                        // TODO: Review if breaking out of the inner loop is the best outcome
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) enum SendResult {
    NoViableSet,
    SentBlobTx,
}

pub(crate) async fn maybe_send_blob_tx(app_data: Arc<AppData>) -> Result<SendResult> {
    let wei_per_byte = 1; // TODO: Fetch from chain

    let next_blob_items = {
        let items = app_data.data_intent_tracker.read().await.get_all_pending();
        if let Some(next_blob_items) = select_next_blob_items(&items, wei_per_byte) {
            next_blob_items
        } else {
            debug!("no viable set of items for blob, out of {}", items.len());
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

    let gas_config = app_data.gas_tracker.estimate(&app_data.provider).await?;
    let sender_address = app_data.sender_wallet.address();

    // Make getting the nonce reliable + heing able to send multiple txs at once

    let tx_params = TxParams {
        chain_id: app_data.chain_id,
        nonce: app_data
            .sync
            .read()
            .await
            .transaction_count_at_head_with_pending_tx(sender_address),
    };

    let blob_tx = construct_blob_tx(
        &app_data.kzg_settings,
        &app_data.publish_config,
        &gas_config,
        &tx_params,
        &app_data.sender_wallet,
        next_blob_items,
    )?;

    debug!(
        "sending blob transaction {}: {:?}",
        blob_tx.tx_hash, blob_tx.tx_summary
    );

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

            format!(
                "error sending blob tx {}: {:?}",
                hex::encode(blob_tx.tx_hash),
                blob_tx.tx_summary
            )
        })?;

    // Sanity check on correct hash
    if blob_tx.tx_hash != tx.tx_hash() {
        warn!(
            "internally computed transaction hash {} does not match returned hash {}",
            blob_tx.tx_hash,
            tx.tx_hash()
        );
    }

    // TODO: Assumes this function is never called concurrently. Therefore it can afford to
    // register the transaction when it has been accepted by the execution node. If this function
    // can be called concurrently it should mark the items as 'Pending' before sending the
    // transaction to the execution client, and remove them if there's an error.
    //
    // Declare items as pending on the computed tx_hash
    app_data
        .data_intent_tracker
        .write()
        .await
        .mark_items_as_pending(&data_intent_ids, blob_tx.tx_hash)
        .wrap_err("consistency error with blob_tx intents")?;
    app_data
        .sync
        .write()
        .await
        .register_pending_blob_tx(blob_tx.tx_summary)
        .wrap_err("consistency error with blob_tx")?;

    Ok(SendResult::SentBlobTx)
}

// TODO: write optimizer algo to find a better distribution
// TODO: is ok to represent wei units as usize?
fn select_next_blob_items(
    data_intents: &[DataIntent],
    blob_gas_price: u128,
) -> Option<Vec<DataIntent>> {
    let items: Vec<Item> = data_intents
        .iter()
        .map(|e| (e.data.len(), e.max_blob_gas_price))
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
                a.data
                    .len()
                    .cmp(&b.data.len())
                    .then_with(|| b.max_blob_gas_price.cmp(&a.max_blob_gas_price))
            });

            items
                .iter()
                .map(|d| {
                    format!(
                        "(MAX / {}, {})",
                        MAX_USABLE_BLOB_DATA_LEN / d.data.len(),
                        d.max_blob_gas_price
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
        DataIntent {
            from: H160([0xff; 20]),
            data: vec![0xbb; data_len],
            data_hash: [0xaa; 32].into(),
            signature: Signature {
                r: 0.into(),
                s: 0.into(),
                v: 0,
            },
            max_blob_gas_price,
        }
    }
}
