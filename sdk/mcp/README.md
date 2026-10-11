# kelvin-rialo-mcp

An [MCP](https://modelcontextprotocol.io) server that lets AI agents read Rialo history through a
[Kelvin](https://kelvinscan.pages.dev) API. All tools are read-only.

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

It talks to the public Kelvin at https://kelvinscan.pages.dev unless `KELVIN_API` says otherwise.

Claude Code:

```sh
claude mcp add kelvin -- npx -y kelvin-rialo-mcp
```

Claude Desktop (`claude_desktop_config.json`), or any MCP client that starts stdio servers:

```json
{
  "mcpServers": {
    "kelvin": {
      "command": "npx",
      "args": ["-y", "kelvin-rialo-mcp"],
      "env": { "KELVIN_NETWORK": "testnet" }
    }
  }
}
```

Set `KELVIN_API` to use your own Kelvin server.

Then ask, for example: *"How many transactions has t2J87f5HDtd4UkpszzBCm6nfbRgkJHRrbJtxuWpwVAG made,
and what did it do first?"*

From a checkout, `npm test` starts the server with the MCP client SDK and calls its tools against
`KELVIN_API`.
