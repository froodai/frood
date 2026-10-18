-- M3: files sent with captures, the run queue of the dispatcher, channel links, module
-- service tokens, and search (full text + embeddings). docs/19_m3_plan.md.

CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Files that came with a capture: voice messages, audio, documents.
CREATE TABLE blobs (
    id            UUID PRIMARY KEY,
    content_type  TEXT NOT NULL,
    filename      TEXT,
    size_bytes    INT NOT NULL CHECK (size_bytes >= 0),
    sha256        TEXT NOT NULL CHECK (length(sha256) = 64),
    data          BYTEA NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE sources ADD COLUMN blob_id UUID REFERENCES blobs (id);

-- One run of a job. A run is created once per (job, event) through its idempotency key.
CREATE TABLE runs (
    id               UUID PRIMARY KEY,
    job              TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL UNIQUE,
    subject          TEXT,                    -- what it works on, e.g. source/<id>
    event_position   BIGINT REFERENCES journal (position),
    input            JSONB NOT NULL DEFAULT '{}',
    status           TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts         INT NOT NULL DEFAULT 0,
    run_after        TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at       TIMESTAMPTZ,
    finished_at      TIMESTAMPTZ,
    output           JSONB,
    error            TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX runs_ready_idx ON runs (run_after) WHERE status = 'queued';
CREATE INDEX runs_subject_idx ON runs (subject);

-- How far each reader of the journal has got.
CREATE TABLE journal_checkpoints (
    reader      TEXT PRIMARY KEY,
    position    BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A person's account in a channel (Telegram, …). Actions from the channel act as this user.
CREATE TABLE channel_links (
    channel      TEXT NOT NULL,
    external_id  TEXT NOT NULL,
    user_id      UUID NOT NULL REFERENCES users (id),
    label        TEXT,                        -- e.g. the Telegram username, for display
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel, external_id)
);

-- One-time codes that link a channel account ("send the bot /link K7M2QX9P").
CREATE TABLE link_codes (
    code_hash   TEXT PRIMARY KEY,
    channel     TEXT NOT NULL,
    user_id     UUID NOT NULL REFERENCES users (id),
    expires_at  TIMESTAMPTZ NOT NULL,
    used_at     TIMESTAMPTZ
);

-- Service tokens of modules: no user, the module named instead.
ALTER TABLE tokens DROP CONSTRAINT tokens_kind_check;
ALTER TABLE tokens ADD CONSTRAINT tokens_kind_check
    CHECK (kind IN ('access', 'refresh', 'personal', 'service'));
ALTER TABLE tokens ALTER COLUMN user_id DROP NOT NULL;
ALTER TABLE tokens ADD COLUMN module TEXT;
ALTER TABLE tokens ADD CONSTRAINT tokens_owner_check
    CHECK (CASE WHEN kind = 'service' THEN module IS NOT NULL AND user_id IS NULL
                ELSE module IS NULL AND user_id IS NOT NULL END);
CREATE UNIQUE INDEX tokens_module_idx ON tokens (module) WHERE kind = 'service' AND revoked_at IS NULL;

-- Search: words over current knowledge, and embeddings per assertion version and model.
CREATE INDEX assertion_versions_text_idx ON assertion_versions
    USING gin (to_tsvector('simple', text)) WHERE recorded_to IS NULL;
CREATE TABLE assertion_embeddings (
    assertion_id  UUID NOT NULL,
    version       INT NOT NULL,
    model         TEXT NOT NULL,
    embedding     vector NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (assertion_id, version, model),
    FOREIGN KEY (assertion_id, version) REFERENCES assertion_versions (assertion_id, version)
);
CREATE INDEX entities_name_trgm_idx ON entities USING gin (lower(name) gin_trgm_ops);
