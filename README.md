<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="brand/kelvin-logo-dark.png">
    <img src="brand/kelvin-logo-light.png" alt="Kelvin: Rialo history, down to the last kelvin" width="460">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/kriss39/kelvin/actions/workflows/ci.yml"><img src="https://github.com/kriss39/kelvin/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-0a6c8c" alt="Apache-2.0">
  <img src="https://img.shields.io/badge/Rialo-testnet-7048e8" alt="Rialo testnet">
</p>

**Kelvin** indexes every [Rialo](https://rialo.io) block and serves the full history the RPC
cannot: every transaction of an address, pageable both ways, with block times, decoded
instructions and value movements, plus a live feed and an explorer.

Rialo's RPC answers `getSignaturesForAddress` with the newest 20 signatures and ignores `limit`
and `before`, so older history cannot be reached. Blocks older than 17 September 2026 are gone.
Kelvin keeps all of it.

*1 RLO = 10⁹ kelvin, the smallest unit of Rialo's native token.*

<p align="center"><img src="docs/img/explorer-home.png" alt="Kelvin explorer home: the RPC's 20 transactions next to Kelvin's full count, indexing status, block-height ruler and an activity heat chart" width="900"></p>

## What the RPC cannot do, and Kelvin does

Measured against `testnet.rialo.io` (0.21.0-alpha.0) and re-checked every hour by Kelvin itself at
`/v1/testnet/rpc-conformance`.

| Need | Rialo RPC | Kelvin |
|---|---|---|
| Address history | Newest 20 only; `limit` and `before` ignored | Every transaction, cursor paging both ways |
| Old blocks | Nothing below block 16,635,303 | Keeps every block it indexed |
| Transaction time | `getTransaction` returns `block_time: null` | `time` on every transaction |
| Time units | Seconds in `getBlock`, milliseconds in `getSignaturesForAddress` | ISO 8601 plus milliseconds everywhere |
| Balance changes | No pre/post balances in transaction meta | RLO and token movements decoded from instructions |
| Large amounts | Balances exceed 2⁵³ | Amounts as decimal strings |
| Live updates | Polling | Server-sent events, optionally for one address |

## Status

Working on testnet; devnet next.

- **Head follower** 2 blocks behind the chain, about 1–4 seconds of lag.
- **Backfill** from the oldest block the RPC serves at 130–220 blocks/s; no missing blocks so far.
- **Decoding** of System, Compute Budget, Token-2022 / SPL Token, Associated Token and oracle
  instructions: 98.9% of instructions; the rest keep their raw data.
- **API** with OpenAPI docs, a live stream, CSV export and hourly RPC conformance checks.
- **TypeScript SDK** and an **MCP server** for AI agents.
- **Explorer** at `/`: live feed, the last hour by program, an activity heat chart, address pages
  (daily activity, top counterparties, REX requests), transaction pages (workflow lineage), block
  pages and a network page (validators, DKG committee, epoch).

## Quick start

Needs Docker and Rust 1.85 or newer.

```sh
docker compose up -d db          # Postgres 16 on 127.0.0.1:54329
cp .env.example .env
cargo build --release
./target/release/kelvin run --network testnet --api 127.0.0.1:8080
```

Open <http://127.0.0.1:8080/> for the explorer and <http://127.0.0.1:8080/docs> for the API
reference. `run` applies migrations, starts the head follower and the backfill and serves the API;
stop it with Ctrl-C and it resumes from its saved cursors.

```sh
curl "http://127.0.0.1:8080/v1/testnet/address/8GFLC4J4h8AGGwku4JVzeWrkdY9oeWkZNUeQfsFkNN2p/txs?limit=2"
```

## Documentation

| | |
|---|---|
| [docs/API.md](docs/API.md) | Conventions, paging, the live stream, examples in curl and JavaScript |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Pipeline, cursors, finality, storage design, decoding, conformance, testing |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | Commands, configuration, deploying to Oracle Cloud, monitoring, maintenance |
| [sdk/typescript](sdk/typescript/README.md) | TypeScript client: typed calls, history iterators, live subscriptions |
| [sdk/mcp](sdk/mcp/README.md) | MCP server so AI agents can query Rialo history |
| [brand/](brand/README.md) | Logo, colours and the link-preview card |
| [`src/openapi.yaml`](src/openapi.yaml) | The API specification |

## Layout

```
src/
  rpc.rs          JSON-RPC client with per-method parameter shapes and retries
  model.rs        getBlock response types
  decode.rs       program labels and instruction decoders
  normalize.rs    blocks to column vectors for batch inserts
  ingest.rs       the ordered, parallel fetch pipeline
  indexer.rs      head follower, backfill and hourly jobs
  conformance.rs  RPC behaviour checks
  redecode.rs     decode stored instructions again
  db.rs           Postgres writes and queries
  api.rs          HTTP API, live stream, explorer
  web/            the explorer page
migrations/       schema
deploy/           Docker image, compose stack, deploy and instance scripts
sdk/typescript/   TypeScript client
sdk/mcp/          MCP server for AI agents
tests/fixtures/   real testnet blocks for the normalizer tests
```

## License

Apache-2.0. See [LICENSE](LICENSE).
