# Deploy

The `compose` deploy target is built into the CLI: `frood apply` renders
`.frood/deploy/compose.yaml` next to your `frood.yaml` (PostgreSQL, and the server when
`deploy.server: container`) and runs `docker compose up`. Ports are bound to 127.0.0.1; public
HTTPS (Caddy) and admin access (Tailscale) come with later milestones.
