-- events table (managed by cqrs-es; structure must not change)
CREATE TABLE events
(
    aggregate_type text                         NOT NULL,
    aggregate_id   text                         NOT NULL,
    sequence       bigint CHECK (sequence >= 0) NOT NULL,
    event_type     text                         NOT NULL,
    event_version  text                         NOT NULL,
    payload        json                         NOT NULL,
    metadata       json                         NOT NULL,
    timestamp      timestamp with time zone     DEFAULT (CURRENT_TIMESTAMP),
    PRIMARY KEY (aggregate_type, aggregate_id, sequence)
);

-- Journey read model — shared, non-PII data only.
-- Populated from Modified events; never cleared by shredding.
CREATE TABLE journey_view
(
    id           UUID   NOT NULL PRIMARY KEY,
    state        TEXT   NOT NULL CHECK (state IN ('InProgress', 'Complete')),
    shared_data  JSONB  NOT NULL DEFAULT '{}',
    current_step TEXT,
    version      BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Latest workflow decision for each journey.
CREATE TABLE journey_workflow_decision
(
    id                SERIAL    NOT NULL PRIMARY KEY,
    journey_id        UUID      NOT NULL REFERENCES journey_view (id) ON DELETE CASCADE,
    suggested_actions TEXT[]    NOT NULL,
    created_at        TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_latest         BOOLEAN   NOT NULL DEFAULT TRUE
);

-- Per-person data — one row per (journey, person_ref).
-- Populated from PersonCaptured and PersonDetailsUpdated events.
-- On SubjectForgotten: identity fields are nulled, details cleared, forgotten set to TRUE.
-- Multiple persons per journey are fully independent.
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

-- Per-subject Data Encryption Keys (DEKs), wrapped with the application KEK.
-- Hard-deleting a row is the crypto-shredding operation.
-- An audit trail is preserved via SubjectForgotten events in the event store.
CREATE TABLE subject_encryption_keys
(
    key_id      UUID      NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_id  UUID      NOT NULL UNIQUE,
    wrapped_key BYTEA     NOT NULL,
    created_at  TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- ── Indexes ──────────────────────────────────────────────────────────────────

-- GIN index for querying shared journey data
CREATE INDEX idx_journey_shared_data
    ON journey_view USING GIN (shared_data);

-- Workflow decision lookups
CREATE INDEX idx_journey_workflow_decision_journey_id
    ON journey_workflow_decision (journey_id);
CREATE INDEX idx_journey_workflow_decision_latest
    ON journey_workflow_decision (journey_id, is_latest)
    WHERE is_latest = TRUE;

-- Person lookups by subject (used during shredding to find affected rows)
CREATE INDEX idx_journey_person_subject_id
    ON journey_person (subject_id);

-- DEK lookups by subject
CREATE INDEX idx_subject_keys_subject_id
    ON subject_encryption_keys (subject_id);

-- Event store: find all journeys that reference a given subject_id.
-- Both indexes only cover rows of the relevant event type to stay narrow.
CREATE INDEX idx_events_person_captured_subject
    ON events ((payload::jsonb -> 'PersonCaptured' ->> 'subject_id'))
    WHERE event_type = 'PersonCaptured';

CREATE INDEX idx_events_person_details_updated_subject
    ON events ((payload::jsonb -> 'PersonDetailsUpdated' ->> 'subject_id'))
    WHERE event_type = 'PersonDetailsUpdated';
