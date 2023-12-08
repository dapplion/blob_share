use std::{str::FromStr, time::Duration};

use crate::{
    geth_helpers::GethMode,
    run_lodestar::{spawn_lodestar, RunLodestarArgs},
    spawn_geth,
};
use ethers::{
    providers::Middleware,
    types::{Address, TransactionRequest},
};
use eyre::Result;
use tokio::time::timeout;

#[tokio::test]
async fn geth_test_spawn_dev() {
    spawn_geth(GethMode::Dev).await;
}

#[tokio::test]
async fn geth_test_spawn_interop() {
    spawn_geth(GethMode::Interop).await;
}

#[tokio::test]
async fn geth_send_regular_transaction_no_inclusion() -> Result<()> {
    let geth = spawn_geth(GethMode::Interop).await;
    let client = geth.http_provider().unwrap();

    let from = client.address();
    let to = Address::from_str("0x0000000000000000000000000000000000000000").unwrap();

    // craft the tx
    let tx = TransactionRequest::new()
        .to(to)
        .value(1)
        .from(from)
        .gas(30000);

    // broadcast it via the eth_sendTransaction API
    let tx = client.send_transaction(tx, None).await?;
    client.get_transaction(tx.tx_hash()).await?;

    Ok(())
}

#[tokio::test]
async fn geth_send_regular_transaction_with_inclusion() -> Result<()> {
    let geth = spawn_geth(GethMode::Interop).await;

    let _lodestar = spawn_lodestar(RunLodestarArgs {
        execution_url: geth.authrpc_url(),
        genesis_eth1_hash: geth.genesis_block_hash_hex(),
    })
    .await;

    let client = geth.http_provider().unwrap();

    let from = client.address();
    let to = Address::from_str("0x0000000000000000000000000000000000000000").unwrap();

    // craft the tx
    let tx = TransactionRequest::new()
        .to(to)
        .value(1)
        .from(from)
        .gas(30000);

    let balance_before = client.get_balance(from, None).await?;
    let nonce1 = client.get_transaction_count(from, None).await?;

    // broadcast it via the eth_sendTransaction API
    let tx = timeout(
        Duration::from_secs(30),
        client.send_transaction(tx, None).await?.confirmations(1),
    )
    .await??;

    println!("{}", serde_json::to_string(&tx)?);

    let nonce2 = client.get_transaction_count(from, None).await?;
    assert!(nonce1 < nonce2);
    let balance_after = client.get_balance(from, None).await?;
    assert!(balance_after < balance_before);

    Ok(())
}
