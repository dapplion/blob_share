use std::fs;

use blob_share::client::{Client, EthProvider, GasPreference};
use clap::Parser;
use ethers::signers::{coins_bip39::English, MnemonicBuilder, Signer};
use eyre::{eyre, Context, Result};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Blob share API base URL
    #[arg(long)]
    pub url: String,

    /// JSON RPC URL to the network where blobs are posted
    #[arg(long)]
    pub eth_provider: Option<String>,

    /// Mnemonic to derive sender key from
    #[arg(long)]
    pub mnemonic: String,

    /// Path of data to post
    #[arg(long)]
    pub data: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client::new(&args.url)?;

    client
        .health()
        .await
        .wrap_err_with(|| eyre!("unable to connect to API {}", args.url))?;

    let provider = EthProvider::new(&args.eth_provider.unwrap()).await?;
    let chain_id = provider.get_chainid().await?.as_u64();

    let wallet = MnemonicBuilder::<English>::default()
        .phrase(args.mnemonic.as_str())
        .index(0u32)?
        .build()?
        .with_chain_id(chain_id);

    let data = fs::read(args.data)?;

    let gas = GasPreference::FetchFromProvider(provider);

    let response = client.post_data_with_wallet(&wallet, data, &gas).await?;
    println!("{:?}", response);

    Ok(())
}
