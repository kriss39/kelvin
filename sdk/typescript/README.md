# kelvin-rialo

TypeScript client for the [Kelvin](../../README.md) API: full, pageable Rialo history, decoded
transactions and live updates. No dependencies; browsers and Node 18+.

```ts
import { Kelvin, formatRlo } from "kelvin-rialo";

const kelvin = new Kelvin({ baseUrl: "https://<kelvin-host>", network: "testnet" });

// Every transaction of an address; pages are fetched as you iterate.
for await (const tx of kelvin.address("t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG").history({ order: "asc" })) {
  console.log(tx.time, tx.signature, tx.status);
}

// One page, if you manage cursors yourself.
const page = await kelvin.address(addr).txs({ limit: 50 });
const next = await kelvin.address(addr).txs({ limit: 50, cursor: page.next_cursor! });

// Details.
const summary = await kelvin.address(addr).summary();  // count, first/last seen, RLO in/out, balance
console.log(formatRlo(summary.balance ?? "0"), "RLO");
const tx = await kelvin.tx(signature);                  // accounts, instructions, transfers, logs
const block = await kelvin.block(16635551);

// Live: server-sent events where available, polling otherwise.
const stop = kelvin.subscribe({ address: addr, onTx: (tx) => console.log("new", tx.signature) });
```

Also: `status()`, `activity()` (the last hour), `network()` (validators, DKG committee),
`conformance()` (the RPC's shortcomings), `workflow(signature)`, `address(a).transfers()`,
`.allTransfers()`, `.activity()`, `.rex()`, `.csvUrl()`, `blocks()`, `programTxs()`.

Amounts are decimal strings in kelvin (1 RLO = 10⁹ kelvin); `formatRlo` converts exactly.
Errors are `KelvinError` with the HTTP `status`.

```sh
npm install && npm test          # tests run against KELVIN_API (default http://127.0.0.1:8080)
node examples/history.mjs <address>
node examples/live.mjs [address]
```
