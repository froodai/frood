-- M5: Inbox items, what was shown where, and who has seen what. docs/22_m5_plan.md.

-- A situation that needs a person: one row per situation, kept until it is resolved.
CREATE TABLE inbox_items (
    id             UUID PRIMARY KEY,
    kind           TEXT NOT NULL CHECK (kind IN ('proposal', 'question', 'reminder', 'alert', 'digest')),
    subject        TEXT NOT NULL UNIQUE,         -- the situation, e.g. proposal/<id>, question/<id>, due/<assertion>/<date>
    state          TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'snoozed', 'resolved')),
    urgency        TEXT NOT NULL CHECK (urgency IN ('now', 'digest', 'silent')),
    -- What the item is about: proposal_id, question_id, run_id, assertion_id, … and why it exists.
    data           JSONB NOT NULL DEFAULT '{}',
    for_users      UUID[] NOT NULL DEFAULT '{}', -- the people the situation is for; empty: nobody in particular
    snoozed_until  TIMESTAMPTZ,
    -- Notify after this moment (investigation before notifying, reminder time).
    notify_after   TIMESTAMPTZ NOT NULL DEFAULT now(),
    investigation  JSONB,                        -- {run_id, state, report} of the investigate run, if any
    resolution     JSONB,                        -- {action, by, at}
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at    TIMESTAMPTZ
);
CREATE INDEX inbox_items_open_idx ON inbox_items (notify_after) WHERE state <> 'resolved';

-- An item shown to a person in a channel: the message, so it can be edited when resolved.
CREATE TABLE inbox_deliveries (
    item_id      UUID NOT NULL REFERENCES inbox_items (id),
    user_id      UUID NOT NULL REFERENCES users (id),
    channel      TEXT NOT NULL,                  -- the module, e.g. telegram
    state        TEXT NOT NULL CHECK (state IN ('sent', 'failed', 'closed')),
    message_ref  TEXT,
    attempts     INT NOT NULL DEFAULT 0,
    last_error   TEXT,
    -- Reminders repeat: the next showing, if any.
    repeat_at    TIMESTAMPTZ,
    sent_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (item_id, user_id, channel)
);
CREATE INDEX inbox_deliveries_user_day_idx ON inbox_deliveries (user_id, sent_at);

-- Per person: items seen, and "seen everything up to" for the digest.
CREATE TABLE inbox_seen (
    item_id   UUID NOT NULL REFERENCES inbox_items (id),
    user_id   UUID NOT NULL REFERENCES users (id),
    seen_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (item_id, user_id)
);
CREATE TABLE attention_checkpoints (
    user_id     UUID PRIMARY KEY REFERENCES users (id),
    seen_until  TIMESTAMPTZ NOT NULL
);
