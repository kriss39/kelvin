// Runs against a Kelvin server: KELVIN_API (default http://127.0.0.1:8080), testnet.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Kelvin, KelvinError, formatRlo } from "../dist/index.js";

const kelvin = new Kelvin({ baseUrl: process.env.KELVIN_API ?? "http://127.0.0.1:8080" });
const ORACLE_REPORTER = "t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG";

test("formatRlo is exact at any size", () => {
  assert.equal(formatRlo("1500000000"), "1.5");
  assert.equal(formatRlo("3995872991362504163"), "3995872991.362504163");
  assert.equal(formatRlo(-8553840n), "-0.00855384");
  assert.equal(formatRlo("0"), "0");
});

test("status", async () => {
  const s = await kelvin.status();
  assert.equal(s.network, "testnet");
  assert.ok(s.indexed_tip > 0);
});

test("history pages through in order without repeats", async () => {
  const seen = new Set();
  let previous = [Infinity, Infinity];
  for await (const tx of kelvin.address(ORACLE_REPORTER).history({ limit: 100 })) {
    assert.ok(tx.block < previous[0] || (tx.block === previous[0] && tx.index < previous[1]), "newest first");
    assert.ok(!seen.has(tx.signature), "no repeats");
    seen.add(tx.signature);
    previous = [tx.block, tx.index];
    if (seen.size === 250) break;
  }
  assert.equal(seen.size, 250);
});

test("transaction detail and workflow", async () => {
  const [first] = (await kelvin.address(ORACLE_REPORTER).txs({ limit: 1 })).items;
  const tx = await kelvin.tx(first.signature);
  assert.equal(tx.signature, first.signature);
  assert.ok(tx.time.endsWith("Z"));
  const wf = await kelvin.workflow(first.signature);
  assert.ok(Array.isArray(wf.nodes));
});

test("errors carry the HTTP status", async () => {
  await assert.rejects(kelvin.block(5), (e) => e instanceof KelvinError && e.status === 404);
  await assert.rejects(kelvin.tx("nope"), (e) => e instanceof KelvinError && e.status === 400);
});

test("subscribe delivers new blocks (polling in Node)", async () => {
  const block = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("no block within 20 s")), 20000);
    const stop = kelvin.subscribe({
      pollIntervalMs: 1500,
      onBlock: (b) => { clearTimeout(timer); stop(); resolve(b); },
      onError: reject,
    });
  });
  assert.ok(block.height > 0);
});
