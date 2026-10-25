-- Sign-ins to MCP servers that wait for the person to come back (docs/26 §4): what is needed
-- to finish them, sealed; they last half an hour.
CREATE TABLE oauth_pending (
    state       TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    sealed      BYTEA NOT NULL,
    settings    JSONB NOT NULL,
    started_by  TEXT NOT NULL,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
