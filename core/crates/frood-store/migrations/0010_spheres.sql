-- The sphere of life an assertion belongs to (docs/26 §3): a label for reading and filters.
ALTER TABLE assertion_versions ADD COLUMN sphere TEXT;
CREATE INDEX assertion_versions_sphere_idx ON assertion_versions (sphere) WHERE recorded_to IS NULL;
