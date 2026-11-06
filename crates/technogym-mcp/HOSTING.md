# Hosting the MCP server

Operator notes for `technogym-mcp serve-http`. Users do not need this; see [README.md](README.md).

## How it works

Each user adds the server as a connector (claude.ai: Settings → Connectors → Add custom
connector, URL `https://<host>/mcp`) and signs in with their own mywellness email and password on
the server's sign-in page. The password goes to mywellness only and is never stored. The
mywellness bearer token is sealed (AES-256-GCM, `SEAL_KEYS`) into the OAuth tokens the client
holds, so the server keeps **no state**: no database, no files, nothing per user.

mywellness has no refresh token. Its bearer token lives one hour and is renewed by using it; each
OAuth refresh (every 20 minutes while the connector is in use) makes one cheap mywellness call and
seals the renewed token. After an hour without use the token is gone: the refresh fails and the
user reconnects.

Endpoints, all under one host:

```
GET  /mcp/healthz                                     "ok"
GET  /.well-known/oauth-authorization-server[/mcp]    OAuth AS metadata
GET  /.well-known/oauth-protected-resource[/mcp]      resource metadata (pointed at by the 401)
POST /mcp/oauth/register                              dynamic client registration
GET  /mcp/oauth/authorize                             sign-in page
POST /mcp/oauth/signin                                the form
POST /mcp/oauth/token                                 code (PKCE S256) and refresh grants
POST /mcp/oauth/revoke                                accepted, no-op, not advertised (see below)
POST /mcp                                             the MCP endpoint (Bearer required)
```

## Configuration

| Variable | Required | Purpose |
|---|---|---|
| `PUBLIC_URL` | yes | Where clients reach the server, e.g. `https://technogym.example.com`. Must be https unless localhost. |
| `SEAL_KEYS` | yes | Comma-separated base64 32-byte keys; `openssl rand -base64 32`. The first seals, every key unseals. |
| `PORT` | | Listen port, default 3000. Binds `0.0.0.0`. |
| `CLIENT_IP_HEADER` | | A header carrying the real client IP (e.g. `cf-connecting-ip`) for the per-IP limits. **Safe only when the server is reachable solely through the proxy that sets it**; otherwise anyone can forge it and every limit becomes global. |
| `RUST_LOG` | | Log level, default `info`. |

```sh
docker run --rm -p 3000:3000 \
  -e PUBLIC_URL=https://technogym.example.com \
  -e SEAL_KEYS="$(openssl rand -base64 32)" \
  ghcr.io/stephanmeijer/technogym-mcp
```

Put TLS termination in front (Caddy, Traefik, Cloudflare). Check:
`curl https://technogym.example.com/mcp/healthz` returns `ok`, and
`curl -i -X POST https://technogym.example.com/mcp` returns `401` with a `resource_metadata`
header.

## Operations

- **Revoke everyone:** replace `SEAL_KEYS` and restart. Every token stops unsealing and every
  user reconnects. There is no per-connection revocation: tokens are stateless.
- **Rotate the key without signing everyone out:** prepend a new key to `SEAL_KEYS` and restart.
  New tokens use the new key; old tokens still unseal under the old one until they expire or
  refresh (then they get the new key). Drop the old key later.
- **Logs** go to stderr, one line per request: a user pseudonym prefix (HMAC of the user id,
  never the id), RPC method, tool name, status and milliseconds. Never tokens, emails, passwords
  or payloads.

## Limits

Per IP: 5 failed sign-ins per 15 minutes, 60 authorize pages and 300 token requests per minute,
1000 registrations per hour. Across all IPs: 100 sign-ins per 15 minutes. Per user: 120 MCP
requests per minute. These protect the server IP's standing with mywellness, which sees every
user's login come from here.

## Known risks

Every user's mywellness traffic, logins included, leaves from one server IP. The client headers
mimic Technogym's own web app, so a change on their side breaks sign-in for everyone at once.

Codes are single-use, tracked in memory for the 60 s they live: a restart inside that window
forgets which were used, and several replicas would each keep their own list. A refresh token
lives 24 hours from the last refresh, so a connector left idle must sign in again. There is no
per-connection revocation (see Operations); add a grant store before hosting for strangers.
