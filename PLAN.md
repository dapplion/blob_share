# Blob Share: MVP → Production Execution Plan

## Current State

**Core loop works end-to-end:** submit data intent → validate + store → pack into blob TX → broadcast → monitor inclusion → handle reorgs → finalize. Client SDK and CLI are complete. Balance tracking via on-chain topups works.

**What's not production-ready:** ~45 TODOs, 1 `todo!()` panic, 40+ `.unwrap()` calls, no rate limiting, no intent cancellation, no pruning, skeletal UI, beacon consumer not wired up, no multi-sender or multi-blob support.

## Target End State

A reliable, self-operated blob bundling service where:
- The core path cannot panic on malformed input or edge cases
- The API is protected against abuse
- Users can cancel intents and view their history + published blobs via the explorer
- Finalized data is pruned to prevent DB bloat
- Multiple sender wallets distribute nonce contention
- Data larger than one blob can be split across multiple blobs
- The beacon consumer retrieves published blobs for display in the explorer

---

## Phase 1: Hardening (eliminate panics, add safety)

### [x] 1.1 Fix `todo!()` panic for metrics on separate port
- **File:** `bundler/src/lib.rs:307`
- **Change:** Replace `todo!("serve metrics on different port")` with a second `HttpServer` bound to `metrics_port` that only registers the `get_metrics` route. Start it alongside the main server in `App::run`.

### [x] 1.2 Replace `.unwrap()` calls in production code
- **`bundler/src/metrics.rs`** (~30 unwrap calls): These are in `lazy_static!` metric registration. Wrap in a `register_metrics()` -> `Result` function called at startup; propagate errors to `App::build`.
- **`bundler/src/explorer/mod.rs:41,57,75`**: Replace `.unwrap()` on template rendering with `.map_err(e500)?` (handlers already return `impl Responder` or `Result`; adjust `get_home` and `get_address` to return `Result<HttpResponse, actix_web::Error>`).
- **`bundler/src/sync.rs:304,731,931,964`**: Replace with `?` or contextual `.ok_or_else(|| eyre!(...))?`.
- **`bundler/src/kzg.rs:254`**: Replace `.expect("bad signature")` with `?` propagation.
- **`bundler/src/blob_tx_data.rs:276,293`**: Replace with `?`.
- **`bundler/src/lib.rs:296`**: Replace `listener.local_addr().unwrap()` with `?`.
- **`bundler/src/reth_fork/tx_sidecar.rs:293`**: Replace with `?`.
- **`bundler/src/utils/mod.rs:206,215`**: Replace with `?`.
- **`bundler_client_cli/src/main.rs:92`**: Replace `panic!()` with `clap` validation or `eyre::bail!`.

### [x] 1.3 Add database indexes
- **File:** New migration `bundler/migrations/YYYYMMDD_add_indexes.sql`
- Add index on `data_intents.eth_address` (used by balance/intent queries per user)
- Add index on `anchor_block.block_number DESC` (used by `ORDER BY block_number DESC LIMIT 1`)

### [x] 1.4 Make connection pool size configurable
- **File:** `bundler/src/lib.rs`
- Add `--db-max-connections` CLI arg (default 10, up from hardcoded 5)
- Pass to `MySqlPoolOptions::new().max_connections()`

### [x] 1.5 Add request body size limit
- **File:** `bundler/src/lib.rs` (HttpServer setup)
- Configure `actix_web::web::JsonConfig::default().limit(256 * 1024)` (~256KB, enough for one blob + overhead)

### [x] 1.6 Add basic rate limiting
- **File:** `bundler/src/lib.rs`, new file `bundler/src/rate_limit.rs`
- Add `actix-governor` dependency or a simple in-memory token bucket middleware
- Rate limit by IP on all endpoints; tighter limit on `POST /v1/data`
- CLI args: `--rate-limit-per-second` (default 10), `--rate-limit-burst` (default 20)

### [x] 1.7 Improve health check
- **File:** `bundler/src/routes/mod.rs:19-22`
- `GET /v1/health` should verify: DB pool can acquire a connection, provider can reach the node (cached check, not per-request). Return 503 if unhealthy.

### [x] 1.8 Make remote node tracker polling interval configurable
- **File:** `bundler/src/remote_node_tracker_task.rs:12` (hardcoded 12s), `bundler/src/lib.rs` (Args)
- Add `--node-poll-interval-sec` CLI arg (default 12)

---

## Phase 2: Operational Features

### [x] 2.1 Cancel data intent endpoint
- **Files:** `bundler/src/routes/mod.rs`, new `bundler/src/routes/delete_data.rs`, `bundler/src/data_intent_tracker.rs`, `bundler/src/app.rs`
- Add `DELETE /v1/data/{id}` — requires signed request (same ECDSA scheme as POST) proving ownership
- Validates intent is still pending (not already packed into a TX)
- Removes from in-memory tracker and marks as cancelled in DB
- Add corresponding `cancel_data_intent()` to `bundler_client`

### [x] 2.2 Mark intents as finalized
- **File:** `bundler/src/app.rs:134` (TODO), `bundler/src/data_intent_tracker.rs`
- When `maybe_advance_anchor_block()` finalizes blocks, update `inclusion_finalized = true` on included intents
- This partially exists but the TODO indicates it's incomplete

### [x] 2.3 Drop inclusions for excluded transactions
- **File:** `bundler/src/app.rs:138` (TODO)
- When finalization excludes a repriced TX, clean up the `intent_inclusion` rows for that TX hash

### [x] 2.4 Prune finalized data
- **File:** `bundler/src/data_intent_tracker.rs:61` (TODO), new migration
- Add background task or hook in finalization: after N blocks past finalization, delete raw `data` column content from finalized intents (keep metadata)
- Consolidate anchor_block table to single row (anchor_block.rs:38 TODO)
- CLI arg: `--prune-after-blocks` (default 1000, 0 = disabled)

### [x] 2.5 Evict underpriced intents
- **File:** `bundler/src/data_intent_tracker.rs:22` (TODO)
- If an intent has been pending longer than a configurable threshold and its max_blob_gas_price is below the current network price, evict it and refund balance
- CLI arg: `--evict-stale-intent-hours` (default 24)

### [x] 2.6 Add API endpoint metrics
- **File:** `bundler/src/metrics.rs`, `bundler/src/lib.rs`
- Add actix-web middleware using `actix-web-prom` or manual `Histogram` tracking request duration + status code per route
- Add counters: `api_requests_total{method, path, status}`, `api_request_duration_seconds{method, path}`

### [x] 2.7 Improve background task error handling
- **Files:** `bundler/src/blob_sender_task.rs:32-50`, `bundler/src/block_subscriber_task.rs:45-51`
- Replace catch-all with categorized error handling: transient errors (network timeout, DB connection) → retry with backoff; permanent errors (invalid state) → log + skip; fatal errors → propagate
- Add retry counter metrics

### [x] 2.8 Nonce deadlock resolution
- **File:** `bundler/src/sync.rs:214-218` (TODO), `bundler/src/blob_sender_task.rs`
- Detect when all pending intents are underpriced relative to current gas and the sender nonce is stuck
- Send a self-transfer (0-value TX to self) to advance the nonce and unblock the pipeline
- Log a warning when this happens

---

## Phase 3: Beacon Consumer + Explorer UI

### [x] 3.1 Wire beacon consumer into the service
- **Files:** `bundler/src/lib.rs` (Args), `bundler/src/app.rs` (AppData), `bundler/src/consumer.rs`
- Add CLI args: `--beacon-api-url` (optional; enables consumer features when set)
- Add `BlobConsumer` (or just `BeaconApiClient`) to `AppData`
- Fix `consumer.rs:83` `assert_eq!(blob_sidecars.data.len(), 1)` — handle multi-blob transactions

### [x] 3.2 Add blob retrieval API endpoint
- **File:** new `bundler/src/routes/get_blobs.rs`, `bundler/src/routes/mod.rs`
- `GET /v1/blobs/{tx_hash}` — given a blob TX hash, fetch blob sidecars from beacon API, decode participants, return structured data
- Cache results in-memory (finalized blobs don't change)

### [x] 3.3 Add address history endpoint
- **Files:** `bundler/src/routes/mod.rs`, `bundler/src/data_intent_tracker.rs`
- `GET /v1/history/{address}` — return all intents (pending + included + finalized) for an address, with inclusion TX hashes and status
- Paginated with `?limit=N&offset=M`

### [x] 3.4 Enhance explorer UI
- **Files:** `bundler/src/explorer/mod.rs`, `bundler/static/templates/*.html`
- **Home page:** Add table of recent published blob TXs (from `intent_inclusion` table), show TX hash + block number + participant count + total data size
- **Address page:** Show intent history (pending, included, finalized), add cancel button for pending intents
- **Intent page:** Show inclusion status, TX hash link, published blob data (fetched via beacon consumer)
- **New page: `/tx/{hash}`** — show decoded blob TX details: participants, data sizes, blob gas used
- Add minimal CSS for readability

---

## Phase 4: Advanced Features

### [x] 4.1 Multi-sender support
- **Files:** `bundler/src/lib.rs`, `bundler/src/sync.rs:144,220`, `bundler/src/blob_sender_task.rs`, `bundler/src/app.rs`
- Accept `--mnemonic` as before but derive N sender wallets (add `--sender-count` arg, default 1)
- Refactor `BlockSync` to track per-sender: nonce, pending TXs, repriced TXs (currently single `target_address`)
- `blob_sender_task` picks the sender with lowest pending TX count for each new blob TX
- Balance topups go to any sender address (all are valid)

### [x] 4.2 Multi-blob data splitting
- **Files:** `bundler/src/routes/post_data.rs:23` (TODO), `bundler/src/data_intent.rs`, `bundler/src/packing.rs`, `bundler/src/blob_sender_task.rs`
- Allow `POST /v1/data` to accept data larger than `MAX_USABLE_BLOB_DATA_LEN`
- Split into chunks, each stored as a separate data intent linked by a `group_id`
- Packing treats each chunk independently but prefers co-locating chunks in the same TX
- Client receives a group ID to track all chunks
- Add `--max-data-size` arg (default `MAX_USABLE_BLOB_DATA_LEN * 6` = ~762KB, one full block)

### [x] 4.3 Improved gas estimation
- **Files:** `bundler/src/gas.rs`, `bundler/src/blob_sender_task.rs:124`
- Add proactive gas estimation: before packing, fetch current base fee + blob gas price, filter intents that can afford current prices
- Add priority fee validation (`gas.rs:39` TODO)
- Expose gas recommendation in `GET /v1/gas` response (suggested max_blob_gas_price for next-block inclusion)

### [x] 4.4 Minimum data charge
- **File:** `bundler/src/data_intent.rs:55` (TODO)
- Enforce minimum 31 bytes (one field element) for cost calculation, even if actual data is smaller
- Prevents too-small intents that waste packing space

### [x] 4.5 Grafana Cloud push metrics
- **File:** `bundler/src/metrics.rs:139` (TODO)
- Add `InfluxLine` variant to `PushMetricsFormat` enum
- Implement InfluxDB line protocol encoding for push gateway

---

## Execution Order & Dependencies

```
Phase 1 (all tasks independent, can be done in any order)
  1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.6 → 1.7 → 1.8

Phase 2 (some dependencies)
  2.1 (cancel) — independent
  2.2 (finalize marking) → 2.3 (excluded TX cleanup) → 2.4 (pruning)
  2.5 (eviction) — independent
  2.6 (API metrics) — independent
  2.7 (error handling) — independent
  2.8 (nonce deadlock) — independent

Phase 3 (sequential)
  3.1 (wire beacon) → 3.2 (blob retrieval endpoint) → 3.3 (address history) → 3.4 (explorer UI)

Phase 4 (mostly independent)
  4.1 (multi-sender) — independent, but large refactor
  4.2 (multi-blob) — independent
  4.3 (gas estimation) — independent
  4.4 (min data charge) — independent
  4.5 (Grafana push) — independent
```

## Phase 5: Post-Plan Improvements

### [x] 5.1 Dynamic gas limit for blob transactions
- **File:** `bundler/src/kzg.rs`
- **Change:** Replace hardcoded `gas_limit: 100_000` in `construct_blob_tx` with dynamic calculation based on actual calldata size. Gas = 21,000 (base) + calldata cost (4 per zero byte, 16 per non-zero byte) + 5,000 safety margin.
- **Why:** Hardcoded 100k gas wastes gas for small payloads and could be insufficient for very large participant lists. Dynamic calculation matches actual EVM intrinsic gas rules.

### [x] 5.2 Fix remaining production .unwrap() and add unit tests for untested modules
- **Files:** `bundler/src/blob_sender_task.rs`, `bundler/src/utils/option_hex_vec.rs`
- **Changes:**
  - Extract sender selection logic in `blob_sender_task.rs` into a standalone `pick_sender_with_fewest_pending()` function, replacing an unsafe `.unwrap()` pattern with idiomatic `min_by_key()`. Add 5 unit tests covering single sender, preference for fewer pending, tie-breaking, zero pending, and empty input.
  - Add 10 unit tests for `option_hex_vec.rs` serde module: serialize/deserialize for Some, None, empty bytes, roundtrip, invalid hex, and missing prefix cases.
- **Why:** The `.unwrap()` in sender selection could panic in theory (though guarded by short-circuit `||`). Extracting it makes it testable and idiomatic. `option_hex_vec.rs` had zero test coverage despite being used for data serialization across the API.

### [x] 5.3 Batch SQL bulk operations and add DataIntentTracker unit tests
- **File:** `bundler/src/data_intent_tracker.rs`
- **Changes:**
  - Add `SQL_BATCH_SIZE` constant (500) and refactor four bulk SQL functions (`fetch_many_data_intent_db_full`, `mark_data_intents_as_inclusion_finalized`, `set_finalized_block_number`, `insert_many_intent_tx_inclusions`) to chunk IDs into batches, preventing MySQL `max_allowed_packet` / bind-variable overflow with large intent sets.
  - Fix `mark_data_intents_as_inclusion_finalized` which incorrectly used `.fetch_all()` on an UPDATE statement (changed to `.execute()`).
  - Add early-return for empty `ids` slices in `fetch_many_data_intent_db_full` and `insert_many_intent_tx_inclusions`.
  - Add 10 unit tests for previously untested `DataIntentTracker` methods: `non_included_intents_total_cost` (3 tests), `non_included_intents_total_data_len` (2 tests), `get_all_intents` (2 tests), and `collect_metrics` (1 test).
- **Why:** Unbounded `IN (...)` and `VALUES (...)` clauses could exceed MySQL's query size limits under production load. The `.fetch_all()` on UPDATE was also semantically incorrect.

### [x] 5.4 Track non-blob sender transactions for correct nonce cleanup
- **File:** `bundler/src/sync.rs`
- **Changes:**
  - Added `NonBlobSenderTx` struct and `sender_non_blob_txs` field to `BlockSummary` to track non-blob transactions from sender addresses (e.g. self-transfers sent to resolve nonce deadlocks).
  - Updated `BlockSummary::from_block` to populate `sender_non_blob_txs` when a transaction from a target sender is not a blob transaction.
  - Updated `sync_block` to clear `pending_transactions` entries for included non-blob sender txs, preventing stale placeholders from persisting after on-chain inclusion.
  - Updated `drop_reorged_blocks` to restore pending placeholders when non-blob sender txs get reorged out.
  - Updated `maybe_advance_anchor_block` to clean up `repriced_transactions` entries at nonces consumed by finalized non-blob sender txs.
  - Added 5 unit tests: `from_block_parses_non_blob_sender_txs`, `from_block_ignores_non_blob_tx_from_non_sender`, `sync_block_clears_pending_for_non_blob_sender_tx`, `sync_block_no_op_when_no_matching_pending_for_non_blob_tx`, `reorg_restores_non_blob_sender_tx_placeholder`, `finalize_clears_repriced_for_non_blob_sender_tx`.
- **Why:** When a self-transfer (or any non-blob tx) from a sender address was included on-chain, `sync_block` didn't clear the corresponding entry from `pending_transactions` because `BlockSummary` only tracked blob transactions. This caused stale placeholder entries to persist indefinitely, affecting `pending_tx_count_for_sender()`, `detect_nonce_deadlock()`, and `balance_with_pending()`.

### [x] 5.5 Add unit tests for untested BlockSync methods
- **File:** `bundler/src/sync.rs`
- **Changes:**
  - Added 16 unit tests covering previously untested `BlockSync` methods:
    - `maybe_advance_anchor_block`: 3 tests — finalization advances anchor and returns included txs, no-op when chain is too short, cleaning up repriced blob txs on finalization.
    - `balance_with_pending`: 4 tests — no activity returns zero, finalized-only balance, unfinalized topups accumulate, pending txs subtract cost.
    - `pending_txs_data_len`: 3 tests — no pending returns zero, sums participant data across txs, ignores other participants.
    - `get_head` / `get_head_gas`: 2 tests — returns anchor when chain is empty, returns latest block when chain has blocks.
    - `SyncBlockOutcome::from_many`: 3 tests — all-known collapses to BlockKnown, mixed known+synced returns Synced with merged hashes, reorg is preserved.
    - `collect_metrics`: 1 test — does not panic with blocks in chain.
- **Why:** These core `BlockSync` methods had no direct unit tests. The finalization path (`maybe_advance_anchor_block`) is critical for correctness — it advances the anchor, cleans up repriced transactions, and computes finalized balances. `balance_with_pending` and `pending_txs_data_len` are used for user-facing balance/quota checks. `SyncBlockOutcome::from_many` merges multi-block sync results.

### [x] 5.6 Add unit tests for blob_tx_data.rs and packing.rs
- **Files:** `bundler/src/blob_tx_data.rs`, `bundler/src/packing.rs`
- **Changes:**
  - Added 18 unit tests to `blob_tx_data.rs` covering previously untested methods and edge cases:
    - `is_blob_tx`: 3 tests — type 3 returns true, type 2 returns false, None type returns false.
    - `encode_blob_tx_data`: 2 tests — empty participants returns single zero byte, single participant encodes version/data_len/address correctly.
    - `BlobTxParticipant::read`: 2 tests — invalid version returns error, truncated input returns error.
    - `BlobTxSummary::from_tx`: 4 tests — non-blob tx returns None, missing max_fee_per_gas/max_priority_fee/max_fee_per_blob_gas each return error, empty input produces empty participants.
    - `participation_count_from`: 3 tests — no match, single match, multiple matches.
    - `cost_to_intent` with block gas: 2 tests — verifies block gas prices affect cost, full blob has no unused space attribution.
    - `effective_gas_price` (indirect): 1 test — verifies effective gas uses priority+base_fee vs max_fee_per_gas.
    - `is_underpriced` delegation: 1 test — verifies BlobTxSummary delegates to GasConfig.
  - Added 16 unit tests to `packing.rs` covering previously untested functions:
    - `pack_items_greedy_sorted`: 5 tests — empty items, single item fills space, underpriced rejected, skips underpriced selects expensive, stops at max_len.
    - `pack_items` dispatching: 2 tests — uses brute force for small sets, uses greedy for large sets.
    - `sort_items`: 1 test — sorts ascending by len.
    - `is_sorted_ascending`: 5 tests — empty, single, sorted, equal elements, unsorted.
    - `Item::with_group` / `Item::new`: 2 tests — group_id stored correctly, new has no group.
- **Why:** `blob_tx_data.rs` contains critical blob transaction parsing and cost calculation logic used across the API and sync paths, but error paths (`from_tx` with missing fields, invalid participant version) had zero test coverage. `packing.rs` had tests for brute_force and knapsack but none for the greedy algorithm (`pack_items_greedy_sorted`) which is the production path for >8 items, nor for helper functions like `sort_items` and `is_sorted_ascending`.

---

## Key Files Modified Across All Phases

| File | Phases |
|------|--------|
| `bundler/src/lib.rs` | 1.1, 1.2, 1.4, 1.5, 1.6, 1.8, 3.1, 4.1, 4.2 |
| `bundler/src/app.rs` | 2.1, 2.2, 2.3, 2.4, 3.1, 4.1 |
| `bundler/src/sync.rs` | 1.2, 2.8, 4.1 |
| `bundler/src/data_intent_tracker.rs` | 2.1, 2.2, 2.4, 2.5, 3.3 |
| `bundler/src/routes/mod.rs` | 1.7, 2.1, 3.2, 3.3 |
| `bundler/src/metrics.rs` | 1.1, 1.2, 2.6, 4.5 |
| `bundler/src/explorer/mod.rs` | 1.2, 3.4 |
| `bundler/src/blob_sender_task.rs` | 2.7, 2.8, 4.1, 4.2 |
| `bundler/src/gas.rs` | 4.3 |
| `bundler/src/consumer.rs` | 3.1 |
| `bundler/src/packing.rs` | 4.2 |
| `bundler/src/routes/post_data.rs` | 4.2, 4.4 |
| `bundler/src/data_intent.rs` | 4.2, 4.4 |
