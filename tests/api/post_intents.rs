use std::time::Duration;

use crate::helpers::{retry_with_timeout, TestHarness, TestMode};
use blob_share::MAX_USABLE_BLOB_DATA_LEN;
use ethers::signers::{LocalWallet, Signer};
use futures::future::join_all;

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
                    .await
                    .unwrap();

                // Fund account
                let wallet = test_harness.get_wallet_genesis_funds();
                test_harness.fund_sender_account(&wallet).await;

                test_post_two_data_intents_up_to_inclusion(&test_harness, wallet.signer(), 0).await;

                Ok(())
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn post_many_intents_series_and_expect_blob_tx() {
    TestHarness::build(TestMode::WithChain)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness
                    .wait_for_app_health(Duration::from_secs(1))
                    .await
                    .unwrap();

                // Fund account
                let wallet = test_harness.get_wallet_genesis_funds();
                test_harness.fund_sender_account(&wallet).await;

                for i in 0..10 {
                    test_post_two_data_intents_up_to_inclusion(&test_harness, wallet.signer(), i)
                        .await;
                }

                Ok(())
            }
        })
        .await
        .unwrap();
}

async fn test_post_two_data_intents_up_to_inclusion(
    test_harness: &TestHarness,
    wallet: &LocalWallet,
    i: u8,
) {
    // Submit data intent
    let data_1 = vec![0xa0_u8 + i; MAX_USABLE_BLOB_DATA_LEN / 3];
    let data_2 = vec![0xb0_u8 + i; MAX_USABLE_BLOB_DATA_LEN / 2 - 1];

    let intent_1_id = test_harness
        .post_data_and_wait_for_pending(wallet, data_1.clone())
        .await;

    // Check data intent is stored
    assert_eq!(test_harness.client.get_data().await.unwrap().len(), 1);

    let intent_2_id = test_harness
        .post_data_and_wait_for_pending(wallet, data_2.clone())
        .await;

    let intents_txhash = test_harness
        .wait_for_intent_inclusion_in_any_tx(&[intent_1_id, intent_2_id], Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        intents_txhash[0], intents_txhash[1],
        "two intents should be in the same tx"
    );

    let (_intent_1_txhash_block, intent_1_block) = test_harness
        .wait_for_intent_inclusion_in_any_block(
            &intent_1_id,
            Some(intents_txhash[0]),
            Duration::from_secs(10),
        )
        .await
        .unwrap();

    let blob_consumer = test_harness.get_blob_consumer(wallet.address());
    // Allow some time for the consensus client to persist the blobs and serve them
    let mut published_data = retry_with_timeout(
        || async {
            blob_consumer
                .extract_data_participation_from_block_hash(intent_1_block)
                .await
        },
        Duration::from_secs(5),
        Duration::from_millis(50),
    )
    .await
    .unwrap();

    // Note: this comparision is unstable, make sure each published data item
    // matches the intented data_i
    published_data.sort_by(|a, b| a.len().cmp(&b.len()));
    let expected_published_data = [data_1, data_2];

    // TODO: this comparision is unstable, make sure each published data item
    // matches the intented data_i
    // Assert that data returned by blob consumer matches the original publish
    assert_eq!(
        &published_data.iter().map(hex::encode).collect::<Vec<_>>(),
        &expected_published_data
            .iter()
            .map(hex::encode)
            .collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn post_many_intents_parallel_and_expect_blob_tx() {
    TestHarness::build(TestMode::WithChain)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness
                    .wait_for_app_health(Duration::from_secs(1))
                    .await?;

                // Fund account
                let wallet = test_harness.get_wallet_genesis_funds();
                test_harness.fund_sender_account(&wallet).await;

                // Num of intents to send at once
                const N: u64 = 32;

                let datas = (0..N as u8)
                    .map(|i| vec![0xa0_u8 + i; MAX_USABLE_BLOB_DATA_LEN / 2])
                    .collect::<Vec<_>>();

                // Post all datas at once
                let intent_ids = join_all(
                    datas
                        .iter()
                        .map(|data| test_harness.post_data(&wallet.signer(), data.to_vec()))
                        .collect::<Vec<_>>(),
                )
                .await;

                // All intents should be known immediatelly
                test_harness
                    .wait_for_known_intents(&intent_ids, Duration::from_secs(2))
                    .await
                    .unwrap();

                // Should dispatch transactions with all the data intents, done in serie can take
                // some time
                let intents_txhash = test_harness
                    .wait_for_intent_inclusion_in_any_tx(
                        &intent_ids,
                        Duration::from_millis(200 * N),
                    )
                    .await
                    .unwrap();
                assert_eq!(intents_txhash.len() as u64, N / 2);

                // Should eventually include the transactions in multiple blocks (non-determinstic)
                let mut intents_block_hash = vec![];
                for id in &intent_ids {
                    let (_, block_hash) = test_harness
                        .wait_for_intent_inclusion_in_any_block(
                            &id,
                            None,
                            Duration::from_secs(2 * N),
                        )
                        .await
                        .unwrap();
                    println!("intent {} included in {}", id, block_hash);
                    intents_block_hash.push(block_hash);
                }

                assert!(
                    intents_txhash.len() < intents_block_hash.len(),
                    "intents tx count {} < blocks count {}",
                    intents_txhash.len(),
                    intents_block_hash.len()
                );

                Ok(())
            }
        })
        .await
        .unwrap();
}
