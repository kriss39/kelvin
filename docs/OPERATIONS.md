# Operations

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | none (`.env` is read) | Postgres connection string |
| `RUST_LOG` | `info` | Log filter, e.g. `info,kelvin=debug` |
| `LOG_FORMAT` | text | `json` for one JSON object per line |

Migrations run automatically at start-up. Networks and their RPC URLs live in the `networks`
table.

## Commands

| Command | Purpose |
|---|---|
| `kelvin run [--network N] [--api ADDR] [--no-backfill] [--backfill-workers 32] [--follow-workers 8]` | The indexer: head follower, backfill, hourly jobs, optionally the API |
| `kelvin serve [--addr ADDR]` | The API alone, e.g. a second instance in front of the same database |
| `kelvin status [--network N]` | Cursors, lag, missing blocks, retention samples, recent events |
| `kelvin conformance` | Run the RPC checks once and print them |
| `kelvin retry-gaps` | Re-fetch heights recorded as not found or not listed |
| `kelvin redecode` | Fill in kinds and transfers after a decoder was added |
| `kelvin range --from A --to B` | Index a fixed range once and print throughput and storage figures |
| `kelvin sizes` | Rows and bytes per table |

## Running locally

```sh
docker compose up -d db
cp .env.example .env
cargo build --release
scripts/indexer.sh start      # run --api 127.0.0.1:8080 in the background, Mac kept awake
scripts/indexer.sh status
scripts/indexer.sh stop       # finishes the current batch; progress is kept
```

Logs go to `logs/indexer-testnet.log`.

For a stable address, `deploy/pages/deploy.sh [backend-url]` publishes
`https://kelvinscan.pages.dev` on Cloudflare Pages (free): the explorer and docs are static there,
and `/v1/*` and `/healthz` are forwarded to the Kelvin server (by default the current tunnel). When
the server moves, run it again with the new URL; the public address stays the same. The first run
asks to log in to Cloudflare. `scripts/tunnel.sh start` (also run by `scripts/indexer.sh start`)
redeploys it automatically whenever the tunnel comes up with a new address.

To share the local explorer, `scripts/tunnel.sh start` opens a Cloudflare quick tunnel and prints
an `https://<random>.trycloudflare.com` address (`stop`, `url` as well). The address changes on
every start and works while the Mac is on. Quick tunnels hold server-sent events back in 128 KiB
chunks; the explorer notices the missing `hello` event within 6 s and polls every 4 s instead. A restart, a crash or a sleeping laptop loses nothing:
both cursors are committed with the data.

## Deploying (Oracle Cloud Always Free)

The target is an Ampere A1 machine (arm64) running Ubuntu 24.04 with a 150 GB boot volume.

1. **Get the machine.** Ampere capacity is often exhausted. `deploy/oci-launch.sh` keeps trying
   to create an instance named `kelvin` (2 OCPU / 12 GB, then 1 OCPU / 6 GB) every minute until
   it succeeds, using `~/.oci/config` and the subnet `kelvin-public`. It never creates a second
   instance.
2. **Ship it.** `deploy/deploy.sh ubuntu@<ip> --restore` builds the arm64 image locally, copies
   it and `deploy/compose.yaml` over SSH, installs Docker if needed, loads this machine's
   database into the server (first time only) and starts the stack. It prints the public URL.
3. **Later releases.** `deploy/deploy.sh ubuntu@<ip>` without `--restore`.

The stack (`deploy/compose.yaml`) is Postgres, `kelvin run --api` and a Cloudflare quick tunnel.
The tunnel gives an `https://<random>.trycloudflare.com` address without opening any port; it
changes when the tunnel container restarts. For a stable address, use a named Cloudflare tunnel on
your own domain.

On the server:

```sh
cd /opt/kelvin
sudo docker compose ps
sudo docker compose logs -f kelvin
sudo docker compose exec kelvin kelvin status
sudo docker compose logs tunnel | grep trycloudflare.com    # the public URL
```

## Monitoring

- `GET /healthz`: 200 with each network's lag in blocks, 503 if the database is unreachable.
  Point an uptime checker at it.
- `kelvin status` or `GET /v1/{network}/status`: lag, backfill progress, missing blocks.
- `events` table: `reset_suspected` (a stored block no longer matches the RPC, or the head moved
  backwards) and `backfill_complete`.
- `GET /v1/{network}/rpc-conformance`: a change in the RPC's behaviour appears as a flipped check.

Expected values: follower lag under 30 blocks (about 4 s); missing blocks 0 or slowly shrinking
through the hourly gap retry.

## Maintenance

- **New decoder.** Deploy, then `kelvin redecode`; it touches only instructions without a kind.
- **Missing blocks.** The hourly gap retry handles transient ones. Ranges with reason `pruned`
  were gone from the RPC before Kelvin reached them and cannot be recovered.
- **Network reset.** If `reset_suspected` appears and is confirmed (devnet has been reset before
  without notice), stop the indexer and move that network's rows aside before re-indexing; the
  cursors in `ingest_state` decide where indexing restarts.
- **Backups.** `docker compose exec -T db pg_dump -U kelvin -Fc kelvin > kelvin.dump`, restored
  with `pg_restore --no-owner`. Copying dumps to object storage (Cloudflare R2) is planned.

## Capacity

At October 2026 testnet activity Kelvin stores about 1.4 KB per block and the chain makes about
7 blocks per second: roughly 0.9 GB per day. The full retained window (14.8 M blocks) is
15–21 GB. A 150 GB volume lasts about four months at that rate; extend it before it fills.
