-- Core operational table: maps subject_id → normalised email.
-- Written transactionally with the event store (see TransactionalEventRepository).
-- Rows are DELETED when the subject is crypto-shredded — email is PII and must
-- not persist after erasure.  Idempotency of the shred flow is preserved because
-- DEK deletion is already idempotent.
CREATE TABLE subject_lookup (
    subject_id  UUID      NOT NULL PRIMARY KEY,
    email_lower TEXT      NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Supports the shred-by-email lookup: SELECT subject_id WHERE email_lower = $1
CREATE INDEX idx_subject_lookup_email
    ON subject_lookup (email_lower);

-- Backfill from the existing journey_person projection so that existing
-- deployments retain shred-by-email capability without a full replay.
INSERT INTO subject_lookup (subject_id, email_lower)
SELECT DISTINCT ON (subject_id) subject_id, lower(email)
FROM   journey_person
WHERE  NOT forgotten
  AND  email IS NOT NULL
ON CONFLICT (subject_id) DO NOTHING;
