use ethers::{
    middleware::SignerMiddleware,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::Address,
};
use eyre::{bail, Result};
use rand::{distributions::Alphanumeric, Rng};
use serde_json::json;
use std::{
    future::Future,
    process::{Child, Command},
    str::FromStr,
    time::{Duration, Instant},
};
use tokio::time::sleep;

/// How long we will wait for anvil to indicate that it is ready.
const STARTUP_TIMEOUT_MILLIS: u64 = 2000;
const DEV_PRIVKEY: &str = "392a230386a19b84b6b865067d5493b158e987d28104ab16365854a8fd851bb0";
const DEV_PUBKEY: &str = "0xdbD48e742FF3Ecd3Cb2D557956f541b6669b3277";

pub struct GethInstance {
    pid: Child,
    container_name: String,
    http_url: String,
    ws_url: String,
    chain_id: u64,
}

impl LighthouseInstance {
    pub fn http_url(&self) -> &str {
        &self.http_url
    }

    pub fn ws_url(&self) -> &str {
        &self.ws_url
    }

    pub fn http_provider(&self) -> Result<SignerMiddleware<Provider<Http>, LocalWallet>> {
        let wallet = LocalWallet::from_bytes(&hex::decode(DEV_PRIVKEY)?)?;
        assert_eq!(wallet.address(), Address::from_str(DEV_PUBKEY)?);
        let provider = Provider::<Http>::try_from(self.http_url())?;

        Ok(SignerMiddleware::new(
            provider,
            wallet.with_chain_id(self.chain_id),
        ))
    }
}

pub async fn spawn_geth() -> LighthouseInstance {
    let lighthouse_image_tag = "sigp/lighthouse:latest-unstable";

    let port_http = unused_port();
    let port_ws = unused_port();

    let container_name = format!("lighthouse-dev-cancun-{}", generate_rand_str(10));

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
        GETH_BUILD_TAG,
    ]);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());

    cmd.args(["--dev"]);
    cmd.args([
        "--http",
        "--http.addr=0.0.0.0",
        &format!("--http.port={port_http}"),
        "--http.api=admin,debug,eth,miner,net,personal,txpool,web3",
        "--ws",
        "--ws.addr=0.0.0.0",
        &format!("--ws.port={port_ws}"),
        "--ws.origins",
        "\"*\"",
        "--ws.api=admin,debug,eth,miner,net,personal,txpool,web3",
        // Logging verbosity: 0=silent, 1=error, 2=warn, 3=info, 4=debug, 5=detail
        "--verbosity=5",
    ]);

    let child = cmd.spawn().expect("could not start docker");

    // Retrieve the IP of the started container
    let http_url = format!("http://localhost:{port_http}");
    let ws_url = format!("ws://localhost:{port_ws}");
    println!("container urls {http_url} {ws_url}");

    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let client_version = retry_with_timeout(
        Duration::from_millis(STARTUP_TIMEOUT_MILLIS),
        Duration::from_millis(50),
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
    )
    .await
    .unwrap();
    println!("connected to geth client {client_version:?}");

    let chain_id = Provider::<Http>::try_from(&http_url)
        .unwrap()
        .get_chainid()
        .await
        .unwrap()
        .as_u64();

    LighthouseInstance {
        pid: child,
        container_name,
        http_url,
        ws_url,
        chain_id,
    }
}

impl Drop for LighthouseInstance {
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

fn run_until_exit(program: &str, args: &[&str]) -> Result<String> {
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

fn generate_rand_str(len: usize) -> String {
    let rng = rand::thread_rng();
    rng.sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

async fn retry_with_timeout<T, Fut, F: FnMut() -> Fut>(
    timeout: Duration,
    retry_interval: Duration,
    mut f: F,
) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    let start = Instant::now();
    loop {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if Instant::now().duration_since(start) < timeout {
                    sleep(retry_interval).await;
                } else {
                    return Err(e);
                }
            }
        }
    }
}
