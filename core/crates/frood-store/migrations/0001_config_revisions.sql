-- Applied revisions of frood.yaml (docs/16_platform.md §5).
-- `content` is the file as written: ${secret:...} references stay unresolved, so no secret
-- values ever reach the database.
CREATE TABLE config_revisions (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    sha256      TEXT NOT NULL CHECK (length(sha256) = 64),
    content     TEXT NOT NULL,
    source      TEXT NOT NULL CHECK (source IN ('file', 'git', 'api')),
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX config_revisions_applied_at_idx ON config_revisions (applied_at DESC);
