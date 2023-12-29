#!/usr/bin/env bash
set -x
set -eo pipefail

# increase limit for macos
ulimit -n 4096

cargo test -- --test-threads=1 --nocapture "$@"
