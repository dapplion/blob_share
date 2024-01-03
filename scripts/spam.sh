#!/bin/bash
set -eo pipefail

source .env

cargo run --bin bundler_client_cli -- --url=https://blobshare.up.railway.app --priv-key=$PRIVKEY --eth-provider=$ETH_PROVIDER --skip-wait --test-data-random='rand_same_alphanumeric,10000' --test-repeat-send 32

