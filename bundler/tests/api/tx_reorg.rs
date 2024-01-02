use blob_share::{utils::get_max_fee_per_blob_gas, MAX_USABLE_BLOB_DATA_LEN};
use log::info;
use reth_primitives::revm_primitives::TARGET_BLOB_GAS_PER_BLOCK;
use std::time::Duration;

use crate::helpers::{
    assert_close_enough, find_excess_blob_gas, Config, DataReq, TestHarness, TestMode, ETH_TO_WEI,
    GENESIS_FUNDS_ADDR, GWEI_TO_WEI,
};

#[tokio::test]
async fn reprice_single_transaction_after_gas_spike() {
    TestHarness::build(
        TestMode::ELMock,
        Some(Config::default().add_initial_topup(*GENESIS_FUNDS_ADDR, ETH_TO_WEI.into())),
    )
    .await
    .spawn_with_fn(|test_harness| {
        async move {
            const INITIAL_GAS: u64 = 30 * GWEI_TO_WEI;
            const INITIAL_BLOB_GAS: u64 = 1 * GWEI_TO_WEI;
            const HIGHER_BLOB_GAS: u64 = (1.25 * GWEI_TO_WEI as f64) as u64;

            // Set current gas price at 1 GWei
            test_harness
                .mine_block_and_wait_for_sync(|b| {
                    b.base_fee_per_gas = Some(INITIAL_GAS.into());
                    b.excess_blob_gas = Some(find_excess_blob_gas(INITIAL_BLOB_GAS as u128).into());
                    // Fill the block's blob space so the calculated blob gas is the same for  the next block
                    b.blob_gas_used = Some(TARGET_BLOB_GAS_PER_BLOCK.into());
                })
                .await;

            // Send intents priced at 1.25 GWei
            let wallet = test_harness.get_signer_genesis_funds();
            let data_intent_id = test_harness
                .post_data_ok(
                    &wallet.signer(),
                    DataReq::new()
                        .with_data_len(MAX_USABLE_BLOB_DATA_LEN)
                        .with_max_blob_gas(HIGHER_BLOB_GAS),
                )
                .await;
            let tx_hash_first = test_harness
                .wait_for_intent_inclusion_in_any_tx(&[data_intent_id], Duration::from_secs(1))
                .await[0];

            // Expect blob tx priced at 1 GWei
            let tx_first = test_harness.mock_el().get_submitted_tx(tx_hash_first);
            // NOTE: math to compute the inverse fake exponential is not exact, so the actual blob gas is 999999890
            assert_close_enough(
                get_max_fee_per_blob_gas(&tx_first).unwrap() as u64,
                1 * GWEI_TO_WEI,
                GWEI_TO_WEI / 10000,
            );

            // Do not include and bump gas to 1.25 GWei
            test_harness
                .mine_block_and_wait_for_sync(|b| {
                    b.base_fee_per_gas = Some(INITIAL_GAS.into());
                    b.excess_blob_gas = Some(find_excess_blob_gas(HIGHER_BLOB_GAS as u128).into());
                    // Fill the block's blob space so the calculated blob gas is the same for  the next block
                    b.blob_gas_used = Some(TARGET_BLOB_GAS_PER_BLOCK.into());
                })
                .await;

            // Expect new transaction with same nonce at 1.25 Gwei
            let tx_hash_repriced = test_harness
                .wait_for_intent_inclusion_in_any_tx_with_filter(
                    &[data_intent_id],
                    Duration::from_secs(1),
                    &Some(vec![tx_hash_first]),
                )
                .await[0];

            info!("repriced tx {}", tx_hash_repriced);

            // Assert transaction has been correctly repriced (same nonce) and includes the original intents
            let tx_repriced = test_harness.mock_el().get_submitted_tx(tx_hash_repriced);
            assert_close_enough(
                get_max_fee_per_blob_gas(&tx_repriced).unwrap() as u64,
                HIGHER_BLOB_GAS,
                GWEI_TO_WEI / 10000,
            );
            assert!(tx_repriced.nonce == tx_first.nonce);
            assert!(tx_repriced.max_priority_fee_per_gas > tx_first.max_priority_fee_per_gas);
            assert!(tx_repriced.max_fee_per_gas > tx_first.max_fee_per_gas);
        }
    })
    .await;
}
