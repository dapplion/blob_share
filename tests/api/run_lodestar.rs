use std::{
    process::Command,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    geth_helpers::{
        generate_rand_str, get_jwtsecret_filepath, pipe_stdout_on_thread, run_until_exit,
        unused_port,
    },
    helpers::retry_with_timeout,
};

const STARTUP_TIMEOUT_MILLIS: u64 = 10_000;
const SECONDS_PER_SLOT: usize = 2;

pub struct LodestarInstance {
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

    // Make sure image is available
    run_until_exit("docker", &["pull", &lodestar_docker_tag]).unwrap();
    log::info!("pulled lodestar image {}", lodestar_docker_tag);

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
        .stderr(std::process::Stdio::piped());

    cmd.args([
        "dev",
        "--genesisValidators=1",
        "--startValidators=0..1",
        &format!("--genesisTime={genesis_timestamp}"),
        &format!("--params.SECONDS_PER_SLOT={}", SECONDS_PER_SLOT),
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

    let mut child = cmd.spawn().expect("could not start docker");
    log::info!("lodestar process started pid {}", child.id());

    pipe_stdout_on_thread(child.stdout.take(), "lodestar stdout");
    pipe_stdout_on_thread(child.stderr.take(), "lodestar stderr");

    // Retrieve the IP of the started container
    let rest_url = format!("http://localhost:{port_rest}");
    log::info!("container urls {port_rest}");

    let client = reqwest::ClientBuilder::new()
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();

    let client_version = retry_with_timeout(
        || async {
            Ok(client
                .get(&format!("{}/eth/v1/node/identity", rest_url))
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
    log::info!("connected to lodestar client {client_version:?}");

    LodestarInstance {
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
