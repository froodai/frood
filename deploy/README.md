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

## End-to-end checks

- `core/crates/frood-api/tests/scenario.rs` — steps 1–5 of the MVP scenario through the real
  core and database, with fakes for Telegram, speech to text, the model and the agent.
- [`e2e/backup-restore.sh`](e2e/backup-restore.sh) — step 6 on Docker: a new installation backs
  up into a folder and an S3-compatible store, checks both restore, is deleted with its
  database and comes back with `frood restore`; the export before and after must match.
