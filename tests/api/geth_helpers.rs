use ethers::{
    middleware::SignerMiddleware,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::{Address, H256},
};
use eyre::{bail, Result};
use lazy_static::lazy_static;
use rand::{distributions::Alphanumeric, Rng};
use serde_json::json;
use std::{
    env,
    io::{BufRead, BufReader, Read},
    process::Command,
    str::FromStr,
    thread,
    time::Duration,
};

use crate::helpers::retry_with_timeout;

/// How long we will wait for anvil to indicate that it is ready.
const STARTUP_TIMEOUT_MILLIS: u64 = 10_000;
const GETH_BUILD_TAG: &str = "geth-dev-cancun:local";
const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";
const DEV_PUBKEY: &str = "0xdbD48e742FF3Ecd3Cb2D557956f541b6669b3277";

lazy_static! {
    pub static ref GENESIS_FUNDS_ADDR: Address = Address::from_str(DEV_PUBKEY).unwrap();
}

pub fn get_jwtsecret_filepath() -> String {
    path_from_cwd(&["tests", "artifacts", "jwtsecret"])
}

#[cfg(target_os = "macos")]
const IS_MACOS: bool = true;
#[cfg(not(target_os = "macos"))]
const IS_MACOS: bool = false;

pub type WalletWithProvider = SignerMiddleware<Provider<Http>, LocalWallet>;

pub fn get_wallet_genesis_funds() -> LocalWallet {
    LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY).unwrap()).unwrap()
}

pub fn get_signer_genesis_funds(
    eth_provider_url: &str,
    chain_id: u64,
) -> Result<WalletWithProvider> {
    let wallet = get_wallet_genesis_funds();
    assert_eq!(wallet.address(), *GENESIS_FUNDS_ADDR);
    let provider = Provider::<Http>::try_from(eth_provider_url)?;

    Ok(SignerMiddleware::new(
        provider,
        wallet.with_chain_id(chain_id),
    ))
}

pub struct GethInstance {
    container_name: String,
    http_url: String,
    ws_url: String,
    port_authrpc: u16,
    chain_id: u64,
    genesis_block_hash: H256,
}

impl GethInstance {
    pub fn http_url(&self) -> &str {
        &self.http_url
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub fn authrpc_url(&self) -> String {
        if IS_MACOS {
            format!("http://host.docker.internal:{}", self.port_authrpc)
        } else {
            // internal docker default bridge network gateway
            format!("http://172.17.0.1:{}", self.port_authrpc)
        }
    }

    pub fn http_provider(&self) -> Result<WalletWithProvider> {
        get_signer_genesis_funds(self.http_url(), self.chain_id)
    }

    pub fn genesis_block_hash_hex(&self) -> String {
        format!("0x{}", hex::encode(self.genesis_block_hash))
    }
}

pub enum GethMode {
    Interop,
    Dev,
}

pub async fn spawn_geth(mode: GethMode) -> GethInstance {
    let geth_version = "v1.13.5";

    let geth_dockerfile_dirpath = path_from_cwd(&["tests", "artifacts", "geth"]);
    log::info!("spawn geth with Dockerfile {}", geth_dockerfile_dirpath);

    // Make sure image is available
    run_until_exit(
        "docker",
        &[
            "build",
            &format!("--build-arg='tag={geth_version}'"),
            &format!("--tag={GETH_BUILD_TAG}"),
            &geth_dockerfile_dirpath,
        ],
    )
    .unwrap();

    let port_http = unused_port();
    let port_ws = unused_port();
    let port_authrpc = unused_port();

    let container_name = format!("geth-dev-cancun-{}", generate_rand_str(10));
    let jwtsecret_path_host = get_jwtsecret_filepath();
    let jwtsecret_path_container = "/jwtsecret";

    let mut cmd = Command::new("docker");
    // Don't run as host, fetch IP latter
    cmd.args([
        "run",
        // Auto-clean container on exit
        "--rm",
        // Name the contaienr to find it latter
        &format!("--name={container_name}"),
        &format!("-p={port_http}:{port_http}"),
        &format!("-p={port_ws}:{port_ws}"),
        &format!("-p={port_authrpc}:{port_authrpc}"),
        &format!("-v={jwtsecret_path_host}:/{jwtsecret_path_container}"),
        GETH_BUILD_TAG,
    ]);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    match mode {
        GethMode::Dev => cmd.args(["--dev"]),
        GethMode::Interop => cmd.args([
            // Interop flags with CL
            "--authrpc.addr=0.0.0.0",
            &format!("--authrpc.port={}", port_authrpc),
            // WARNING! this * may have to be surrounded by quotes in some platforms
            "--authrpc.vhosts=*",
            &format!("--authrpc.jwtsecret={jwtsecret_path_container}"),
        ]),
    };

    cmd.args([
        "--nodiscover",
        "--http",
        "--http.addr=0.0.0.0",
        &format!("--http.port={port_http}"),
        "--http.api=admin,debug,eth,miner,net,personal,txpool,web3",
        "--ws",
        "--ws.addr=0.0.0.0",
        &format!("--ws.port={port_ws}"),
        "--ws.origins=\"*\"",
        "--ws.api=admin,debug,eth,miner,net,personal,txpool,web3",
        // Logging verbosity: 0=silent, 1=error, 2=warn, 3=info, 4=debug, 5=detail
        "--verbosity=5",
    ]);

    let mut child = cmd.spawn().expect("could not start docker");

    pipe_stdout_on_thread(child.stdout.take(), "geth stdout");
    pipe_stdout_on_thread(child.stderr.take(), "geth stderr");

    // Retrieve the IP of the started container
    let http_url = format!("http://localhost:{port_http}");
    let ws_url = format!("ws://localhost:{port_ws}");
    log::info!("Attempting to connect to Geth JSON RPC {http_url} {ws_url}");

    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let client_version = retry_with_timeout(
        || async {
            let request_body = json!({
                "jsonrpc": "2.0",
                "method": "web3_clientVersion",
                "params": [],
                "id": 1
            });

            Ok(client
                .post(&http_url)
                .json(&request_body)
                .send()
                .await?
                .text()
                .await?)
        },
        Duration::from_millis(STARTUP_TIMEOUT_MILLIS),
        Duration::from_millis(50),
    )
    .await
    .unwrap();
    log::info!("connected to Geth client {client_version:?}");

    let client = Provider::<Http>::try_from(&http_url).unwrap();
    let chain_id = client.get_chainid().await.unwrap().as_u64();
    let genesis_block = client.get_block(0).await.unwrap().unwrap();

    GethInstance {
        container_name,
        http_url,
        ws_url,
        port_authrpc,
        chain_id,
        genesis_block_hash: genesis_block.hash.unwrap(),
    }
}

impl Drop for GethInstance {
    fn drop(&mut self) {
        if let Err(e) = run_until_exit("docker", &["rm", "--force", &self.container_name]) {
            eprintln!("error removing geth instance: {e:?}");
        }
    }
}

/// A bit of hack to find an unused TCP port.
///
/// Does not guarantee that the given port is unused after the function exists, just that it was
/// unused before the function started (i.e., it does not reserve a port).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn unused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("Failed to create TCP listener to find unused port");

    let local_addr = listener
        .local_addr()
        .expect("Failed to read TCP listener local_addr to find unused port");
    local_addr.port()
}

pub fn pipe_stdout_on_thread<R: Read + Send + 'static>(stdout: Option<R>, prefix: &'static str) {
    let reader = BufReader::new(stdout.expect(&format!("stdout is None for {}", prefix)));

    // Spawn a new thread to handle stdout
    thread::spawn(move || {
        for line in reader.lines() {
            match line {
                Ok(line) => println!("[{}]: {}", prefix, line),
                Err(e) => eprintln!("Error reading line of {}: {}", prefix, e),
            }
        }
    });
}

pub fn run_until_exit(program: &str, args: &[&str]) -> Result<String> {
    // Replace "your_command" with the command you want to run
    // and add any arguments as additional strings in the array
    let output = Command::new(program)
        .args(args)
        .output()
        .expect("Failed to execute command");

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        bail!(
            "Command '{program} {args:?}' error: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

pub fn generate_rand_str(len: usize) -> String {
    let rng = rand::thread_rng();
    rng.sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

pub fn path_from_cwd(parts: &[&str]) -> String {
    let mut path = env::current_dir().unwrap();
    for part in parts {
        path.push(part);
    }
    path.to_str().unwrap().to_string()
}
