use std::{str::FromStr, time::Duration};

use crate::{
    geth_helpers::get_wallet_genesis_funds,
    helpers::{TestHarness, ADDRESS_ZERO},
};
use blob_share::{
    client::{DataIntentId, DataIntentStatus},
    MAX_USABLE_BLOB_DATA_LEN,
};
use ethers::types::{Address, H256};
use eyre::Result;
use tokio::time::sleep;

#[tokio::test]
async fn health_check_works() {
    let testing_harness = TestHarness::spawn_with_el_only().await;
    testing_harness.client.health().await.unwrap();
}

#[tokio::test]
async fn post_two_intents_and_expect_blob_tx() -> Result<()> {
    let testing_harness = TestHarness::spawn_with_chain().await;

    // Submit data intent
    let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 3];
    let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2];

    let wallet = testing_harness.get_wallet_genesis_funds();
    // Fund account
    testing_harness.fund_sender_account(&wallet).await;

    // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
    let intent_1_id = testing_harness
        .post_data(&wallet.signer(), data_1.clone())
        .await;

    // Check data intent is stored
    let intents = testing_harness.client.get_data().await?;
    assert_eq!(intents.len(), 1);

    let intent_2_id = testing_harness
        .post_data(&wallet.signer(), data_2.clone())
        .await;

    sleep(Duration::from_millis(100)).await;

    // After sending enough intents the blob transaction should be emitted
    let intents = testing_harness.client.get_data().await?;
    assert_eq!(intents.len(), 0);

    let intent_1_tx = testing_harness
        .wait_for_pending_intent(&intent_1_id, Duration::from_secs(1))
        .await
        .unwrap();
    let intent_2_tx = testing_harness
        .wait_for_pending_intent(&intent_2_id, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        intent_1_tx, intent_2_tx,
        "two intents should be in the same tx"
    );

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
