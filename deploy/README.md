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

### A tunnel instead of open ports

A laptop or a home server has no address on the internet, yet Claude in the browser and ChatGPT
call Frood from their clouds. A tunnel gives it a lasting HTTPS address without a domain of one's
own or a port opened on the router:

```yaml
server:
  public_url: https://frood.tail1234.ts.net   # the tunnel's address
deploy:
  tunnel: { uses: tailscale, auth_key: '${secret:TAILSCALE_AUTH_KEY}' }
```

- **Tailscale Funnel** (free plans too): an auth key from the admin console (Settings → Keys,
  not ephemeral). The machine is named after the first label of `public_url`; the tailnet's name
  is on the DNS page. Funnel needs HTTPS certificates on for the tailnet and the `funnel` attribute
  in its access controls (new tailnets have it).
- **Cloudflare Tunnel** for a domain in Cloudflare: `tunnel: { uses: cloudflare, token: … }` with
  the token of a named tunnel (Zero Trust → Networks → Tunnels); its public hostname is
  `public_url`'s host, with the service `http://127.0.0.1:8080`. Quick tunnels
  (`trycloudflare.com`) are not offered: their address changes on every start, and it is the
  OAuth issuer, so connected clients would break.

The tunnel runs next to the server in its network and reaches it on 127.0.0.1; TLS ends at the
tunnel, so Frood runs no Caddy and needs no ports. Frood answers at the root of the address. The
secret goes to `.frood/deploy/tunnel.env` (readable only by the owner), not into the compose file.
With a public address, sign-in on this computer is off: sign in with a button in Telegram or a
passkey. `frood init` asks about it when Frood runs on this computer.

### Behind the server's own proxy

When several apps share the server and none of them should own its HTTPS front, the server runs
a proxy of its own and Frood is one of the apps behind it:

```yaml
server:
  public_url: https://203-0-113-7.sslip.io/frood
deploy:
  proxy:
    external: /srv/edge/paths/frood.caddy   # Frood's file in that proxy
```

Frood then runs no Caddy and leaves ports 80 and 443 to the proxy; `frood apply` removes a
Caddy left from an earlier config. It writes only Frood's routes to the file, whole and only when
they change, reaching the server on `127.0.0.1:<port>`:

- with a path in `public_url`, the routes above without a site block and without the 404 for the
  rest of the site; the proxy imports the file into its site block for that host;
- with a domain of its own, a site block for it.

```caddyfile
203-0-113-7.sslip.io {
	import /srv/edge/paths/*.caddy
	handle {
		respond 404
	}
}
```

The proxy picks the file up itself: `caddy --watch` sees only its main file, not imported ones,
so watch the folder (a systemd path unit running `caddy reload`, for example).
`deploy.proxy.routes` does not go with `external`: other apps keep their own files in the proxy.

## End-to-end checks

- `core/crates/frood-api/tests/scenario.rs` — steps 1–5 of the MVP scenario through the real
  core and database, with fakes for Telegram, speech to text, the model and the agent.
- [`e2e/backup-restore.sh`](e2e/backup-restore.sh) — step 6 on Docker: a new installation backs
  up into a folder and an S3-compatible store, checks both restore, is deleted with its
  database and comes back with `frood restore`; the export before and after must match.
