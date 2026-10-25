-- Sources connected from the console (docs/26 §4): a layer over frood.yaml, one revision per
-- change; frood.yaml wins over it.
CREATE TABLE config_overlay (
    revision    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    data        JSONB NOT NULL,
    changed_by  TEXT NOT NULL,
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    note        TEXT
);

-- What the console stores secretly (tokens), sealed to the installation's vault key.
CREATE TABLE vault (
    name        TEXT PRIMARY KEY,
    sealed      BYTEA NOT NULL,
    updated_by  TEXT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
