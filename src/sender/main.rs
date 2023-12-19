use std::fs;
use std::str::FromStr;
use std::time::Duration;

use blob_share::client::{
    Client, DataIntentId, DataIntentStatus, EthProvider, GasPreference, NoncePreference,
};
use clap::Parser;
use ethers::middleware::SignerMiddleware;
use ethers::providers::{Http, Middleware, Provider};
use ethers::signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use ethers::types::TransactionRequest;
use eyre::{eyre, Context, Result};
use tokio::time::sleep;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Blob share API base URL
    #[arg(env, long)]
    pub url: String,

    /// JSON RPC URL to the network where blobs are posted
    #[arg(env, long)]
    pub eth_provider: String,

    /// Mnemonic to derive sender key from
    #[arg(env, long)]
    pub mnemonic: Option<String>,

    /// Private key hex encoded to derive sender key from
    #[arg(env, long)]
    pub priv_key: Option<String>,

    /// Path of data to post
    #[arg(env, long)]
    pub data: String,

    /// Lower bound balance to trigger a topup: 1e17
    #[arg(env, long, default_value_t = 100000000000000000)]
    pub balance_lower_bound: u128,
    /// If under lower bound, topup to upper bound: 2e17
    #[arg(env, long, default_value_t = 200000000000000000)]
    pub balance_upper_bound: u128,

    /// Factor of extra pricing against next block's blob gas price
    #[arg(env, long, default_value_t = 1.25)]
    pub blob_gas_price_factor: f64,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::new(&args.url)?;

    let sync_status = client
        .get_sync()
        .await
        .wrap_err_with(|| eyre!("unable to connect to API {}", args.url))?;
    println!(
        "Connected to client at {}, sync status {:#?}",
        args.url, sync_status,
    );

    let provider: Provider<Http> = Provider::<Http>::try_from(&args.eth_provider)?;
    let chain_id = provider.get_chainid().await?.as_u64();

    let wallet = if let Some(mnemonic) = args.mnemonic {
        MnemonicBuilder::<English>::default()
            .phrase(mnemonic.as_str())
            .build()?
    } else if let Some(priv_key) = args.priv_key {
        LocalWallet::from_bytes(&hex::decode(priv_key).wrap_err_with(|| "invalid priv_key format")?)
            .wrap_err_with(|| "priv_key bytes not valid")?
    } else {
        panic!("Must set either mnemonic or priv_key");
    }
    .with_chain_id(chain_id);

    // Maybe fund account
    let balance = client
        .get_balance_by_address(wallet.address())
        .await
        .wrap_err("get_balance_by_address")?;
    let balance = if balance < 0 { 0 } else { balance as u128 };

    if balance < args.balance_lower_bound {
        println!(
            "balance {} below lower bound {}, sending funding tx",
            balance, args.balance_lower_bound
        );

        let sender = client.get_sender().await.wrap_err("get_sender")?;
        let client = SignerMiddleware::new(provider.clone(), wallet.clone());

        let tx = TransactionRequest::new()
            .to(sender.address)
            .value(args.balance_upper_bound.saturating_sub(balance));

        let pending_tx = client
            .send_transaction(tx, None)
            .await
            .wrap_err("send fund tx")?;
        println!(
            "sent funding transaction to sender address {}, hash: {}",
            sender.address,
            pending_tx.tx_hash()
        );

        if let Some(receipt) = pending_tx.confirmations(1).await? {
            println!(
                "funding transaction included in block {:?} {:?}",
                receipt.block_number, receipt.block_hash
            );
        } else {
            println!("no receipt avail in funding transaction");
        }
    } else {
        println!(
            "balance ok, above lower bound {} > {}",
            balance, args.balance_lower_bound
        );
    }

    // Read data to publish
    let data = fs::read(args.data)?;

    let gas =
        GasPreference::RelativeToHead(EthProvider::Http(provider), args.blob_gas_price_factor);
    let nonce = NoncePreference::Timebased;

    let response = client
        .post_data_with_wallet(&wallet, data, &gas, &nonce)
        .await?;
    let id = DataIntentId::from_str(&response.id)?;
    println!("{:?}", id);

    loop {
        let status = client.get_status_by_id(id).await?;
        println!("status {:?}", status);

        if let DataIntentStatus::InConfirmedTx { .. } = status {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}
