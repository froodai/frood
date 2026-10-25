-- What a channel module says about itself for the console: the address that opens the channel
-- with a link code appended (Telegram: https://t.me/<bot>?start=).
CREATE TABLE channel_about (
    channel     TEXT PRIMARY KEY,
    start_url   TEXT,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
