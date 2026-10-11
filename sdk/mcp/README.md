# kelvin-rialo-mcp

An [MCP](https://modelcontextprotocol.io) server that lets AI agents read Rialo history through a
[Kelvin](../../README.md) API. All tools are read-only.

| Tool | Answers |
|---|---|
| `address_summary` | Transaction count, first/last activity, RLO in and out, live balance |
| `address_history` | A page of an address's transactions (the RPC alone stops at 20) |
| `address_transfers` | A page of RLO and token transfers with direction |
| `address_activity` | Transactions per day and top counterparties |
| `transaction` | Status, time, fee, accounts, decoded instructions, transfers, logs |
| `block` | A block with its user transactions |
| `network_status` | Indexing status, the last hour of activity, validators |
| `rpc_conformance` | The public RPC's known shortcomings |

## Use

```sh
cd sdk/typescript && npm install && npm run build
cd ../mcp && npm install && npm run build
```

Claude Code:

```sh
claude mcp add kelvin -e KELVIN_API=https://<kelvin-host> -- node /path/to/kelvin/sdk/mcp/dist/index.js
```

Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "kelvin": {
      "command": "node",
      "args": ["/path/to/kelvin/sdk/mcp/dist/index.js"],
      "env": { "KELVIN_API": "https://<kelvin-host>", "KELVIN_NETWORK": "testnet" }
    }
  }
}
```

Then ask, for example: *"How many transactions has t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG made,
and what did it do first?"*

`npm test` starts the server with the MCP client SDK and calls its tools against `KELVIN_API`.
