use std::sync::Arc;

use ethers::{
    signers::Signer,
    types::{
        transaction::eip2718::TypedTransaction, Eip1559TransactionRequest, NameOrAddress, TxHash,
    },
};
use eyre::{eyre, Context, Result};

use crate::{
    backoff::BackoffState,
    blob_tx_data::BlobTxParticipant,
    data_intent_tracker::DataIntentDbRowFull,
    debug, error,
    gas::GasConfig,
    info,
    kzg::{construct_blob_tx, BlobTx, TxParams},
    metrics,
    packing::{pack_items, Item},
    sync::NonceStatus,
    utils::address_from_vec,
    warn, AppData, MAX_USABLE_BLOB_DATA_LEN,
};

pub(crate) async fn blob_sender_task(app_data: Arc<AppData>) -> Result<()> {
    let mut id = 0_u64;
    let mut backoff = BackoffState::new("blob_sender_task");

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
                    backoff.on_success();
                    match outcome {
                        // Loop again to try to create another blob transaction
                        SendResult::SentBlobTx(_) => continue,
                        // Self-transfer sent to resolve nonce deadlock; break and wait
                        // for the next block to confirm it before sending more blob txs
                        SendResult::SentSelfTransfer(_) => break,
                        // Break out of inner loop and wait for new notification
                        SendResult::NoViableSet | SendResult::NoNonceAvailable => break,
                    }
                }
                Err(e) => {
                    if app_data.config.panic_on_background_task_errors {
                        return Err(e);
                    }

                    metrics::BLOB_SENDER_TASK_ERRORS.inc();
                    metrics::BLOB_SENDER_TASK_RETRIES.inc();
                    error!("error on send blob tx {:?}", e);

                    // Back off before retrying to avoid tight error loops on
                    // transient failures (network timeouts, DB connection issues)
                    backoff.on_error().await;

                    // Break out to wait for next notification rather than
                    // immediately retrying, since the error is likely
                    // still present
                    break;
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum SendResult {
    NoViableSet,
    SentBlobTx(#[allow(dead_code)] TxHash),
    SentSelfTransfer(#[allow(dead_code)] TxHash),
    NoNonceAvailable,
}

#[tracing::instrument(ret, err, skip(app_data, _id), fields(id = _id))]
// `_id` argument is used by tracing to track all internal log lines
pub(crate) async fn maybe_send_blob_tx(app_data: Arc<AppData>, _id: u64) -> Result<SendResult> {
    let _timer = metrics::BLOB_SENDER_TASK_TIMES.start_timer();

    // Only allow to send blob transactions if synced with remote node
    app_data.assert_node_synced().await?;

    // Sync available intents
    app_data.sync_data_intents().await?;

    let max_fee_per_blob_gas = app_data.blob_gas_price_next_head_block().await;

    let data_intent_summaries = {
        let (mut pending_data_intents, items_from_previous_inclusions) = app_data
            .get_all_intents_available_for_packing(max_fee_per_blob_gas as u64)
            .await;

        // Sort ascending before creating items to preserve order
        pending_data_intents.sort_by(|a, b| a.data_len.cmp(&b.data_len));

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
            // No viable set of intents at current gas prices. Check if this is
            // caused by a nonce deadlock: all pending blob txs are underpriced
            // and cannot be repriced because no intents can afford current gas.
            if let Some((stuck_nonce, prev_gas)) = app_data.detect_nonce_deadlock().await {
                return send_self_transfer(app_data, stuck_nonce, prev_gas).await;
            }
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

    metrics::PACKED_BLOB_USED_LEN.observe(blob_tx.tx_summary.used_bytes as f64);
    metrics::PACKED_BLOB_ITEMS.observe(blob_tx.tx_summary.participants.len() as f64);

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
            let data = item
                .data
                .as_ref()
                .ok_or_else(|| eyre!("intent {} has been pruned, cannot pack", item.id))?;
            Ok(BlobTxParticipant {
                address: address_from_vec(&item.eth_address)?,
                data_len: data.len(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let datas = next_blob_items
        .into_iter()
        .map(|item| {
            item.data
                .ok_or_else(|| eyre!("intent {} has been pruned, cannot pack", item.id))
        })
        .collect::<Result<Vec<_>>>()?;

    let blob_tx = construct_blob_tx(
        &app_data.kzg_settings,
        app_data.config.l1_inbox_address,
        gas_config,
        &tx_params,
        &app_data.sender_wallet,
        participants,
        datas,
    )?;

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

/// Send a 0-value self-transfer (EIP-1559) to resolve a nonce deadlock.
///
/// When all pending blob transactions are underpriced and no viable set of intents
/// can afford current gas prices, the sender nonce becomes stuck. This function
/// sends a minimal transaction (sender → sender, value 0) at the stuck nonce with
/// gas prices bumped above the previous transaction, which advances the nonce and
/// unblocks the pipeline.
#[tracing::instrument(skip(app_data))]
async fn send_self_transfer(
    app_data: Arc<AppData>,
    stuck_nonce: u64,
    prev_gas: GasConfig,
) -> Result<SendResult> {
    let sender_address = app_data.sender_wallet.address();

    // Estimate current gas prices
    let (max_fee_per_gas, max_priority_fee_per_gas) =
        app_data.provider.estimate_eip1559_fees().await?;

    let mut gas_config = GasConfig {
        max_fee_per_gas: max_fee_per_gas.as_u128(),
        max_priority_fee_per_gas: max_priority_fee_per_gas.as_u128(),
        // Not relevant for a non-blob tx, but keep for tracking
        max_fee_per_blob_gas: 0,
    };
    // Ensure gas prices are at least 110% of the stuck transaction
    gas_config.reprice_to_at_least(prev_gas);

    warn!(
        "nonce deadlock detected: sending self-transfer at nonce {} with gas {:?} (previous gas: {:?})",
        stuck_nonce, gas_config, prev_gas
    );

    // Build an EIP-1559 self-transfer: sender → sender, value 0, empty data
    let tx_request = Eip1559TransactionRequest {
        from: Some(sender_address),
        to: Some(NameOrAddress::Address(sender_address)),
        gas: Some(21_000u64.into()), // Simple transfer gas cost
        value: Some(0u64.into()),
        data: None,
        nonce: Some(stuck_nonce.into()),
        access_list: Default::default(),
        max_priority_fee_per_gas: Some(gas_config.max_priority_fee_per_gas.into()),
        max_fee_per_gas: Some(gas_config.max_fee_per_gas.into()),
        chain_id: Some(app_data.chain_id.into()),
    };

    let typed_tx: TypedTransaction = tx_request.into();
    let signature = app_data
        .sender_wallet
        .sign_transaction(&typed_tx)
        .await
        .wrap_err("failed to sign self-transfer transaction")?;
    let signed_tx = typed_tx.rlp_signed(&signature);

    let tx_hash = app_data
        .provider
        .send_raw_transaction(signed_tx)
        .await
        .wrap_err("failed to send self-transfer transaction")?;

    info!(
        "sent self-transfer tx {} at nonce {} to resolve nonce deadlock",
        tx_hash, stuck_nonce
    );

    // Register the self-transfer in BlockSync so nonce tracking stays consistent
    app_data
        .register_sent_self_transfer(stuck_nonce, tx_hash, gas_config)
        .await;

    metrics::NONCE_DEADLOCK_SELF_TRANSFERS.inc();

    Ok(SendResult::SentSelfTransfer(tx_hash))
}
