-- ── Event store ────────────────────────────────────────────────────────────--
-- Managed by cqrs-es; structure must not change.
CREATE TABLE events
(
    aggregate_type TEXT                         NOT NULL,
    aggregate_id   TEXT                         NOT NULL,
    sequence       BIGINT CHECK (sequence >= 0) NOT NULL,
    event_type     TEXT                         NOT NULL,
    event_version  TEXT                         NOT NULL,
    payload        JSONB                        NOT NULL,
    metadata       JSONB                        NOT NULL,
    timestamp      TIMESTAMP WITH TIME ZONE     DEFAULT (CURRENT_TIMESTAMP),
    PRIMARY KEY (aggregate_type, aggregate_id, sequence)
);

-- Find all journeys that reference a given subject_id. Both event types carry
-- subject_id in plaintext; AttributesSet exposes a `subjects` array written by
-- the crypto layer. Partial indexes keep each one narrow.
CREATE INDEX idx_events_subject_registered_subject
    ON events ((payload -> 'SubjectRegistered' ->> 'subject_id'))
    WHERE event_type = 'SubjectRegistered';

CREATE INDEX idx_events_subject_bound_subject
    ON events ((payload -> 'SubjectBound' ->> 'subject_id'))
    WHERE event_type = 'SubjectBound';

CREATE INDEX idx_events_attributes_set_subjects
    ON events USING GIN ((payload -> 'AttributesSet' -> 'subjects'))
    WHERE event_type = 'AttributesSet';

-- ── Journey read model — shared, non-PII data only ───────────────────────────
-- Populated from AttributesSet events; never cleared by shredding.
CREATE TABLE journey_view
(
    id          UUID      NOT NULL PRIMARY KEY,
    state       TEXT      NOT NULL CHECK (state IN ('InProgress', 'Complete')),
    shared_data JSONB     NOT NULL DEFAULT '{}',
    version     BIGINT    NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_journey_shared_data
    ON journey_view USING GIN (shared_data);

-- ── Latest workflow decision per journey ─────────────────────────────────────
CREATE TABLE journey_workflow_decision
(
    id                SERIAL    NOT NULL PRIMARY KEY,
    journey_id        UUID      NOT NULL REFERENCES journey_view (id) ON DELETE CASCADE,
    suggested_actions TEXT[]    NOT NULL,
    phase             TEXT,
    created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_latest         BOOLEAN   NOT NULL DEFAULT TRUE
);

CREATE INDEX idx_journey_workflow_decision_journey_id
    ON journey_workflow_decision (journey_id);
CREATE INDEX idx_journey_workflow_decision_latest
    ON journey_workflow_decision (journey_id, is_latest)
    WHERE is_latest = TRUE;

-- ── Per-person/subject projection ────────────────────────────────────────────
-- One row per (journey, person_ref). Write paths: SubjectBound creates the row
-- (email from subject_lookup); SubjectRegistered updates the email; AttributesSet
-- mirrors per-person secret fields into `details`. On SubjectForgotten the
-- identity fields are nulled, details cleared, and forgotten set TRUE.
CREATE TABLE journey_person
(
    journey_id UUID      NOT NULL REFERENCES journey_view (id) ON DELETE CASCADE,
    person_ref TEXT      NOT NULL,
    subject_id UUID      NOT NULL,
    name       TEXT,
    email      TEXT,
    phone      TEXT,
    details    JSONB     NOT NULL DEFAULT '{}',
    forgotten  BOOLEAN   NOT NULL DEFAULT FALSE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (journey_id, person_ref)
);

-- Person lookups by subject (used during shredding to find affected rows).
CREATE INDEX idx_journey_person_subject_id
    ON journey_person (subject_id);

-- ── Per-subject Data Encryption Keys (DEKs) ──────────────────────────────────
-- Wrapped with a versioned application KEK. Hard-deleting a row is the
-- crypto-shredding operation; the audit trail lives in SubjectForgotten events.
CREATE TABLE subject_encryption_keys
(
    key_id       UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id   UUID      NOT NULL UNIQUE,
    wrapped_key  BYTEA     NOT NULL,
    kek_id       TEXT      NOT NULL,
    rewrapped_at TIMESTAMP,
    created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_subject_keys_subject_id
    ON subject_encryption_keys (subject_id);
-- Lets the background re-wrap sweeper find stale (old-KEK) rows efficiently.
CREATE INDEX idx_subject_keys_kek_id
    ON subject_encryption_keys (kek_id);

-- ── Email → subject_id index ─────────────────────────────────────────────────
-- Written transactionally with the event store (SubjectLookupHook). Rows are
-- DELETED on crypto-shredding — email is PII and must not persist after erasure.
CREATE TABLE subject_lookup
(
    subject_id  UUID      NOT NULL PRIMARY KEY,
    email_lower TEXT      NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Supports shred-by-email: SELECT subject_id WHERE email_lower = $1.
CREATE INDEX idx_subject_lookup_email
    ON subject_lookup (email_lower);
