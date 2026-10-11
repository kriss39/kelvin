# Kelvin

Full, pageable transaction history for [Rialo](https://rialo.io).

Rialo's RPC answers `getSignaturesForAddress` with the newest 20 signatures and ignores `limit` and
`before`, so older history cannot be reached at all. Blocks older than about three weeks are pruned.
Kelvin indexes every block into Postgres and serves the full history through a small REST API.

*1 RLO = 10⁹ kelvin, the smallest unit of Rialo's native token.*

## Status

Early, working, testnet first.

- Head follower: about 1–4 seconds behind the chain.
- Backfill: walks forward from the oldest block the RPC still serves (16,635,303 on testnet,
  17 Sep 2026) at 130–220 blocks/s.
- Decoding: System and Compute Budget instructions, with native RLO transfers including inner
  instructions. Other programs keep their raw instruction data.
- REST API with OpenAPI docs.

## What the RPC cannot do, and Kelvin does

Measured against `testnet.rialo.io`, RPC version 0.21.0-alpha.0, on 11 Oct 2026.

| Need | Rialo RPC | Kelvin |
|---|---|---|
| Address history | Newest 20 only; `limit` and `before` ignored | Every indexed transaction, cursor paging both ways |
| Old blocks | Nothing below 16,635,303 | Keeps every block it indexed |
| Transaction time | `getTransaction` returns `block_time: null` | `time` on every transaction |
| Time units | Seconds in `getBlock`, milliseconds in `getSignaturesForAddress` | ISO 8601 plus milliseconds everywhere |
| Balance changes | No pre/post balances in transaction meta | RLO transfers decoded from instructions |
| Large amounts | Balances exceed 2⁵³ | Amounts as decimal strings |

## Quick start

Needs Docker and Rust 1.85 or newer.

```sh
docker compose up -d db          # Postgres 16 on 127.0.0.1:54329
cp .env.example .env
cargo build --release
./target/release/kelvin run --network testnet --api 127.0.0.1:8080
```

`run` applies migrations, starts the head follower and the backfill, and serves the API. Stop it
with Ctrl-C; it resumes from its saved cursors. Then open <http://127.0.0.1:8080/docs>.

Other commands:

| Command | What it does |
|---|---|
| `kelvin run [--api ADDR] [--no-backfill]` | Follow the head, backfill below it, optionally serve the API |
| `kelvin serve [--addr ADDR]` | Serve the API only |
| `kelvin status` | Cursors, lag, missing blocks, the RPC's retention window, recent events |
| `kelvin range --from A --to B` | Index a fixed range once and print throughput and storage figures |
| `kelvin sizes` | Rows and bytes per table |

`scripts/indexer.sh start | stop | status` wraps `run` for a laptop: it starts Docker if needed and
keeps the Mac awake while indexing.

## API

All paths are under `/v1/{network}`, where `network` is `testnet` or `devnet`.

| Endpoint | Returns |
|---|---|
| `GET /status` | Head, indexed tip, lag, backfill progress |
| `GET /address/{address}` | Transaction count, first and last activity, RLO in and out, live balance |
| `GET /address/{address}/txs` | The address's transactions, newest first (`order=asc` for oldest first) |
| `GET /address/{address}/transfers` | RLO transfers to and from the address |
| `GET /tx/{signature}` | Accounts and roles, all instructions, transfers, logs |
| `GET /blocks`, `GET /blocks/{height}` | Indexed blocks, and one block with its transactions |
| `GET /programs/{program}/txs` | Transactions that invoke a program |

Lists take `limit` (1–100, default 25) and return `next_cursor`; pass it back as `cursor`.
Amounts are strings in kelvin. The full spec is in [`src/openapi.yaml`](src/openapi.yaml).

```sh
curl "http://127.0.0.1:8080/v1/testnet/address/8GFLC4J4h8AGGwku4JVzeWrkdY9oeWkZNUeQfsFkNN2p/txs?limit=2"
```

## How it works

```
Rialo RPC ──getBlocks / getBlock──▶ ingest ──▶ normalize + decode ──▶ Postgres ──▶ REST API
             (parallel, in order)     │                                  ▲
                                      └── head follower + backfill ──────┘ cursors in ingest_state
```

- One `getBlock` per height carries everything: transactions, meta, logs and inner instructions.
- Requests run in parallel but are consumed in block order, so each committed batch moves a cursor
  and a restart repeats no work. Writes are idempotent.
- Heights the RPC cannot serve are recorded in `missing_blocks` with a reason instead of being
  skipped silently.
- Addresses are stored once in an `addresses` dictionary and referenced by 8-byte ids, which cut
  storage by about 29%. Rows take about 1.4 KB per block at current testnet activity.
- System transactions (one `TokenomicsGovernance` call per block) are counted, not stored.
- Blocks are final as soon as the RPC serves them, so the follower stays 2 blocks behind the head.
  A periodic check compares stored blocks with the RPC to notice network resets.

## Layout

```
src/
  rpc.rs        JSON-RPC client with per-method parameter shapes and retries
  model.rs      getBlock response types
  decode.rs     program labels and instruction decoders
  normalize.rs  blocks to column vectors for batch inserts
  ingest.rs     the fetch pipeline
  indexer.rs    head follower, backfill and periodic checks
  db.rs         Postgres writes and queries
  api.rs        HTTP API
migrations/     schema
scripts/        indexer.sh, rpc_probe.py
```

## License

Apache-2.0. See [LICENSE](LICENSE).
