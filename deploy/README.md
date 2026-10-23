# Deploy

The `compose` deploy target is built into the CLI: `frood apply` renders
`.frood/deploy/compose.yaml` next to your `frood.yaml` (PostgreSQL, the server when
`deploy.server: container`, the modules) and runs `docker compose up`. Everything listens on
127.0.0.1 only.

## Installing a release

A release (tag `v<version>`) publishes images of the core, the modules and the agents for x86
and ARM as `ghcr.io/froodai/<name>:<version>`. On a server with Docker:

```sh
sh install.sh 0.1.0                       # puts `frood` into /usr/local/bin
frood init --dir /srv/frood --name team   # the server's image is the release's
```

Modules then use the release's images, e.g. `telegram: { image: ghcr.io/froodai/telegram:0.1.0 }`.

## Public HTTPS

Cloud clients (Claude, ChatGPT) reach Frood only at a public HTTPS address. Set it and apply:

```yaml
server:
  public_url: https://mem.example.com    # or, without a domain, https://203-0-113-7.sslip.io
```

`frood apply` then adds Caddy in front: it gets a certificate from Let's Encrypt by itself and is
the only thing listening beyond the machine (ports 80 and 443 — open them in the firewall). The
DNS name must point at the server before the first start. A sign-in provider other than `dev` is
needed (Google or another OpenID Connect provider, `auth.providers`).

### Sharing the address with other apps

Frood can live under a path, and the rest of the site can serve other things through the same
Caddy:

```yaml
server:
  public_url: https://203-0-113-7.sslip.io/frood
deploy:
  proxy:
    routes: /srv/pages/routes.caddy      # Caddy routes for the rest of the site
```

Frood then answers under `/frood` (the admin panel at `/frood/admin/`, MCP at `/frood/mcp`) and
at the root of the site only for OAuth discovery (`/.well-known/oauth-authorization-server` and
the `…/frood` forms of the RFC 8414 and RFC 9728 addresses). The routes file goes into the site
block as it is; write each app as a `handle` block, e.g.

```caddyfile
handle_path /pages/* {
	reverse_proxy 172.17.0.1:8790
}
```

Paths that no route claims answer 404. Caddy sees the routes file's folder at the same path, so
the file may import files next to it.
`frood apply` checks the routes with `caddy validate` before it changes anything and reloads
Caddy after an edit. A sign-in provider's redirect address follows the path:
`https://203-0-113-7.sslip.io/frood/oauth/callback/google`.

## End-to-end checks

- `core/crates/frood-api/tests/scenario.rs` — steps 1–5 of the MVP scenario through the real
  core and database, with fakes for Telegram, speech to text, the model and the agent.
- [`e2e/backup-restore.sh`](e2e/backup-restore.sh) — step 6 on Docker: a new installation backs
  up into a folder and an S3-compatible store, checks both restore, is deleted with its
  database and comes back with `frood restore`; the export before and after must match.
