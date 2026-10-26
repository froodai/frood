-- Passkeys (docs/28 §2): a person's fingerprint, Face ID or security key, kept as the public
-- key the browser registered, and the one-time links that add the first one.
CREATE TABLE passkeys (
    id             UUID PRIMARY KEY,
    user_id        UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    credential_id  TEXT NOT NULL UNIQUE,          -- base64url, as the browser names it
    passkey        JSONB NOT NULL,                -- public key and sign-in counter
    label          TEXT,                          -- the device, as the person named it
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at   TIMESTAMPTZ
);
CREATE INDEX passkeys_user ON passkeys (user_id);

CREATE TABLE passkey_links (
    code_hash   TEXT PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    used_at     TIMESTAMPTZ
);
