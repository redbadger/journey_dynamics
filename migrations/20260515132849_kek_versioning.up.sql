-- Track which KEK version wrapped each DEK so multiple versions can coexist
-- during a rotation.
--
-- The DEFAULT 'legacy:v1' backfills any pre-existing rows in the same
-- statement; we then DROP the default so new INSERTs must always supply kek_id
-- explicitly (a missing kek_id is a programming error, not a data default).
--
-- rewrapped_at is observability sugar — updated whenever a DEK is re-wrapped
-- under a newer KEK version; not load-bearing.

ALTER TABLE subject_encryption_keys
    ADD COLUMN kek_id       TEXT      NOT NULL DEFAULT 'legacy:v1',
    ADD COLUMN rewrapped_at TIMESTAMP;

ALTER TABLE subject_encryption_keys
    ALTER COLUMN kek_id DROP DEFAULT;

-- Lets the background re-wrap sweeper find stale rows efficiently.
CREATE INDEX idx_subject_keys_kek_id ON subject_encryption_keys (kek_id);
