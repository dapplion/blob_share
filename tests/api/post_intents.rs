use std::time::Duration;

use crate::{
    geth_helpers::GENESIS_FUNDS_ADDR,
    helpers::{retry_with_timeout, unique, Config, TestHarness, TestMode, FINALIZE_DEPTH},
};
use blob_share::{
    client::{NoncePreference, PostDataIntentV1, PostDataIntentV1Signed},
    MAX_PENDING_DATA_LEN_PER_USER, MAX_USABLE_BLOB_DATA_LEN,
};
use ethers::signers::{LocalWallet, Signer};
use log::info;

#[tokio::test]
async fn health_check_works() {
    let testing_harness = TestHarness::spawn_with_el_only().await;
    testing_harness.client.health().await.unwrap();
}

#[tokio::test]
async fn reject_post_data_before_any_topup() {
    TestHarness::build(TestMode::ELOnly, None)
        .await
        .spawn_with_fn(|test_harness| async move {
            let wallet = test_harness.get_signer_genesis_funds();
            // Send data request before funding the sender address
            let res = test_harness.post_data_of_len(&wallet.signer(), 69).await;

            assert_eq!(
                res.unwrap_err().to_string(),
                "non-success response status 500 body: Insufficient balance"
            );
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn reject_post_data_after_insufficient_balance() {
    TestHarness::build(
        TestMode::ELOnly,
        Some(Config::default().add_initial_topup(*GENESIS_FUNDS_ADDR, 1000)),
    )
    .await
    .spawn_with_fn(|test_harness| async move {
        let wallet = test_harness.get_signer_genesis_funds();

        // First request should be ok
        test_harness
            .post_data_of_len(wallet.signer(), 1000)
            .await
            .unwrap();
        // Second request must be rejected
        let res = test_harness.post_data_of_len(wallet.signer(), 1000).await;

        assert_eq!(
            res.unwrap_err().to_string(),
            "non-success response status 500 body: Insufficient balance"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reject_single_data_intent_too_big() {
    TestHarness::build(
        TestMode::ELOnly,
        Some(Config::default().add_initial_topup(*GENESIS_FUNDS_ADDR, 100000000)),
    )
    .await
    .spawn_with_fn(|test_harness| async move {
        let wallet = test_harness.get_signer_genesis_funds();
        // Send data request before funding the sender address
        let res = test_harness
            .post_data_of_len(&wallet.signer(), MAX_USABLE_BLOB_DATA_LEN + 1)
            .await;

        assert_eq!(
            res.unwrap_err().to_string(),
            "non-success response status 400 body: data length 126977 over max usable blob data 126976"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn reject_post_data_request_invalid_signature_mutate_nonce() {
    reject_post_data_request_invalid_signature(|intent| {
        intent.nonce += 1;
    })
    .await;
}

#[tokio::test]
async fn reject_post_data_request_invalid_signature_mutate_max_blob_gas_price() {
    reject_post_data_request_invalid_signature(|intent| {
        intent.intent.max_blob_gas_price += 1;
    })
    .await;
}

#[tokio::test]
async fn reject_post_data_request_invalid_signature_mutate_data() {
    reject_post_data_request_invalid_signature(|intent| {
        intent.intent.data[0] += 1;
    })
    .await;
}

async fn reject_post_data_request_invalid_signature<F>(mutate: F)
where
    F: FnOnce(&mut PostDataIntentV1Signed),
{
    TestHarness::build(
        TestMode::ELOnly,
        Some(Config::default().add_initial_topup(*GENESIS_FUNDS_ADDR, 100000000000)),
    )
    .await
    .spawn_with_fn(|test_harness| async move {
        let wallet = test_harness.get_signer_genesis_funds();
        let nonce = 1;
        let mut intent_signed = PostDataIntentV1Signed::with_signature(
            wallet.signer(),
            PostDataIntentV1 {
                from: wallet.address(),
                data: vec![0xaa; 1000],
                max_blob_gas_price: 1,
            },
            Some(nonce),
        )
        .await
        .unwrap();

        // Mutate after signing
        mutate(&mut intent_signed);

        let res = test_harness.client.post_data(&intent_signed).await;

        let err_str = res.unwrap_err().to_string();
        assert!(
            err_str.contains("Signature verification failed"),
            "Expected error 'Signature verification failed' but got '{}'",
            err_str
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn post_two_intents_and_expect_blob_tx() {
    TestHarness::build(TestMode::WithChain, None)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness.wait_for_app_health().await;

                // Fund account
                let wallet = test_harness.get_signer_genesis_funds();
                test_harness.fund_sender_account(&wallet, None).await;

                test_post_two_data_intents_up_to_inclusion(&test_harness, wallet.signer(), 0).await;
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn post_many_intents_series_and_expect_blob_tx() {
    TestHarness::build(TestMode::WithChain, None)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness.wait_for_app_health().await;

                // Fund account
                let wallet = test_harness.get_signer_genesis_funds();
                test_harness.fund_sender_account(&wallet, None).await;

                // +4 for the time it takes the fund transaction to go through
                let n = 4 + 2 * FINALIZE_DEPTH;
                for i in 0..4 + 2 * FINALIZE_DEPTH {
                    test_post_two_data_intents_up_to_inclusion(
                        &test_harness,
                        wallet.signer(),
                        i as u8,
                    )
                    .await;
                    info!("test-progress: completed step {i}/{n}");
                }
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

    // Ensure node is synced up to 1 block of difference
    let sync = test_harness.client.get_sync().await.unwrap();
    assert!(
        sync.node_head.number - sync.synced_head.number < 1,
        "node not synced, head: {:?} node: {:?}",
        sync.node_head,
        sync.synced_head
    );

    let balance_before_intent_1 = test_harness
        .client
        .get_balance_by_address(wallet.address())
        .await
        .unwrap();

    let intent_1_id = test_harness
        .post_data_and_wait_for_pending(wallet, data_1.clone())
        .await;

    // Check data intent is stored
    assert_eq!(test_harness.client.get_data().await.unwrap().len(), 1);
    // Check by_id API
    let intent_1 = test_harness
        .client
        .get_data_by_id(&intent_1_id)
        .await
        .unwrap();
    assert_eq!(intent_1.data, data_1);

    // Check balance has decreased
    let balance_after_intent_1 = test_harness
        .client
        .get_balance_by_address(wallet.address())
        .await
        .unwrap();
    assert!(
        balance_after_intent_1 < balance_before_intent_1,
        "balance should decrease {balance_after_intent_1} < {balance_before_intent_1}"
    );

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
    info!("test-progress: intents included in txs {intents_txhash:?}",);

    let (_intent_1_txhash_block, intent_1_block) = test_harness
        .wait_for_intent_inclusion_in_any_block(
            &intent_1_id,
            Some(intents_txhash[0]),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    info!("test-progress: intents included in block {intent_1_block}");

    // Check balance has decreased again for intent2 and after inclusion
    let balance_after_inclusion_2 = test_harness
        .client
        .get_balance_by_address(wallet.address())
        .await
        .unwrap();
    assert!(
        balance_after_inclusion_2 < balance_after_intent_1,
        "balance should decrease {balance_after_inclusion_2} < {balance_after_intent_1}"
    );

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
    TestHarness::build(TestMode::WithChain, None)
        .await
        .spawn_with_fn(|test_harness| {
            async move {
                // TODO: Should run this as part of test harness setup
                test_harness.wait_for_app_health().await;

                // Fund account
                let wallet = test_harness.get_signer_genesis_funds();
                test_harness.fund_sender_account(&wallet, None).await;

                // Num of intents to send at once
                const N: u64 = 32;

                let datas = (0..N as u8)
                    .map(|i| vec![0xa0_u8 + i; MAX_USABLE_BLOB_DATA_LEN / 2])
                    .collect::<Vec<_>>();

                // Post all datas in quick succession before asserting for inclusion. Post in
                // sequence so the server can check the nonce is sequential.
                let mut intent_ids = vec![];
                for (i, data) in datas.iter().enumerate() {
                    info!("sending data intent with nonce {}", i);
                    intent_ids.push(
                        test_harness
                            .post_data(
                                &wallet.signer(),
                                data.to_vec(),
                                Some(NoncePreference::Value(i as u64)),
                            )
                            .await,
                    )
                }

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
                let unique_intents_txhash = unique(&intents_txhash);
                if unique_intents_txhash.len() as u64 != N / 2 {
                    panic!(
                        "unique_intents_txhash should equal N/2: {:?}",
                        unique_intents_txhash
                    );
                }

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

                // Some transactions should be included in the same block. So the count of unique
                // block hashes must be less than the unique count of tx hashes
                let unique_intents_block_hash = unique(&intents_block_hash);
                assert!(
                    unique_intents_block_hash.len() < unique_intents_txhash.len(),
                    "blocks count {} < intents tx count {}",
                    unique_intents_block_hash.len(),
                    unique_intents_txhash.len(),
                );
            }
        })
        .await
        .unwrap();
}
