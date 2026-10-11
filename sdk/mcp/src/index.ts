#!/usr/bin/env node
/**
 * MCP server over stdio that gives AI agents Rialo history through a Kelvin API.
 *
 *   KELVIN_API      Kelvin server, default http://127.0.0.1:8080
 *   KELVIN_NETWORK  testnet (default) or devnet
 */
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { Kelvin, type Network } from "kelvin-rialo";
import { z } from "zod";

const kelvin = new Kelvin({
  baseUrl: process.env.KELVIN_API ?? "http://127.0.0.1:8080",
  network: (process.env.KELVIN_NETWORK as Network | undefined) ?? "testnet",
});

const server = new McpServer({ name: "kelvin", version: "0.1.0" });

const readOnly = { readOnlyHint: true, openWorldHint: true } as const;
const address = z.string().describe("Rialo address, base58");
const order = z.enum(["desc", "asc"]).optional().describe("desc: newest first (default); asc: oldest first");

/** Tool results are JSON text; errors come back as tool errors the agent can read. */
async function answer(work: () => Promise<unknown>) {
  try {
    return { content: [{ type: "text" as const, text: JSON.stringify(await work(), null, 2) }] };
  } catch (e) {
    return { isError: true, content: [{ type: "text" as const, text: e instanceof Error ? e.message : String(e) }] };
  }
}

server.registerTool(
  "address_summary",
  {
    title: "Address summary",
    description:
      "Transaction count, first and last activity, RLO received and sent, and live balance of a Rialo address. " +
      "Amounts are strings in kelvin (1 RLO = 1e9 kelvin).",
    inputSchema: { address },
    annotations: readOnly,
  },
  ({ address }) => answer(() => kelvin.address(address).summary()),
);

server.registerTool(
  "address_history",
  {
    title: "Address transaction history",
    description:
      "One page of an address's transactions (the Rialo RPC itself only returns the newest 20). " +
      "Pass next_cursor back as cursor for the following page.",
    inputSchema: {
      address,
      limit: z.number().int().min(1).max(100).optional().describe("1-100, default 25"),
      order,
      cursor: z.string().optional(),
    },
    annotations: readOnly,
  },
  ({ address, limit, order, cursor }) => answer(() => kelvin.address(address).txs({ limit, order, cursor })),
);

server.registerTool(
  "address_transfers",
  {
    title: "Address transfers",
    description: "One page of RLO and token transfers to and from an address, with direction.",
    inputSchema: { address, limit: z.number().int().min(1).max(100).optional(), order, cursor: z.string().optional() },
    annotations: readOnly,
  },
  ({ address, limit, order, cursor }) => answer(() => kelvin.address(address).transfers({ limit, order, cursor })),
);

server.registerTool(
  "address_activity",
  {
    title: "Address activity",
    description: "Transactions per UTC day and the top RLO counterparties of an address.",
    inputSchema: { address },
    annotations: readOnly,
  },
  ({ address }) => answer(() => kelvin.address(address).activity()),
);

server.registerTool(
  "transaction",
  {
    title: "Transaction",
    description:
      "A transaction by signature: time, status and error, fee, accounts and roles, decoded instructions (inner ones included), transfers and logs.",
    inputSchema: { signature: z.string().describe("Transaction signature, base58") },
    annotations: readOnly,
  },
  ({ signature }) => answer(() => kelvin.tx(signature)),
);

server.registerTool(
  "block",
  {
    title: "Block",
    description: "A block by height with its user transactions.",
    inputSchema: { height: z.number().int().min(0) },
    annotations: readOnly,
  },
  ({ height }) => answer(() => kelvin.block(height)),
);

server.registerTool(
  "network_status",
  {
    title: "Network status",
    description:
      "Indexing status (head, lag, backfill), the last hour of activity (transactions, success rate, active signers, RLO moved, programs called) and the validator set.",
    annotations: readOnly,
  },
  () => answer(async () => ({ status: await kelvin.status(), last_hour: await kelvin.activity(), network: await kelvin.network() })),
);

server.registerTool(
  "rpc_conformance",
  {
    title: "Rialo RPC shortcomings",
    description: "Known shortcomings of the public Rialo RPC, re-checked hourly, with what was observed and since when.",
    annotations: readOnly,
  },
  () => answer(() => kelvin.conformance()),
);

await server.connect(new StdioServerTransport());
