DROP INDEX IF EXISTS idx_journey_person_subject_id;
ALTER TABLE journey_person DROP COLUMN IF EXISTS subject_id;
DROP INDEX IF EXISTS idx_journey_subject_mapping_subject_id;
DROP TABLE IF EXISTS journey_subject_mapping;
DROP INDEX IF EXISTS idx_subject_keys_subject_id;
DROP TABLE IF EXISTS subject_encryption_keys;
