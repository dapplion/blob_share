use std::{str::FromStr, time::Duration};

use crate::{
    geth_helpers::get_wallet,
    helpers::{TestHarness, ADDRESS_ZERO},
    spawn_geth,
};
use blob_share::MAX_USABLE_BLOB_DATA_LEN;
use ethers::{
    providers::Middleware,
    types::{Address, TransactionRequest},
    utils::parse_ether,
};
use eyre::Result;
use tokio::time::sleep;

#[ignore]
#[tokio::test]
async fn test_spawn_geth() {
    spawn_geth().await;
}

#[ignore]
#[tokio::test]
async fn health_check_works() {
    let testing_harness = TestHarness::spawn().await;
    testing_harness.client.health().await.unwrap();
}

#[ignore]
#[tokio::test]
async fn post_single_data_intent() {
    let testing_harness = TestHarness::spawn().await;

    // TODO: automate
    let eth_provider_url = "http://localhost:8545";
    let chain_id = 999;
    let wallet = get_wallet(eth_provider_url, chain_id).unwrap();

    // Submit data intent
    let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 2];

    testing_harness
        .post_data(&wallet.signer(), data_1.clone(), "data_intent_1")
        .await;

    // Check data intent is stored
    let intents = testing_harness.get_data().await;
    assert_eq!(intents.len(), 1);
}

#[ignore]
#[tokio::test]
async fn post_two_intents_and_expect_blob_tx() -> Result<()> {
    let testing_harness = TestHarness::spawn().await;

    // Submit data intent
    let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 2];
    let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];

    // TODO: automate
    let eth_provider_url = "http://localhost:8545";
    let chain_id = 999;
    let sender = testing_harness.get_sender().await;
    let wallet = get_wallet(eth_provider_url, chain_id)?;

    // Fund account
    let tx = TransactionRequest::new()
        .from(wallet.address())
        .to(sender.address)
        .value(parse_ether("0.1")?);
    let tx = wallet.send_transaction(tx, None).await?;
    tx.confirmations(1).await?;

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    let intent_1_id = testing_harness
        .post_data(&wallet.signer(), data_1.clone(), "intent_1")
        .await;

    // Check data intent is stored
    let intents = testing_harness.get_data().await;
    assert_eq!(intents.len(), 1);

    let intent_2_id = testing_harness
        .post_data(&wallet.signer(), data_2.clone(), "intent_2")
        .await;

    sleep(Duration::from_millis(100)).await;

    // After sending enough intents the blob transaction should be emitted
    let intents = testing_harness.get_data().await;
    assert_eq!(intents.len(), 0);

    let tx = testing_harness
        .wait_for_block_with_tx_from(
            testing_harness.sender_address(),
            Duration::from_millis(20000),
            Some(Duration::from_millis(500)),
        )
        .await;
    let expected_data = [data_1, data_2].concat();
    // Assert correctly constructed transaction
    assert_eq!(hex::encode(&tx.input), hex::encode(expected_data));
    assert_eq!(tx.to, Some(Address::from_str(ADDRESS_ZERO).unwrap()));

    Ok(())
}
