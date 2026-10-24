-- A short title over the canonical text (docs/26 §1); older versions have none.
ALTER TABLE assertion_versions ADD COLUMN title TEXT;
