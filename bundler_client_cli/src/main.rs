use std::fs;
use std::io::Read;
use std::ops::Range;
use std::time::Duration;

use bundler_client::{types::DataIntentStatus, Client, GasPreference, NoncePreference};
use clap::Parser;
use dotenv::dotenv;
use ethers::middleware::SignerMiddleware;
use ethers::providers::{Http, Middleware, Provider};
use ethers::signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use ethers::types::TransactionRequest;
use eyre::{bail, eyre, Context, Result};
use rand::{thread_rng, Rng};
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
    pub data: Option<String>,

    /// Lower bound balance to trigger a topup: 1e17
    #[arg(env, long, default_value_t = 100000000000000000)]
    pub balance_lower_bound: u128,
    /// If under lower bound, topup to upper bound: 2e17
    #[arg(env, long, default_value_t = 200000000000000000)]
    pub balance_upper_bound: u128,

    /// Factor of extra pricing against next block's blob gas price
    #[arg(env, long, default_value_t = 1.25)]
    pub blob_gas_price_factor: f64,

    /// Do not wait for intent to be included
    #[arg(env, long)]
    pub skip_wait: bool,

    /// FOR TESTING PURPOSES ONLY
    /// Send random data of a specific length. Serialized `RandomData` enum.
    #[arg(env, long)]
    pub test_data_random: Option<String>,

    /// FOR TESTING PURPOSES ONLY
    /// Times to send a data request
    #[arg(env, long)]
    pub test_repeat_send: Option<usize>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenv().ok();
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

    let wallet = if let Some(mnemonic) = &args.mnemonic {
        MnemonicBuilder::<English>::default()
            .phrase(mnemonic.as_str())
            .build()?
    } else if let Some(priv_key) = &args.priv_key {
        LocalWallet::from_bytes(&hex::decode(priv_key).wrap_err_with(|| "invalid priv_key format")?)
            .wrap_err_with(|| "priv_key bytes not valid")?
    } else {
        eyre::bail!("Must set either --mnemonic or --priv-key");
    }
    .with_chain_id(chain_id);
    println!(
        "Using wallet with address 0x{}",
        hex::encode(wallet.address().to_fixed_bytes())
    );

    maybe_fund_sender_account(&args, &client, &provider, &wallet).await?;

    if let Some(send_count) = args.test_repeat_send {
        for i in 0..send_count {
            println!("sending request time {i}/{send_count}");
            send_data_request(&args, &client, &wallet).await?;
        }
    } else {
        send_data_request(&args, &client, &wallet).await?;
    }

    Ok(())
}

async fn maybe_fund_sender_account(
    args: &Args,
    client: &Client,
    provider: &Provider<Http>,
    wallet: &LocalWallet,
) -> Result<()> {
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
            .to(sender.addresses[0])
            .value(args.balance_upper_bound.saturating_sub(balance));

        let pending_tx = client
            .send_transaction(tx, None)
            .await
            .wrap_err("send fund tx")?;
        println!(
            "sent funding transaction to sender address {}, hash: {}",
            sender.addresses[0],
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

    Ok(())
}

async fn send_data_request(args: &Args, client: &Client, wallet: &LocalWallet) -> Result<()> {
    // Read data to publish
    let data = if let Some(test_data_random) = &args.test_data_random {
        let mut rng = thread_rng();
        match RandomData::from_str(test_data_random)? {
            RandomData::Rand(data_len) => {
                let data_len = rng.gen_range(data_len);
                (0..data_len).map(|_| rng.gen()).collect::<Vec<u8>>()
            }
            RandomData::RandSameAlphanumeric(data_len) => {
                let data_len = rng.gen_range(data_len);
                let random_char = rng.sample(rand::distributions::Alphanumeric) as u8;
                vec![random_char; data_len]
            }
        }
    } else if let Some(data_filepath) = &args.data {
        fs::read(data_filepath)?
    } else {
        let mut buffer = Vec::new();
        std::io::stdin().read_to_end(&mut buffer)?;
        buffer
    };

    let gas = GasPreference::RelativeToHead(args.blob_gas_price_factor);
    let nonce = NoncePreference::Timebased;

    let response = client
        .post_data_with_wallet(wallet, data, &gas, &nonce)
        .await
        .wrap_err_with(|| "post_data_with_wallet")?;
    let id = response.id;
    println!("{:?}", id);

    if !args.skip_wait {
        loop {
            let status = client
                .get_status_by_id(id)
                .await
                .wrap_err_with(|| format!("get_status_by_id {id}"))?;
            println!("status {:?}", status);

            if let DataIntentStatus::InConfirmedTx { .. } = status {
                break;
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    Ok(())
}

#[derive(Clone)]
enum RandomData {
    Rand(Range<usize>),
    RandSameAlphanumeric(Range<usize>),
}

impl RandomData {
    fn from_str(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split(',').collect();
        let first = get_part(&parts, 0)?;

        Ok(match first {
            "rand" => RandomData::Rand(parse_range(get_part(&parts, 1)?)?),
            "rand_same_alphanumeric" => {
                RandomData::RandSameAlphanumeric(parse_range(get_part(&parts, 1)?)?)
            }
            _ => bail!(format!("unknown RandomData variant {first}")),
        })
    }
}

fn get_part<'a>(parts: &'a [&'a str], i: usize) -> Result<&'a str> {
    parts
        .get(i)
        .ok_or_else(|| eyre!("no arg in position {i}"))
        .copied()
}

fn parse_range(range_str: &str) -> Result<std::ops::Range<usize>> {
    let bounds: Vec<&str> = range_str.split("..").collect();
    match *bounds.as_slice() {
        [start] => {
            let start = start.parse::<usize>()?;
            Ok(start..start + 1)
        }
        [start, end] => {
            let start = start.parse::<usize>()?;
            let end = end.parse::<usize>()?;
            if start >= end {
                bail!("start >= end");
            }
            Ok(start..end)
        }
        _ => bail!("Invalid range format"),
    }
}
