# Blob sharing protocol

Implementation of trusted blob sharing protocol. Supports submissions via a permissionless API authenticated via ECDSA signatures. Publishers pre-pay credits via on-chain transfers. For complete rationale and design decisions, refer to https://hackmd.io/@dapplion/blob_sharing

<img src="https://hackmd.io/_uploads/ByUbygIVT.png" alt="drawing" width="450"/>

<!-- HELP_START -->
```
Usage: blobshare [OPTIONS] --database-url <DATABASE_URL>

Options:
  -p, --port <PORT>
          Name of the person to greet [env: PORT=] [default: 5000]
  -b, --bind-address <BIND_ADDRESS>
          Number of times to greet [env: BIND_ADDRESS=] [default: 127.0.0.1]
      --eth-provider <ETH_PROVIDER>
          JSON RPC endpoint for an ethereum execution node [env: ETH_PROVIDER=] [default: ws://127.0.0.1:8546]
      --eth-provider-interval <ETH_PROVIDER_INTERVAL>
          JSON RPC polling interval in miliseconds, used for testing [env: ETH_PROVIDER_INTERVAL=]
      --starting-block <STARTING_BLOCK>
          First block for service to start accounting [env: STARTING_BLOCK=] [default: 0]
      --data-dir <DATA_DIR>
          Directory to persist anchor block finalized data [env: DATA_DIR=] [default: ./data]
      --mnemonic <MNEMONIC>
          Mnemonic for tx sender. If not set a random account will be generated. TODO: UNSAFE, handle hot keys better [env: MNEMONIC=]
      --panic-on-background-task-errors
          FOR TESTING ONLY: panic if a background task experiences an error for a single event [env: PANIC_ON_BACKGROUND_TASK_ERRORS=]
      --finalize-depth <FINALIZE_DEPTH>
          Consider blocks `finalize_depth` behind current head final. If there's a re-org deeper than this depth, the app will crash and expect to re-sync on restart [env: FINALIZE_DEPTH=] [default: 64]
      --max-pending-transactions <MAX_PENDING_TRANSACTIONS>
          Max count of pending transactions that will be sent before waiting for inclusion of the previously sent transactions. A number higher than the max count of blobs per block should not result better UX. However, a higher number risks creating transactions that can become underpriced in volatile network conditions [env: MAX_PENDING_TRANSACTIONS=] [default: 6]
      --database-url <DATABASE_URL>
          Database URL to mysql DB with format `mysql://user:password@localhost/test` [env: DATABASE_URL=]
      --metrics
          Enable serving metrics [env: METRICS=]
      --metrics-port <METRICS_PORT>
          Metrics server port. If it's the same as the main server it will be served there [env: METRICS_PORT=] [default: 9000]
      --metrics-bearer-token <METRICS_BEARER_TOKEN>
          Require callers to the /metrics endpoint to add Bearer token auth [env: METRICS_BEARER_TOKEN=]
      --metrics-push-url <METRICS_PUSH_URL>
          Enable prometheus push gateway to the specified URL [env: METRICS_PUSH_URL=]
      --metrics-push-interval-sec <METRICS_PUSH_INTERVAL_SEC>
          Customize push gateway frequency [env: METRICS_PUSH_INTERVAL_SEC=] [default: 15]
      --metrics-push-basic-auth <METRICS_PUSH_BASIC_AUTH>
          Provide Basic Auth for push gateway requests [env: METRICS_PUSH_BASIC_AUTH=]
      --metrics-push-format <METRICS_PUSH_FORMAT>
          Format to send push gateway metrics [env: METRICS_PUSH_FORMAT=] [default: protobuf] [possible values: protobuf, plain-text]
  -h, --help
          Print help
  -V, --version
          Print version

```
<!-- HELP_END -->

## Usage

WIP / unpublished

## Development

See [CLAUDE.md](./CLAUDE.md) for project structure and build commands.

### Autonomous development with Ralph Loop

The project includes a [Ralph Loop](https://ghuntley.com/loop/) setup for autonomous AI-driven development. Tasks are tracked in [PLAN.md](./PLAN.md) using `### [ ]` / `### [x]` checkbox markers.

```bash
# Run in a VM for sandboxing (recommended)
./ralph.sh
```

Run inside a sandboxed VM (e.g. OrbStack, Lima) since the script uses `--dangerously-skip-permissions`.
