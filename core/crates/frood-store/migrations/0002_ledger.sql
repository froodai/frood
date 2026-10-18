-- Knowledge Ledger (docs/11_knowledge_model.md v0.2).
--
-- Assertion versions are bitemporal rows: `valid_*` is when the statement holds in the world,
-- `recorded_*` is when the ledger held that belief. Rows are immutable except that
-- `recorded_to` is set once, when a later belief replaces the row. "Current knowledge" is every
-- row with `recorded_to IS NULL`; for one assertion those rows never overlap in valid time.

CREATE EXTENSION IF NOT EXISTS btree_gist;

-- Where information came from: a note, a voice message, a chat message, a document.
CREATE TABLE sources (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL,
    origin          TEXT NOT NULL,           -- 'capture' or 'source:<name>'
    uri             TEXT,                    -- stable link in the origin system
    title           TEXT,
    content         TEXT,                    -- kept for captures; external sources keep excerpts only
    content_sha256  TEXT NOT NULL CHECK (length(content_sha256) = 64),
    said_at         TIMESTAMPTZ,             -- when the content was produced
    captured_by     TEXT NOT NULL,           -- actor
    captured_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    metadata        JSONB NOT NULL DEFAULT '{}',
    UNIQUE (origin, content_sha256)
);

-- An exact place inside a source.
CREATE TABLE evidence (
    id          UUID PRIMARY KEY,
    source_id   UUID NOT NULL REFERENCES sources (id),
    anchor      JSONB NOT NULL,              -- {"type":"whole"} | {"type":"time","from":..} | ...
    quote       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX evidence_source_idx ON evidence (source_id);

-- Something a source says, not yet knowledge.
CREATE TABLE observations (
    id            UUID PRIMARY KEY,
    source_id     UUID NOT NULL REFERENCES sources (id),
    evidence_id   UUID REFERENCES evidence (id),
    text          TEXT NOT NULL,
    modality      TEXT NOT NULL,
    said_at       TIMESTAMPTZ,
    extracted_by  TEXT NOT NULL,
    confidence    REAL CHECK (confidence BETWEEN 0 AND 1),
    flags         TEXT[] NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX observations_source_idx ON observations (source_id);

-- What knowledge is about: people, projects, components, topics.
CREATE TABLE entities (
    id           UUID PRIMARY KEY,
    type         TEXT NOT NULL,
    name         TEXT NOT NULL,
    aliases      TEXT[] NOT NULL DEFAULT '{}',
    merged_into  UUID REFERENCES entities (id),
    created_by   TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (merged_into IS DISTINCT FROM id)
);
CREATE INDEX entities_name_idx ON entities (lower(name));

-- Proposed changes to knowledge, like pull requests.
CREATE TABLE proposals (
    id               UUID PRIMARY KEY,
    idempotency_key  TEXT UNIQUE,
    author           TEXT NOT NULL,
    rationale        TEXT,
    risk             TEXT NOT NULL CHECK (risk IN ('new', 'change', 'conflict', 'stale_knowledge', 'entity_merge')),
    confidence       REAL CHECK (confidence BETWEEN 0 AND 1),
    base             JSONB NOT NULL DEFAULT '[]',  -- [{"assertion_id":..,"version":..}]
    alternatives     JSONB NOT NULL,               -- [{"key":..,"label":..,"operations":[..]}]
    evidence         UUID[] NOT NULL DEFAULT '{}',
    state            TEXT NOT NULL CHECK (state IN ('open', 'accepted', 'rejected', 'deferred', 'stale')),
    deferred_until   TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at       TIMESTAMPTZ
);
CREATE INDEX proposals_state_idx ON proposals (state, created_at);

CREATE TABLE verdicts (
    id           UUID PRIMARY KEY,
    proposal_id  UUID NOT NULL REFERENCES proposals (id),
    decision     TEXT NOT NULL CHECK (decision IN ('accept', 'accept_with_edits', 'reject', 'defer', 'investigate')),
    alternative  TEXT,
    edits        JSONB,
    reason       TEXT,
    decided_by   TEXT NOT NULL,                    -- 'user:..' or 'policy:..'
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX verdicts_proposal_idx ON verdicts (proposal_id);

-- Stable identity of an assertion; its content lives in versions.
CREATE TABLE assertions (
    id          UUID PRIMARY KEY,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE assertion_versions (
    assertion_id   UUID NOT NULL REFERENCES assertions (id),
    version        INT NOT NULL CHECK (version > 0),
    kind           TEXT NOT NULL,
    text           TEXT NOT NULL,
    subjects       UUID[] NOT NULL DEFAULT '{}',
    slots          JSONB NOT NULL DEFAULT '{}',
    modality       TEXT NOT NULL,
    authority      JSONB NOT NULL DEFAULT '[]',
    said_at        TIMESTAMPTZ,
    valid_from     TIMESTAMPTZ NOT NULL,
    valid_to       TIMESTAMPTZ,
    recorded_from  TIMESTAMPTZ NOT NULL,
    recorded_to    TIMESTAMPTZ,
    status         TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'completed', 'historical', 'retired')),
    review_after   TIMESTAMPTZ,
    evidence       UUID[] NOT NULL DEFAULT '{}',
    proposal_id    UUID NOT NULL REFERENCES proposals (id),
    verdict_id     UUID NOT NULL REFERENCES verdicts (id),
    PRIMARY KEY (assertion_id, version),
    CHECK (valid_to IS NULL OR valid_to > valid_from),
    CHECK (recorded_to IS NULL OR recorded_to >= recorded_from),
    -- Current knowledge about one assertion never overlaps in valid time.
    EXCLUDE USING gist (
        assertion_id WITH =,
        tstzrange(valid_from, valid_to) WITH &&
    ) WHERE (recorded_to IS NULL)
);
CREATE INDEX assertion_versions_current_idx ON assertion_versions (kind, status) WHERE recorded_to IS NULL;
CREATE INDEX assertion_versions_subjects_idx ON assertion_versions USING gin (subjects);
CREATE INDEX assertion_versions_slots_idx ON assertion_versions USING gin (slots jsonb_path_ops);

-- Links between assertions (contradicts, depends_on, part_of, derived_from, relates_to).
CREATE TABLE assertion_links (
    id                  UUID PRIMARY KEY,
    from_assertion      UUID NOT NULL REFERENCES assertions (id),
    to_assertion        UUID NOT NULL REFERENCES assertions (id),
    kind                TEXT NOT NULL CHECK (kind IN ('contradicts', 'depends_on', 'part_of', 'derived_from', 'relates_to')),
    recorded_from       TIMESTAMPTZ NOT NULL,
    recorded_to         TIMESTAMPTZ,
    created_by_verdict  UUID NOT NULL REFERENCES verdicts (id),
    CHECK (from_assertion <> to_assertion)
);
CREATE UNIQUE INDEX assertion_links_current_idx
    ON assertion_links (from_assertion, to_assertion, kind) WHERE recorded_to IS NULL;

-- Operational markers on top of status: 'contested', 'needs_verification'. Not versioned.
CREATE TABLE assertion_flags (
    assertion_id  UUID NOT NULL REFERENCES assertions (id),
    flag          TEXT NOT NULL CHECK (flag IN ('contested', 'needs_verification')),
    reason        TEXT,
    set_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (assertion_id, flag)
);

-- Every state change, in commit order. Writers take a transaction-level advisory lock before
-- appending, so positions become visible in order and readers never skip a gap.
CREATE TABLE journal (
    position     BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    id           UUID NOT NULL UNIQUE,
    type         TEXT NOT NULL,
    subject      TEXT,
    actor        TEXT NOT NULL,
    data         JSONB NOT NULL DEFAULT '{}',
    occurred_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
