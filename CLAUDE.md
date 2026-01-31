# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Blob Share is a trusted blob sharing protocol for Ethereum EIP-4844. It implements a bundler service that accepts authenticated data submissions (via ECDSA signatures), packs them into blob transactions, and broadcasts them on-chain. Publishers pre-pay credits via on-chain transfers.

## Workspace Structure

Cargo workspace with 3 crates:
- **`bundler`** — Core service: HTTP API (actix-web), blob transaction creation, chain sync, MySQL persistence (sqlx)
- **`bundler_client`** — Client library for the bundler API
- **`bundler_client_cli`** — CLI tool for submitting data and managing accounts

## Build & Development Commands

```bash
# Build
cargo build
SQLX_OFFLINE=true cargo build    # Build without live DB (uses cached query metadata)

# Format & lint (CI runs these with strict settings)
cargo fmt -- --check
cargo clippy -- --deny warnings

# Tests (must be single-threaded due to integration test dependencies)
cargo test -- --test-threads=1 --nocapture

# Unit tests only (no DB/Docker required)
cargo test --lib

# Database setup (requires Docker; starts MySQL, runs migrations)
cargo install --version='~0.7' sqlx-cli --no-default-features --features rustls,mysql
./scripts/init_db.sh

# After modifying SQL queries, re-sync offline query data
cargo sqlx prepare --workspace

# Docker build
docker build -t blobshare:dev .
```

## Architecture

### Runtime Model

The application starts an actix-web HTTP server alongside several background tokio tasks that run concurrently via `tokio::try_join!` in `App::run` (`bundler/src/lib.rs`):

1. **HTTP server** — API endpoints under `/v1/` (health, sender, sync, gas, data, status, balance)
2. **`blob_sender_task`** — Packs pending data intents into EIP-4844 blob transactions and broadcasts them
3. **`block_subscriber_task`** — Monitors new Ethereum blocks, updates canonical chain state
4. **`remote_node_tracker_task`** — Tracks Ethereum node sync status
5. **`push_metrics_task`** — Optional Prometheus push gateway
6. **`run_consistency_checks_task`** — Initial sync verification on startup

### Core Data Flow

1. Client submits data intent with ECDSA signature → `POST /v1/data` (`routes/post_data.rs`)
2. Intent validated, stored in MySQL, tracked in-memory by `DataIntentTracker`
3. `blob_sender_task` packs intents using bin-packing (`packing.rs`: brute-force for ≤8 items, greedy otherwise)
4. Blob transaction created (`blob_tx_data.rs`, `kzg.rs`) and broadcast
5. `block_subscriber_task` monitors inclusion, handles reorgs

### State Management

- **`BlockSync`** (`sync.rs`) — Canonical chain view, pending/repriced transactions, balance tracking, reorg handling (configurable `finalize_depth`, default 64 blocks)
- **`DataIntentTracker`** (`data_intent_tracker.rs`) — In-memory pending intent index
- Shared via `Arc<AppData>` with `RwLock` for concurrent access

### Database

MySQL via sqlx with offline mode (`SQLX_OFFLINE=true`). Migrations in `bundler/migrations/`. Tables: data intents, users (with nonce for replay protection), anchor blocks, intent inclusions.

### Key Constants (bundler/src/lib.rs)

- `MAX_USABLE_BLOB_DATA_LEN = 31 * FIELD_ELEMENTS_PER_BLOB` (~126KB per blob)
- `MAX_PENDING_DATA_LEN_PER_USER = MAX_USABLE_BLOB_DATA_LEN * 16` (~2MB)

### Testing

Integration tests in `bundler/tests/api/` spawn real Geth and Lodestar instances via Docker. All tests must run single-threaded (`--test-threads=1`). The CI also verifies sqlx offline data is synced with `cargo sqlx prepare --check --workspace`.

### Custom EIP-4844 Types

`bundler/src/reth_fork/` contains forked reth types for EIP-4844 blob transaction encoding (`tx_eip4844.rs`, `tx_sidecar.rs`).

## Development Workflow

Before committing any change, always run this sequence:

1. `cargo fmt`
2. `SQLX_OFFLINE=true cargo clippy -- --deny warnings` — fix all warnings
3. `SQLX_OFFLINE=true cargo test --lib` — unit tests, no DB required
4. If SQL queries were added or modified, run `cargo sqlx prepare --workspace` to re-sync offline query metadata (requires a running DB), or verify the build passes with `SQLX_OFFLINE=true cargo check`
5. If adding a new database migration, place it in `bundler/migrations/` with the naming convention `YYYYMMDD_description.sql`
