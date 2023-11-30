use ethers::providers::{Http, Middleware, Provider};
use eyre::{bail, Result};
use rand::{distributions::Alphanumeric, Rng};
use std::{
    future::Future,
    net::IpAddr,
    process::{Child, Command},
    time::{Duration, Instant},
};
use tokio::time::sleep;

/// How long we will wait for anvil to indicate that it is ready.
const STARTUP_TIMEOUT_MILLIS: u64 = 2000;
const GETH_BUILD_TAG: &str = "geth-dev-cancun:local";

pub struct GethInstance {
    pid: Child,
    http_url: String,
    ws_url: String,
}

pub async fn spawn_geth() -> GethInstance {
    let geth_version = "v1.13.5";

    // Make sure image is available
    run_until_exit(
        "docker",
        &[
            "build",
            &format!("--build-arg='tag={geth_version}'"),
            &format!("--tag={GETH_BUILD_TAG}"),
            "./tests/api/geth",
        ],
    )
    .unwrap();

    let container_name = format!("geth-dev-cancun-{}", generate_rand_str(10));

    let mut cmd = Command::new("docker");
    // Don't run as host, fetch IP latter
    cmd.args([
        "run",
        // Auto-clean container on exit
        "--rm",
        // Name the contaienr to find it latter
        "--name",
        &container_name,
        GETH_BUILD_TAG,
    ]);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());

    let port_http = unused_port();
    let port_ws = unused_port();

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
    ]);

    let child = cmd.spawn().expect("could not start docker");

    // Retrieve the IP of the started container
    let container_ip = retry_with_timeout(
        Duration::from_millis(STARTUP_TIMEOUT_MILLIS),
        Duration::from_millis(50),
        || async {
            run_until_exit(
                "docker",
                &[
                    "inspect",
                    "-f='{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}'",
                    &container_name,
                ],
            )
        },
    )
    .await
    .unwrap();
    let container_ip = extract_valid_ip(&container_ip).unwrap();

    let http_url = format!("http://{container_ip}:{port_http}");
    let ws_url = format!("ws://{container_ip}:{port_ws}");
    println!("got container IPs {http_url}");

    retry_with_timeout(
        Duration::from_millis(STARTUP_TIMEOUT_MILLIS),
        Duration::from_millis(50),
        || async {
            let provider = Provider::<Http>::try_from(&http_url)?;
            Ok(provider.client_version().await?)
        },
    )
    .await
    .unwrap();

    GethInstance {
        pid: child,
        http_url,
        ws_url,
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

fn extract_valid_ip(input: &str) -> Result<IpAddr> {
    let potential_ip = input.trim_matches(|c: char| !c.is_numeric() && c != '.');
    Ok(potential_ip.parse()?)
}
