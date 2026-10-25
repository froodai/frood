-- Files can live outside the database (docs/26 §2): a folder or an S3-compatible storage, keyed
-- by their SHA-256. `data` is empty for those.
ALTER TABLE blobs ALTER COLUMN data DROP NOT NULL;
ALTER TABLE blobs ADD COLUMN stored TEXT NOT NULL DEFAULT 'database'
    CHECK (stored IN ('database', 'objects'));
ALTER TABLE blobs ADD CONSTRAINT blobs_data_where_stored
    CHECK ((stored = 'database') = (data IS NOT NULL));
