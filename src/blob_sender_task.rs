use std::sync::Arc;

use ethers::{signers::Signer, types::TxHash};
use eyre::{Context, Result};

use crate::{
    blob_tx_data::BlobTxParticipant,
    data_intent_tracker::DataIntentDbRowFull,
    debug,
    gas::GasConfig,
    kzg::{construct_blob_tx, BlobTx, TxParams},
    metrics,
    packing::{pack_items, Item},
    sync::NonceStatus,
    utils::address_from_vec,
    warn, AppData, MAX_USABLE_BLOB_DATA_LEN,
};

/// Limit the maximum number of times a data intent included in a previous transaction can be
/// included again in a new transaction.
const MAX_PREVIOUS_INCLUSIONS: usize = 2;

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
                        SendResult::SentBlobTx(_) => continue,
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
    SentBlobTx(TxHash),
    NoNonceAvailable,
}

#[tracing::instrument(ret, err, skip(app_data), fields(id = _id))]
// `_id` argument is used by tracing to track all internal log lines
pub(crate) async fn maybe_send_blob_tx(app_data: Arc<AppData>, _id: u64) -> Result<SendResult> {
    let _timer = metrics::BLOB_SENDER_TASK_TIMES.start_timer();

    // Sync available intents
    app_data.sync_data_intents().await?;

    let max_fee_per_blob_gas = app_data.blob_gas_price_next_head_block().await;

    let data_intent_summaries = {
        let (pending_data_intents, items_from_previous_inclusions) = app_data
            .get_all_intents_available_for_packing(MAX_PREVIOUS_INCLUSIONS)
            .await;

        let items: Vec<Item> = pending_data_intents
            .iter()
            .map(|e| Item::new(e.data_len, e.max_blob_gas_price))
            .collect::<Vec<_>>();

        debug!(
            "attempting to pack valid blob, max_fee_per_blob_gas {} items_from_previous_inclusions {} items {:?}",
            max_fee_per_blob_gas, items_from_previous_inclusions, items
        );

        let _timer_pck = metrics::PACKING_TIMES.start_timer();

        if let Some(selected_indexes) = pack_items(
            &items,
            MAX_USABLE_BLOB_DATA_LEN,
            max_fee_per_blob_gas.try_into()?,
        ) {
            selected_indexes
                .iter()
                .map(|i| pending_data_intents[*i].clone())
                .collect::<Vec<_>>()
        } else {
            return Ok(SendResult::NoViableSet);
        }
    };

    let data_intent_ids = data_intent_summaries
        .iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();

    debug!(
        "selected {} items for blob tx: {:?}",
        data_intent_summaries.len(),
        data_intent_ids
    );

    // TODO: Do this sequence atomic, lock data intent rows here
    let data_intents = app_data.data_intents_by_id(&data_intent_ids).await?;

    // TODO: Check if it's necessary to do a round-trip to the EL to estimate gas
    //
    // ### EIP-1559 refresher:
    // - assert tx.max_fee_per_gas >= tx.max_priority_fee_per_gas
    // - priority_fee_per_gas = min(tx.max_priority_fee_per_gas, tx.max_fee_per_gas - block.base_fee_per_gas)
    let (max_fee_per_gas, max_priority_fee_per_gas) =
        app_data.provider.estimate_eip1559_fees().await?;

    let mut gas_config = GasConfig {
        max_fee_per_gas: max_fee_per_gas.as_u128(),
        max_priority_fee_per_gas: max_priority_fee_per_gas.as_u128(),
        max_fee_per_blob_gas,
    };
    debug!("gas_config {:?}", gas_config);

    let sender_address = app_data.sender_wallet.address();

    // Make getting the nonce reliable + heing able to send multiple txs at once
    let nonce = match app_data.get_next_available_nonce(sender_address).await? {
        NonceStatus::NotAvailable => return Ok(SendResult::NoNonceAvailable),
        NonceStatus::Available(nonce) => nonce,
        NonceStatus::Repriced(nonce, prev_tx_gas) => {
            gas_config.reprice_to_at_least(prev_tx_gas);
            nonce
        }
    };

    let blob_tx =
        match construct_and_send_tx(app_data.clone(), nonce, &gas_config, data_intents).await {
            Ok(blob_tx) => blob_tx,
            Err(e) => {
                // TODO: consider persisting the existance of this transaction before sending it out
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
        app_data
            .register_sent_blob_tx(&data_intent_ids, blob_tx.tx_summary)
            .await?;

        // TODO: Review when it's best to re-sync the data_intent_tracker
        app_data.sync_data_intents().await?;
    }

    Ok(SendResult::SentBlobTx(blob_tx.tx_hash))
}

#[tracing::instrument(skip(app_data, gas_config, next_blob_items))]
async fn construct_and_send_tx(
    app_data: Arc<AppData>,
    nonce: u64,
    gas_config: &GasConfig,
    next_blob_items: Vec<DataIntentDbRowFull>,
) -> Result<BlobTx> {
    let intent_ids = next_blob_items.iter().map(|i| i.id).collect::<Vec<_>>();

    let tx_params = TxParams {
        chain_id: app_data.chain_id,
        nonce,
    };

    let participants = next_blob_items
        .iter()
        .map(|item| {
            Ok(BlobTxParticipant {
                address: address_from_vec(&item.eth_address)?,
                data_len: item.data.len(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let datas = next_blob_items
        .into_iter()
        .map(|item| item.data)
        .collect::<Vec<_>>();

    let blob_tx = construct_blob_tx(
        &app_data.kzg_settings,
        app_data.config.l1_inbox_address,
        gas_config,
        &tx_params,
        &app_data.sender_wallet,
        participants,
        datas,
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
