# Frood on Kubernetes

The chart in [`frood/`](frood) installs Frood into one namespace of a cluster where pods run
without root, privileges or Docker, next to a managed Postgres:

- **the server**: one Deployment behind a Service and an Ingress, with `frood.yaml` in a ConfigMap;
- **agents** ([runtime-k8s](../../modules/runtime-k8s)): a Deployment that runs each turn of an
  agent job as a Kubernetes Job, with its own ServiceAccount, Role and RoleBinding;
- optionally **Telegram** and **backups** ([telegram](../../modules/telegram),
  [backup-restic](../../modules/backup-restic));
- optionally **NetworkPolicies** for all of it.

There is no Caddy and no PostgreSQL in the chart: the cluster's ingress controller does TLS, the
platform runs the database. Every pod runs as user 10001 with no privilege escalation, no
capabilities, a read-only root filesystem and the `RuntimeDefault` seccomp profile, so the
namespace may enforce the `restricted` Pod Security Standard.

## 1. The database

A managed Postgres with three extensions Frood's migrations create:
`vector`, `pg_trgm` and `btree_gist`. On a platform where the database user may not create
extensions, ask its administrators to create them in Frood's database first. The backup module's
restore check also needs `CREATEDB` (it restores into a temporary database next to Frood's).

Put the whole URL into a Secret:

```sh
kubectl -n frood create secret generic frood-db \
  --from-literal=url='postgres://frood:…@db.example.internal:5432/frood?sslmode=require'
```

## 2. Secrets

Everything `frood.yaml` names as `${secret:NAME}` is read from the server's environment. Put the
values into one Secret and name it in `secrets.existingSecret`; the server gets all its keys.

```sh
age-keygen | grep AGE-SECRET-KEY > vault.key   # seals the tokens the console stores
kubectl -n frood create secret generic frood-secrets \
  --from-file=FROOD_VAULT_KEY=vault.key \
  --from-literal=GOOGLE_CLIENT_SECRET=… \
  --from-literal=ANTHROPIC_API_KEY=… \
  --from-literal=TELEGRAM_BOT_TOKEN=…
```

Service tokens between the server and its modules are generated on install into
`<release>-module-tokens` and kept on upgrades and uninstall (or bring your own Secret with
`FROOD_MODULE_TOKEN_<MODULE>` keys in `moduleTokens.existingSecret`).

## 3. Files

`files.type` says where photos, voice messages and documents go:

- `pvc` (default): a PersistentVolumeClaim, `ReadWriteOnce`. The server then runs as one replica
  and is replaced with the `Recreate` strategy; the claim is kept when the release is removed.
- `s3`: a bucket. Set `config.storage.objects` to
  `{ uses: s3, endpoint, bucket, access_key_id: '${secret:…}', secret_access_key: '${secret:…}' }`.
  Backups do not copy the bucket: keep it safe with the storage's own versioning.
- `database`: in Postgres, for small installations.

## 4. The address and sign-in

By default the Ingress is public, so Claude and ChatGPT (their connectors and phone apps), Claude
Code and the Telegram Mini App reach Frood without a VPN. Frood's own sign-in decides who gets
in: with Google, `auth.allow_domains: [example.com]` lets in every work account of the domain,
`auth.allow: [me@example.com]` only the people listed.

```yaml
ingress:
  className: nginx            # the cluster's public ingress class
  host: frood.example.com
  tls:
    clusterIssuer: letsencrypt  # cert-manager makes the certificate; or set tls.secretName
config:
  auth:
    allow_domains: [example.com]
    providers:
      - { kind: oidc, id: google, label: Google, issuer: 'https://accounts.google.com',
          client_id: 1234-abc.apps.googleusercontent.com, client_secret: '${secret:GOOGLE_CLIENT_SECRET}' }
```

The Google client's redirect URI is `https://frood.example.com/oauth/callback/google`.
`server.public_url` follows from `ingress.host` (`server.publicUrl` overrides it).

For a Frood only the company network reaches, use an internal ingress class and issuer
([examples/internal.yaml](frood/examples/internal.yaml)); cloud clients then cannot reach it.

### A sign-in proxy in front (optional)

Frood needs none, but a cluster may want every public site behind its own sign-in proxy
(oauth2-proxy and the like). With ingress-nginx:

```yaml
ingress:
  externalAuth:
    enabled: true
    url: 'https://oauth2.example.com/oauth2/auth'
    signin: 'https://oauth2.example.com/oauth2/start?rd=$scheme://$host$escaped_request_uri'
```

The chart then makes two Ingresses for the host: the main one behind the proxy, and one for the
paths in `externalAuth.openPaths` without it: `/mcp`, `/oauth`, `/.well-known` and `/v1`.
**Keep those paths open.** MCP clients (Claude, ChatGPT, Claude Code), the OAuth flow they sign in
with, and scripts on the `/v1` API carry Frood's tokens, not a browser session: behind a browser
sign-in proxy they get its login page instead of an answer, and connecting fails. Frood checks the
tokens on those paths itself. A future Telegram webhook path belongs in the list too.

## 5. Agents

With `agents.enabled` (the default), jobs with `runtime: agents` run their agent (Claude Code;
Codex when `agents.agentImages.codex` is set) as Jobs in the release's namespace, or in
`agents.jobNamespace`, where the chart puts the Role. The module may only create, read, let go
and delete Jobs (also all Jobs of a run at once, at its end), list pods and read their logs,
create and delete PersistentVolumeClaims, and create Secrets; nothing cluster-wide.

A turn's pod:

- runs the agent's image as user 10001, with no API token, `requests` equal to the limits
  (`agents.config.cpus`, `memory_mb`), and a read-only root;
- gets its command line and credentials from a Secret its Job owns, not from the Job;
- keeps the run's sessions on a claim of `session_mb` (the cluster's default StorageClass unless
  `storage_class`), deleted when the run ends;
- reaches Frood at its Service's full name, e.g. `http://frood.frood.svc:8080`, which the chart
  passes as `frood_url`;
- ends at the run's `budget.max_minutes` plus a minute even if nobody stops it.

```sh
kubectl -n frood get jobs -l app.kubernetes.io/managed-by=frood-runtime-k8s
```

## 6. Models through a gateway

An account may send its calls to another endpoint with the provider's API, such as a company
LLM gateway: `base_url` is used by the server's own calls and by agents alike
(`ANTHROPIC_BASE_URL` for Claude Code). The key is a `secret`, or the output of `api_key_helper`,
a command for gateways that hand out short-lived keys: the server runs it with `sh -c` and again
after five minutes or a 401, and agents get it as Claude Code's `apiKeyHelper`, so the command
must work inside both the server's and the agent's images. Codex takes only a `secret`.

A gateway proxy that holds the company's credentials itself runs best next to Frood: a Deployment
and a Service in the namespace (`extraObjects`, see
[examples/llm-gateway.yaml](frood/examples/llm-gateway.yaml), a generic sketch, not a real
product), with `base_url` naming its Service with the namespace, e.g.
`http://llm-gateway.frood.svc:4000/anthropic`. Plain `http://` is accepted for such in-cluster
names (`*.svc`, `*.svc.cluster.local`, dotless names) and loopback only.

## 7. Telegram and backups

- `telegram.enabled`: the bot, with `TELEGRAM_BOT_TOKEN` from `secrets.existingSecret` (or
  `telegram.botToken`). It long-polls Telegram, so it needs no address of its own, but a bot can
  serve only one installation: give a work Frood its own bot.
- `backup.enabled`: backups into restic repositories, configured in `config.backup` (see
  [modules/README.md](../../modules/README.md#backups)). With a PVC for files it runs on the
  server's node and reads the files read-only. A managed Postgres has its own backups; this one is
  for a copy elsewhere.

## 8. NetworkPolicies

`networkPolicy.enabled` limits who talks to whom: the server is reached on its port from
`networkPolicy.ingressFrom` (the ingress controller's namespace) and from its own pods; modules
only from the server; agent pods from nobody. Egress goes to the cluster DNS, the internet
(`networkPolicy.internet`, private ranges excluded), the private addresses in
`networkPolicy.privateEgress` (the managed Postgres, a company gateway), and the release's own
pods. The agents module also reaches the API server: list its endpoints in
`networkPolicy.apiServer`:

```sh
kubectl get endpoints kubernetes -n default -o jsonpath='{.subsets[*].addresses[*].ip}:{.subsets[*].ports[*].port}'
```

Whether a policy can name the API server by its endpoint address depends on the CNI; check that
agent jobs start after turning the policies on.

## 9. Installing

```sh
helm install frood deploy/helm/frood -n frood -f my-values.yaml
kubectl -n frood rollout status deploy/frood
```

[examples/public.yaml](frood/examples/public.yaml) is a complete work installation: a public
address with Google sign-in, files in S3, agents, Telegram and NetworkPolicies. The server waits
for the database and runs its migrations before it answers `/readyz`. A personal token for a
script or for Claude Code:

```sh
kubectl -n frood exec deploy/frood -- frood --config /etc/frood/frood.yaml \
  token create --email me@example.com --label laptop
```

Upgrades: `helm upgrade` with the new chart; a change of `config` rolls the server, which applies
new migrations on start.
