-- Files and links kept with knowledge (docs/26 §2). Each is a source of its own, attached to an
-- assertion across its versions, or to a proposal while it waits.
CREATE TABLE attachments (
    id            UUID PRIMARY KEY,
    assertion_id  UUID REFERENCES assertions (id),
    proposal_id   UUID REFERENCES proposals (id),
    source_id     UUID NOT NULL REFERENCES sources (id),
    added_by      TEXT NOT NULL,
    added_at      TIMESTAMPTZ NOT NULL,
    removed_at    TIMESTAMPTZ,
    CHECK ((assertion_id IS NULL) <> (proposal_id IS NULL))
);
CREATE UNIQUE INDEX attachments_once
    ON attachments ((coalesce(assertion_id, proposal_id)), source_id) WHERE removed_at IS NULL;
CREATE INDEX attachments_of_assertion ON attachments (assertion_id) WHERE removed_at IS NULL;
CREATE INDEX attachments_of_proposal ON attachments (proposal_id) WHERE removed_at IS NULL;
