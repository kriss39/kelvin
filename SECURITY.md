# Security

Please report vulnerabilities privately through GitHub:
[Report a vulnerability](https://github.com/kriss39/kelvin/security/advisories/new). Do not open
a public issue for them.

## Scope

- The indexer, API and explorer in this repository.
- The public instance at <https://kelvinscan.pages.dev>.
- The npm packages `kelvin-rialo` and `kelvin-rialo-mcp`.

## How the public instance is exposed

- The API is read-only. It serves public chain data and holds no user accounts or keys.
- The server opens no inbound ports to the internet. Traffic arrives through an outbound
  Cloudflare tunnel. SSH is allowed from one administrator address only.
- Each client gets 10 requests per second (bursts of 40). Queries time out after 15 s, and at
  most 3 CSV exports run at once.
