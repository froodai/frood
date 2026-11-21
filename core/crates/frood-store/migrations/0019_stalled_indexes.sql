-- "Frood stopped learning" (docs/33 S1) asks on every `/v1/now` whether anything new came in
-- since a moment and how the reading jobs ran since then: without these, each ask reads the
-- whole of proposals, assertion_versions and runs.
CREATE INDEX proposals_created_idx ON proposals (created_at);
CREATE INDEX assertion_versions_recorded_from_idx ON assertion_versions (recorded_from);
CREATE INDEX runs_job_created_idx ON runs (job, created_at);
