-- docs/25 §4: a person takes a note back. It stays in history, marked; what came from it is
-- undone, and sending the same note again brings it back.
ALTER TABLE sources ADD COLUMN withdrawn_at TIMESTAMPTZ, ADD COLUMN withdrawn_by TEXT;
