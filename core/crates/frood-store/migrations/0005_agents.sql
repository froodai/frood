-- M4: agent runs that wait for answers, their questions and steps, run tokens, schedules and
-- source checkpoints. docs/21_m4_plan.md.

ALTER TABLE runs DROP CONSTRAINT runs_status_check;
ALTER TABLE runs ADD CONSTRAINT runs_status_check
    CHECK (status IN ('queued', 'running', 'waiting_for_input', 'succeeded', 'failed', 'cancelled'));
ALTER TABLE runs ADD COLUMN on_behalf_of UUID REFERENCES users (id);
ALTER TABLE runs ADD COLUMN runtime TEXT;            -- module running the agent
ALTER TABLE runs ADD COLUMN session_ref TEXT;        -- runtime session, to resume it
ALTER TABLE runs ADD COLUMN cost_usd NUMERIC(12, 6) NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN pending_messages JSONB NOT NULL DEFAULT '[]';  -- delivered on resume
CREATE INDEX runs_active_idx ON runs (status) WHERE status IN ('running', 'waiting_for_input');

-- What happened inside a run: progress notes, tool calls, model usage, the runtime's events.
CREATE TABLE run_events (
    run_id  UUID NOT NULL REFERENCES runs (id),
    seq     BIGINT GENERATED ALWAYS AS IDENTITY,
    kind    TEXT NOT NULL,
    data    JSONB NOT NULL DEFAULT '{}',
    at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (run_id, seq)
);

-- A question an agent asks a person; the answer resumes the run.
CREATE TABLE questions (
    id              UUID PRIMARY KEY,
    run_id          UUID NOT NULL REFERENCES runs (id),
    job             TEXT NOT NULL,
    asked_for       UUID REFERENCES users (id),   -- the person the run acts for, if any
    text            TEXT NOT NULL,
    options         JSONB NOT NULL DEFAULT '[]',
    context         TEXT,
    status          TEXT NOT NULL CHECK (status IN ('open', 'answered', 'expired', 'cancelled')),
    answer          TEXT,
    answered_by     TEXT,                          -- actor
    answered_at     TIMESTAMPTZ,
    expires_at      TIMESTAMPTZ,
    on_expire       TEXT NOT NULL DEFAULT 'skip' CHECK (on_expire IN ('skip', 'cancel', 'default_answer')),
    default_answer  TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX questions_open_idx ON questions (created_at) WHERE status = 'open';
CREATE INDEX questions_run_idx ON questions (run_id);

-- Run tokens: an agent's credentials for one run.
ALTER TABLE tokens DROP CONSTRAINT tokens_kind_check;
ALTER TABLE tokens ADD CONSTRAINT tokens_kind_check
    CHECK (kind IN ('access', 'refresh', 'personal', 'service', 'run'));
ALTER TABLE tokens ADD COLUMN run_id UUID REFERENCES runs (id);
ALTER TABLE tokens DROP CONSTRAINT tokens_owner_check;
ALTER TABLE tokens ADD CONSTRAINT tokens_owner_check CHECK (
    CASE kind
        WHEN 'service' THEN module IS NOT NULL AND user_id IS NULL AND run_id IS NULL
        WHEN 'run' THEN run_id IS NOT NULL AND module IS NULL
        ELSE module IS NULL AND run_id IS NULL AND user_id IS NOT NULL
    END
);

-- When each scheduled job (and each item it runs for) last fired.
CREATE TABLE schedule_state (
    job        TEXT NOT NULL,
    item       TEXT NOT NULL DEFAULT '',            -- e.g. the source of `for_each: source`
    last_tick  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (job, item)
);

-- How far each source has been read, for "what changed since last time".
CREATE TABLE source_checkpoints (
    source      TEXT PRIMARY KEY,
    checkpoint  JSONB NOT NULL DEFAULT '{}',
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
