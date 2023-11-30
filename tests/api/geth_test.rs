use std::str::FromStr;

use crate::{helpers::TestHarness, spawn_geth};
use ethers::{
    providers::{Http, Middleware, Provider},
    types::{Address, TransactionRequest},
};
use eyre::Result;

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
async fn geth_send_regular_transaction() -> Result<()> {
    let geth = spawn_geth().await;
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
    let tx = client.send_transaction(tx, None).await?.await?;

    println!("{}", serde_json::to_string(&tx)?);

    let nonce2 = client.get_transaction_count(from, None).await?;

    assert!(nonce1 < nonce2);

    let balance_after = client.get_balance(from, None).await?;
    assert!(balance_after < balance_before);

    println!("Balance before {balance_before}");
    println!("Balance after {balance_after}");

    Ok(())
}
