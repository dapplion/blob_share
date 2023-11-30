use std::{str::FromStr, time::Duration};

use crate::{
    helpers::{TestHarness, ADDRESS_ZERO},
    spawn_geth,
};
use blob_share::MAX_USABLE_BLOB_DATA_LEN;
use ethers::{
    providers::{Http, Middleware, Provider},
    types::Address,
};
use tokio::time::sleep;

#[tokio::test]
async fn test_spawn_geth() {
    spawn_geth().await;
}

#[tokio::test]
async fn health_check_works() {
    let testing_harness = TestHarness::spawn().await;
    testing_harness.test_health().await;
}

#[tokio::test]
async fn post_single_data_intent() {
    let testing_harness = TestHarness::spawn().await;

    // Submit data intent
    let from = "0x0000000000000000000000000000000000000000".to_string();
    let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 2];

    testing_harness
        .post_data_intent(&from, data_1.clone(), "data_intent_1")
        .await;

    // Check data intent is stored
    let intents = testing_harness.get_data_intents().await;
    assert_eq!(intents.len(), 1);
}

#[tokio::test]
async fn post_two_intents_and_expect_blob_tx() {
    let testing_harness = TestHarness::spawn().await;

    // Submit data intent
    let from = "0x0000000000000000000000000000000000000000".to_string();
    let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 2];
    let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    testing_harness
        .post_data_intent(&from, data_1.clone(), "data_intent_1")
        .await;

    // Check data intent is stored
    let intents = testing_harness.get_data_intents().await;
    assert_eq!(intents.len(), 1);

    testing_harness
        .post_data_intent(&from, data_2.clone(), "data_intent_2")
        .await;

    sleep(Duration::from_millis(100)).await;

    // After sending enough intents the blob transaction should be emitted
    let intents = testing_harness.get_data_intents().await;
    assert_eq!(intents.len(), 0);

    testing_harness
        .wait_for_block(1, Duration::from_millis(200))
        .await;
    let tx = testing_harness.get_block_first_tx(1).await;
    let expected_data = [data_1, data_2].concat();
    // Assert correctly constructed transaction
    assert_eq!(hex::encode(&tx.input), hex::encode(expected_data));
    assert_eq!(tx.to, Some(Address::from_str(ADDRESS_ZERO).unwrap()));
}
