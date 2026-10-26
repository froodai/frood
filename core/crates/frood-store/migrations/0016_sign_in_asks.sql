-- Sign-in through a person's channel (docs/28 §2): Frood sends a button to their linked
-- Telegram, and the browser that asked waits for the press.
CREATE TABLE sign_in_asks (
    id          UUID PRIMARY KEY,
    tx_hash     TEXT NOT NULL,                -- the sign-in request (auth_requests.id_hash) that asked
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    channel     TEXT NOT NULL,
    code        TEXT NOT NULL,                -- shown on the page and in the message, to compare
    state       TEXT NOT NULL DEFAULT 'asked' CHECK (state IN ('asked', 'approved', 'denied', 'used', 'expired')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
CREATE INDEX sign_in_asks_tx ON sign_in_asks (tx_hash);
CREATE INDEX sign_in_asks_user ON sign_in_asks (user_id, created_at);

-- The button is an Inbox item, so the channel shows it, takes the press and edits the message.
ALTER TABLE inbox_items DROP CONSTRAINT inbox_items_kind_check;
ALTER TABLE inbox_items ADD CONSTRAINT inbox_items_kind_check
    CHECK (kind IN ('proposal', 'question', 'reminder', 'alert', 'digest', 'sign_in'));
