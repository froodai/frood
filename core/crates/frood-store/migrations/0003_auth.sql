-- Sign-in and tokens (docs/12_api_mcp_auth.md §3). Every secret value (session, code, token)
-- is stored only as a SHA-256 hash.

CREATE TABLE users (
    id             UUID PRIMARY KEY,
    email          TEXT NOT NULL UNIQUE CHECK (email = lower(email)),
    display_name   TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at  TIMESTAMPTZ
);

-- Clients registered through Dynamic Client Registration (RFC 7591). Clients identified by a
-- Client ID Metadata Document are not stored: their document is fetched on demand.
CREATE TABLE oauth_clients (
    client_id      TEXT PRIMARY KEY,
    client_name    TEXT,
    redirect_uris  TEXT[] NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- An /oauth/authorize request waiting for sign-in and consent.
CREATE TABLE auth_requests (
    id_hash              TEXT PRIMARY KEY,
    client_id            TEXT NOT NULL,
    client_name          TEXT,
    redirect_uri         TEXT NOT NULL,
    code_challenge       TEXT NOT NULL,
    scopes               TEXT[] NOT NULL,
    resource             TEXT,
    state                TEXT,
    upstream_state_hash  TEXT UNIQUE,
    upstream_nonce       TEXT,
    upstream_verifier    TEXT,
    provider             TEXT,
    user_id              UUID REFERENCES users (id),
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at           TIMESTAMPTZ NOT NULL
);

CREATE TABLE auth_codes (
    code_hash       TEXT PRIMARY KEY,
    client_id       TEXT NOT NULL,
    redirect_uri    TEXT NOT NULL,
    code_challenge  TEXT NOT NULL,
    scopes          TEXT[] NOT NULL,
    resource        TEXT,
    user_id         UUID NOT NULL REFERENCES users (id),
    family          UUID NOT NULL,           -- tokens issued from this code share it
    expires_at      TIMESTAMPTZ NOT NULL,
    used_at         TIMESTAMPTZ
);

-- Browser sessions of the sign-in pages.
CREATE TABLE sessions (
    id_hash     TEXT PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE tokens (
    id            UUID PRIMARY KEY,
    token_hash    TEXT NOT NULL UNIQUE,
    kind          TEXT NOT NULL CHECK (kind IN ('access', 'refresh', 'personal')),
    user_id       UUID NOT NULL REFERENCES users (id),
    client_id     TEXT,
    scopes        TEXT[] NOT NULL,
    resource      TEXT,                      -- audience; NULL means any resource of this installation
    label         TEXT,                      -- name of a personal token
    family        UUID,                      -- refresh rotation family
    expires_at    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ,
    revoked_at    TIMESTAMPTZ
);
CREATE INDEX tokens_family_idx ON tokens (family) WHERE family IS NOT NULL;
CREATE INDEX tokens_user_idx ON tokens (user_id);
