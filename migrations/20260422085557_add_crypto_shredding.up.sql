-- Stores per-subject Data Encryption Keys (DEKs), wrapped with the KEK.
-- When a subject is forgotten (GDPR erasure), their row is hard-deleted.
-- An audit trail exists via the SubjectForgotten event in the event store.
CREATE TABLE subject_encryption_keys (
    key_id      UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id  UUID      NOT NULL UNIQUE,
    wrapped_key BYTEA     NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_keys_subject_id ON subject_encryption_keys(subject_id);

-- Maps journeys (aggregates) to data subjects.
-- Populated when a PersonCaptured event is persisted.
-- Used to determine which DEK to use when encrypting Modified events.
CREATE TABLE journey_subject_mapping (
    aggregate_id TEXT      NOT NULL PRIMARY KEY,
    subject_id   UUID      NOT NULL,
    created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_journey_subject_mapping_subject_id ON journey_subject_mapping(subject_id);

-- Add subject_id to journey_person so we can delete by subject during shredding.
ALTER TABLE journey_person ADD COLUMN subject_id UUID;
CREATE INDEX idx_journey_person_subject_id ON journey_person(subject_id);
