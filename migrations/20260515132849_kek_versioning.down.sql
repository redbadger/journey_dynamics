-- Reverse the kek_versioning migration.
DROP INDEX IF EXISTS idx_subject_keys_kek_id;

ALTER TABLE subject_encryption_keys
    DROP COLUMN IF EXISTS rewrapped_at,
    DROP COLUMN IF EXISTS kek_id;
