# `backup.target@1`

Backups of an installation into storages the installation names (docs/16 §7). The core runs the
`backup` and `backup-verify` jobs on the schedules in `frood.yaml` and calls the module that gives
this capability; the module makes a consistent dump of the database, stores it with the files the
core hands over, and later checks that each storage can be restored from.

Everything that matters is in the database: knowledge with its history, the journal, files of
sources, config revisions, the Inbox. The core adds `frood.yaml` and the secrets file, already
encrypted to the installation's recovery key: the module never sees secrets in the clear, other
than the ones it needs to reach the database and the storages.

`POST /v1/capabilities/backup.target@1`

## `op: backup`

```json
{
  "op": "backup",
  "installation": "personal",
  "database": { "url": "postgres://frood:…@postgres:5432/frood" },
  "files": [
    { "name": "frood.yaml", "data_base64": "dmVyc2lvbjogMQo…" },
    { "name": "secrets.env.age", "data_base64": "YWdlLWVuY3J5cHRpb24ub3JnL3Yx…" }
  ],
  "about": { "frood_version": "0.0.1", "schema_version": 6 },
  "targets": [
    { "name": "disk", "repository": "/repos/disk", "password": "…", "env": {} },
    { "name": "b2", "repository": "s3:https://s3.eu-central-003.backblazeb2.com/frood-backups", "password": "…",
      "env": { "AWS_ACCESS_KEY_ID": "…", "AWS_SECRET_ACCESS_KEY": "…" } }
  ],
  "keep": { "daily": 30, "monthly": 12 }
}
```

- The dump is taken once and stored in every target. It must be consistent: one snapshot of the
  database, and the row count of every table in that same snapshot goes into `manifest.json`
  next to it, so a restore can be checked against it.
- A target's repository is created on first use. `keep` is applied after each backup (restic's
  `forget --prune` rules, by installation).
- One target failing does not stop the others.

Response:

```json
{
  "made_at": "2026-10-20T03:00:04Z",
  "dump_bytes": 18234112,
  "tables": 31,
  "targets": [
    { "name": "disk", "ok": true, "snapshot": "4f2a9c1e", "added_bytes": 1203311 },
    { "name": "b2", "ok": false, "error": "Fatal: unable to open config file: Stat: 403 Forbidden" }
  ]
}
```

## `op: verify`

```json
{ "op": "verify", "installation": "personal", "database": { "url": "…" }, "targets": [ … ] }
```

For each target: restore its latest backup of the installation, load the dump into a separate,
temporary database on the same server, compare every table's row count with `manifest.json`,
check the files are there, and drop the temporary database.

Response:

```json
{
  "targets": [
    { "name": "disk", "ok": true, "snapshot": "4f2a9c1e", "made_at": "2026-10-20T03:00:04Z", "tables": 31 },
    { "name": "b2", "ok": false, "snapshot": "77d01b3a", "error": "journal: 1201 rows, the backup says 1202" }
  ]
}
```

## Layout of a backup

A backup is one snapshot with these files; restoring needs nothing but the storage's password and
the recovery key:

| File | What |
|---|---|
| `db.dump` | `pg_dump --format=custom` of the database |
| `manifest.json` | `installation`, `made_at`, `frood_version`, `schema_version`, `counts` (rows per table) |
| `frood.yaml` | The config as written, with `${secret:…}` references |
| `secrets.env.age` | The secrets file, encrypted to the recovery key ([age](https://age-encryption.org)) |

Errors: 400 for a request the module cannot use (final); 5xx or `retryable: true` when the
database is unreachable. Per-target failures are not errors: they are in the response.
