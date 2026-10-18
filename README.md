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
| `bundles/` | Config-only bundles: jobs, instructions, rules |
| `deploy/` | Deployment notes and templates |
| `docs/` | Design documents and review page |

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
(`AGPL-3.0-only`). Contracts, SDKs, official modules and bundles are licensed under Apache-2.0
so that third-party modules can use any license. Contributions require a CLA.
