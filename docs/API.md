# Using the API

The full reference is the OpenAPI spec (`src/openapi.yaml`), browsable at `/docs` on any Kelvin
server. This page covers the conventions and the common tasks.

Base path: `/v1/{network}`, where `network` is `testnet` or `devnet`. All responses are JSON
except `/stream`. Any origin may call the API from a browser.

## Conventions

| Topic | Rule |
|---|---|
| Amounts | Decimal strings in kelvin (1 RLO = 10⁹ kelvin). They can exceed 2⁵³, so parse them as big integers. Token amounts are in the mint's base units. |
| Times | `time` is ISO 8601 UTC, `time_ms` milliseconds since the epoch. Rialo block times have one-second resolution. |
| Addresses, signatures | Base58, as everywhere on Rialo. |
| Paging | Lists return `{ "items": [...], "next_cursor": "…" }`. Pass `next_cursor` back as `cursor` to continue; `null` means the end. `limit` is 1–100 (default 25), `order` is `desc` (newest first, default) or `asc`. |
| Errors | `{ "error": "…" }` with 400 for a malformed parameter, 404 for something not in the index (the message says whether a block was pruned before Kelvin started, could not be fetched, or is not indexed yet), 503 for a timeout. |
| Caching | Blocks and transactions are final: `Cache-Control: public, max-age=86400`. Lists and status: `max-age=5`. |

## Full history of an address

```sh
B=https://<kelvin-host>/v1/testnet
A=8GFLC4J4h8AGGwku4JVzeWrkdY9oeWkZNUeQfsFkNN2p

curl "$B/address/$A"                       # count, first/last seen, RLO in and out, live balance
curl "$B/address/$A/txs?limit=100"         # newest 100
curl "$B/address/$A/txs?limit=100&cursor=31442600:0"   # the next 100
curl "$B/address/$A/txs?order=asc&limit=10"            # the oldest 10
curl "$B/address/$A/transfers?limit=50"    # RLO and token movements with direction
```

Each transaction summary carries the address's `role` (`signer`, `writable`), the top-level
instructions with decoded `kind`, and the transfers that involve the address.

A whole history, in JavaScript:

```js
async function* history(base, address) {
  let cursor = null;
  do {
    const url = `${base}/address/${address}/txs?limit=100${cursor ? `&cursor=${cursor}` : ""}`;
    const page = await (await fetch(url)).json();
    yield* page.items;
    cursor = page.next_cursor;
  } while (cursor);
}

for await (const tx of history("https://<kelvin-host>/v1/testnet", address)) {
  console.log(tx.time, tx.signature, tx.status);
}
```

## A transaction or a block

```sh
curl "$B/tx/<signature>"     # accounts with roles, all instructions (inner ones nested), transfers, logs
curl "$B/blocks/16635551"    # block time, counts and its user transactions
curl "$B/blocks?limit=10"    # latest indexed blocks
curl "$B/programs/Qrac1eProcessor1111111111111111111111111111/txs"   # transactions invoking a program
```

Unlike the RPC's `getTransaction`, every transaction comes with its block time.

## Live stream

`GET /stream` is a server-sent event stream fed by the head follower, about a second behind the
chain:

- `event: hello`, sent at once on connect (if it does not arrive, a proxy is buffering the
  stream; poll `/blocks` and `/address/{address}/txs` instead)
- `event: block`, data: a block summary (height, time, transaction counts)
- `event: tx`, data: a transaction summary
- `event: lagged`, data: how many events a slow client missed

With `?address=…`, only that address's transactions are sent.

```js
const es = new EventSource("https://<kelvin-host>/v1/testnet/stream?address=" + address);
es.addEventListener("tx", (e) => {
  const tx = JSON.parse(e.data);
  console.log("new transaction", tx.signature, tx.transfers);
});
```

## Activity

- `GET /activity`: the last hour on the network (about 25,000 blocks): transactions, failures,
  distinct fee payers, RLO moved, token transfers and the programs called.
- `GET /address/{address}/activity`: the address's transactions per UTC day and its six most
  frequent RLO counterparties. Days the backfill has not reached yet are absent, not zero.

## Status and RPC checks

- `GET /status`: chain head, indexed tip and lag, backfill progress, the oldest block the RPC
  still serves, missing blocks and approximate totals.
- `GET /rpc-conformance`: the RPC's known shortcomings, each with the latest observation and
  since when it has held.
- `GET /healthz` (outside `/v1`): 200 with each network's lag, or 503 if the database is down.
