# Blob sharing protocol

Implementation of trusted blob sharing protocol. Supports submissions via a permissionless API authenticated via ECDSA signatures. Publishers pre-pay credits via on-chain transfers. For complete rationale and design decisions, refer to https://hackmd.io/@dapplion/blob_sharing

<img src="https://hackmd.io/_uploads/ByUbygIVT.png" alt="drawing" width="450"/>

<!-- HELP_START -->
```
Usage: blobshare [OPTIONS]

Options:
  -p, --port <PORT>
          Name of the person to greet [default: 5000]
  -b, --bind-address <BIND_ADDRESS>
          Number of times to greet [default: 127.0.0.1]
      --eth-provider <ETH_PROVIDER>
          JSON RPC endpoint for an ethereum execution node [default: ws://127.0.0.1:8546]
      --eth-provider-interval <ETH_PROVIDER_INTERVAL>
          JSON RPC polling interval in miliseconds, used for testing
      --starting-block <STARTING_BLOCK>
          First block for service to start accounting [default: 0]
      --mnemonic <MNEMONIC>
          Mnemonic for tx sender TODO: UNSAFE, handle hot keys better [default: "any any any any any any any any any any any any any any"]
      --panic-on-background-task-errors
          FOR TESTING ONLY: panic if a background task experiences an error for a single event
  -h, --help
          Print help
  -V, --version
          Print version

```
<!-- HELP_END -->

## Usage

WIP / unpublished
