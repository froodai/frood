# Frood

Open, self-hosted memory and proactive-assistant platform. A small core keeps a canonical,
bitemporal knowledge ledger (assertions, their sources, proposed changes and human verdicts);
everything else — storage, search, agent runtimes, speech recognition, sources, channels — is a
replaceable module.

Status: early development, milestone **M0** (repository skeleton). Design notes (in Russian) live
in [`docs/`](docs/README.md); the implementation plan is [`docs/17_mvp_plan.md`](docs/17_mvp_plan.md).

## Layout

| Path | What |
|---|---|
| `core/` | Rust workspace; builds one binary, `frood` (server + CLI) |
| `contracts/` | Versioned capability contracts and conformance tests |
| `modules/` | Official out-of-process modules |
| `deploy/` | Deployment notes and templates |
| `docs/` | Design documents and review page |

## Install on your computer

Needs Docker (Docker Desktop on a Mac).

```bash
curl -fsSL https://raw.githubusercontent.com/froodai/frood/main/deploy/install.sh | sh -s 0.1.9
frood init --dir ~/frood
```

`frood init` asks a few questions — your email, a model key to read notes with, a Telegram bot,
a folder for backups, when the morning digest comes — each with an answer by default, then
starts Frood and prints the lines that connect Claude Code and Codex. On this computer you sign
in to the console without a password; a server with its own address uses Google or another
OIDC provider (`frood init --public-url https://…`).

## Quick start (development)

Requires Rust 1.99+ and Docker.

```bash
cd core && cargo build
./target/debug/frood init --dir /tmp/frood-dev --server host
./target/debug/frood --config /tmp/frood-dev/frood.yaml apply
./target/debug/frood --config /tmp/frood-dev/frood.yaml serve
```

`apply` starts PostgreSQL in Docker, runs migrations and records the applied config revision.
With `--server host` the server runs on your machine; `--server container` runs it in Docker too.

## License

The core (`core/`) is licensed under the GNU Affero General Public License v3.0
(`AGPL-3.0-only`). Contracts, SDKs and official modules are licensed under Apache-2.0
so that third-party modules can use any license. Contributions require a CLA.
