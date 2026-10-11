// Starts the server over stdio with the MCP client SDK and calls its tools against KELVIN_API.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const transport = new StdioClientTransport({
  command: process.execPath,
  args: ["dist/index.js"],
  env: { ...process.env, KELVIN_API: process.env.KELVIN_API ?? "http://127.0.0.1:8080" },
});
const client = new Client({ name: "kelvin-test", version: "0.0.0" });
await client.connect(transport);

test("lists the tools", async () => {
  const { tools } = await client.listTools();
  assert.deepEqual(tools.map((t) => t.name).sort(), [
    "address_activity", "address_history", "address_summary", "address_transfers",
    "block", "network_status", "rpc_conformance", "transaction",
  ]);
  assert.ok(tools.every((t) => t.annotations?.readOnlyHint));
});

test("address_summary answers with JSON", async () => {
  const r = await client.callTool({ name: "address_summary", arguments: { address: "t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG" } });
  const body = JSON.parse(r.content[0].text);
  assert.ok(body.transaction_count > 20);
});

test("address_history pages with a cursor", async () => {
  const r = await client.callTool({ name: "address_history", arguments: { address: "t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG", limit: 5 } });
  const page = JSON.parse(r.content[0].text);
  assert.equal(page.items.length, 5);
  assert.ok(page.next_cursor);
});

test("a bad signature is a tool error, not a crash", async () => {
  const r = await client.callTool({ name: "transaction", arguments: { signature: "nope" } });
  assert.equal(r.isError, true);
  assert.match(r.content[0].text, /64 bytes|base58/);
});

test.after(() => client.close());
