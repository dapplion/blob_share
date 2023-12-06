use std::{
    process::{Child, Command},
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::geth_helpers::{
    generate_rand_str, get_jwtsecret_filepath, retry_with_timeout, run_until_exit, unused_port,
};

const STARTUP_TIMEOUT_MILLIS: u64 = 10000;

pub struct LodestarInstance {
    pid: Child,
    container_name: String,
    rest_url: String,
}

impl LodestarInstance {
    pub fn http_url(&self) -> &str {
        &self.rest_url
    }
}

pub struct RunLodestarArgs {
    pub genesis_eth1_hash: String,
    pub execution_url: String,
}

pub async fn spawn_lodestar(runner_args: RunLodestarArgs) -> LodestarInstance {
    let lodestar_docker_tag = "chainsafe/lodestar";

    let port_rest = unused_port();

    let container_name = format!("lodestar-dev-cancun-{}", generate_rand_str(10));
    let jwtsecret_path_host = get_jwtsecret_filepath();
    let jwtsecret_path_container = "/jwtsecret";
    // Override genesis timestamp to save 5 seconds of iddle time
    let genesis_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 1;

    let mut cmd = Command::new("docker");
    // Don't run as host, fetch IP latter
    cmd.args([
        "run",
        // Auto-clean container on exit
        "--rm",
        // Name the contaienr to find it latter
        &format!("--name={container_name}"),
        &format!("-p={port_rest}:9596"),
        &format!("-v={jwtsecret_path_host}:/{jwtsecret_path_container}"),
        lodestar_docker_tag,
    ]);

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());

    cmd.args([
        "dev",
        "--genesisValidators=1",
        "--startValidators=0..1",
        &format!("--genesisTime={genesis_timestamp}"),
        "--params.SECONDS_PER_SLOT=1",
        "--params.ALTAIR_FORK_EPOCH=0",
        "--params.BELLATRIX_FORK_EPOCH=0",
        "--params.TERMINAL_TOTAL_DIFFICULTY=0",
        "--params.CAPELLA_FORK_EPOCH=0",
        "--params.DENEB_FORK_EPOCH=0",
        "--reset",
        "--rest",
        "--rest.namespace=\"*\"",
        "--rest.address=0.0.0.0",
        "--rest.port=9596", // keep default for validator client
        &format!("--jwtSecret={jwtsecret_path_container}"),
        &format!("--execution.urls={}", runner_args.execution_url),
        &format!("--genesisEth1Hash={}", runner_args.genesis_eth1_hash),
    ]);

    let child = cmd.spawn().expect("could not start docker");

    // Retrieve the IP of the started container
    let rest_url = format!("http://localhost:{port_rest}");
    println!("container urls {port_rest}");

    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let client_version = retry_with_timeout(
        Duration::from_millis(STARTUP_TIMEOUT_MILLIS),
        Duration::from_millis(50),
        || async {
            Ok(client
                .get(&format!("{}/eth/v1/node/identity", rest_url))
                .send()
                .await?
                .text()
                .await?)
        },
    )
    .await
    .unwrap();
    println!("connected to lodestar client {client_version:?}");

    LodestarInstance {
        pid: child,
        container_name,
        rest_url,
    }
}

impl Drop for LodestarInstance {
    fn drop(&mut self) {
        if let Err(e) = run_until_exit("docker", &["rm", "--force", &self.container_name]) {
            eprintln!("error removing lodestar instance: {e:?}");
        }
    }
}