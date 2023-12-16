use std::{str::FromStr, time::Duration};

use crate::helpers::{retry_with_timeout, TestHarness, TestMode, ADDRESS_ZERO, MS_100};
use blob_share::MAX_USABLE_BLOB_DATA_LEN;
use ethers::{providers::Middleware, types::Address};
use eyre::Result;
use log::LevelFilter;
use tokio::time::sleep;

#[tokio::test]
async fn health_check_works() {
    let testing_harness = TestHarness::spawn_with_el_only().await;
    testing_harness.client.health().await.unwrap();
}

#[tokio::test]
async fn post_two_intents_and_expect_blob_tx() {
    TestHarness::build(TestMode::WithChain)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness
                    .wait_for_app_health(Duration::from_secs(1))
                    .await?;

                // Submit data intent
                let data_1 = vec![0xaa_u8; MAX_USABLE_BLOB_DATA_LEN / 3];
                let data_2 = vec![0xbb_u8; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];

                let wallet = test_harness.get_wallet_genesis_funds();
                // Fund account
                test_harness.fund_sender_account(&wallet).await;

                // $ curl -vv localhost:8000/data -X POST -H "Content-Type: application/json" --data '{"from": "0x00", "data": "0x00", "max_price": 1}'
                let intent_1_id = test_harness
                    .post_data(&wallet.signer(), data_1.clone())
                    .await;
                test_harness
                    .wait_for_known_intent(&intent_1_id, MS_100)
                    .await?;

                // Check data intent is stored
                let intents = test_harness.client.get_data().await?;
                assert_eq!(intents.len(), 1);

                let intent_2_id = test_harness
                    .post_data(&wallet.signer(), data_2.clone())
                    .await;

                sleep(Duration::from_millis(100)).await;

                // After sending enough intents the blob transaction should be emitted
                let intents = test_harness.client.get_data().await?;
                assert_eq!(intents.len(), 0);

                let intent_1_txhash = test_harness
                    .wait_for_intent_inclusion_in_any_tx(&intent_1_id, Duration::from_secs(1))
                    .await?;
                let intent_2_txhash = test_harness
                    .wait_for_intent_inclusion_in_any_tx(&intent_2_id, Duration::from_secs(1))
                    .await?;
                assert_eq!(
                    intent_1_txhash, intent_2_txhash,
                    "two intents should be in the same tx"
                );

                let (intent_1_txhash_block, intent_1_block) = test_harness
                    .wait_for_intent_inclusion_in_any_block(
                        &intent_1_id,
                        Some(intent_1_txhash),
                        Duration::from_secs(10),
                    )
                    .await?;

                let block = test_harness
                    .eth_provider
                    .get_block_with_txs(intent_1_block)
                    .await?
                    .expect(&format!("block {intent_1_block} should be known"));
                let _intent_1_tx = block
                    .transactions
                    .iter()
                    .find(|tx| tx.hash == intent_1_txhash_block)
                    .expect("blob transaction not found in block");

                let blob_consumer = test_harness.get_blob_consumer(wallet.address());
                // Allow some time for the consensus client to persist the blobs and serve them
                let published_data = retry_with_timeout(
                    || async {
                        blob_consumer
                            .extract_data_participation_from_block(&block)
                            .await
                    },
                    Duration::from_secs(5),
                    Duration::from_millis(50),
                )
                .await?;

                // Assert that data returned by blob consumer matches the original publish
                assert_eq!(hex::encode(data_1), hex::encode(&published_data[0]));
                assert_eq!(hex::encode(data_2), hex::encode(&published_data[1]));

                Ok(())
            }
        })
        .await
        .unwrap();
}
