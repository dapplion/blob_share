# AGENTS.md

This file provides guidance to OpenAI Codex when working with this repository.
For Claude Code guidance, see [CLAUDE.md](./CLAUDE.md).

## Project context

Rust workspace implementing a trusted blob sharing protocol for Ethereum EIP-4844. Three crates: `bundler` (core service), `bundler_client` (client library), `bundler_client_cli` (CLI tool). Uses actix-web, sqlx (MySQL), ethers-rs.

## Review guidelines

When reviewing changes, pay special attention to:

- **In-memory vs DB consistency**: Data is cached in-memory (`DataIntentTracker`) and persisted to MySQL. Verify that values stored to DB match what the in-memory structs expect on reload. Watch for fields that serve dual purposes (e.g. `data_len` for both packing capacity and cost calculation).
- **Panic paths**: Flag any `.unwrap()`, `.expect()`, `panic!()`, or `todo!()` in non-test code. These must use `?`, `.ok_or_else()`, or `.map_err()` instead.
- **Cost calculation correctness**: Cost uses `chargeable_data_len` (floored to 31 bytes / one field element), not raw `data_len`. Verify both `DataIntent::max_cost()` and `DataIntentSummary::max_cost()` are consistent.
- **Blob packing correctness**: Packing and capacity checks must use actual byte length (`data_len`), not chargeable length. Off-by-one errors in bin-packing are critical.
- **Reorg safety**: `BlockSync` handles chain reorgs up to `finalize_depth`. Changes to sync logic must preserve the invariant that finalized data is never rolled back.
- **Nonce management**: Sender nonce tracking must be strictly sequential. Verify no gaps or duplicates in nonce assignment across pending/repriced transactions.
- **SQL injection / query safety**: All user-supplied values must go through sqlx bind parameters, never string interpolation.
- **Integer overflow**: Cost calculations multiply `u64 * u64` — verify these use `u128` intermediates. `data_len` conversions between `usize` and `u32` must be checked.
- **Error propagation**: Background tasks (`blob_sender_task`, `block_subscriber_task`) must not silently swallow errors. Verify errors are logged and/or propagated.
- **Treat P2 issues as P1**: Flag all bugs including normal-priority issues, not just critical/urgent ones.
