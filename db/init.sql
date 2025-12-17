CREATE TABLE events
(
    aggregate_type text                         NOT NULL,
    aggregate_id   text                         NOT NULL,
    sequence       bigint CHECK (sequence >= 0) NOT NULL,
    event_type     text                         NOT NULL,
    event_version  text                         NOT NULL,
    payload        json                         NOT NULL,
    metadata       json                         NOT NULL,
    PRIMARY KEY (aggregate_type, aggregate_id, sequence)
);

-- Structured read model for journey views
CREATE TABLE journey_view
(
    id                  UUID                         NOT NULL PRIMARY KEY,
    state               TEXT                         NOT NULL CHECK (state IN ('InProgress', 'Complete')),
    current_step        TEXT,
    version             BIGINT CHECK (version >= 0)  NOT NULL DEFAULT 0,
    created_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Table for captured data in journeys
CREATE TABLE journey_data_capture
(
    id                  SERIAL                       NOT NULL PRIMARY KEY,
    journey_id          UUID                         NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    key                 TEXT                         NOT NULL,
    value               JSONB                        NOT NULL,
    sequence            INTEGER                      NOT NULL,
    created_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (journey_id, sequence)
);

-- Table for workflow decisions
CREATE TABLE journey_workflow_decision
(
    id                  SERIAL                       NOT NULL PRIMARY KEY,
    journey_id          UUID                         NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    available_actions   TEXT[]                       NOT NULL,
    primary_next_step   TEXT,
    created_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    is_latest           BOOLEAN                      NOT NULL DEFAULT TRUE
);

-- Table for person data captured during journeys
CREATE TABLE journey_person
(
    id                  SERIAL                       NOT NULL PRIMARY KEY,
    journey_id          UUID                         NOT NULL REFERENCES journey_view(id) ON DELETE CASCADE,
    name                TEXT                         NOT NULL,
    email               TEXT                         NOT NULL,
    phone               TEXT,
    created_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at          TIMESTAMP                    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (journey_id)
);

-- Index for quick lookups
CREATE INDEX idx_journey_data_capture_journey_id ON journey_data_capture(journey_id);
CREATE INDEX idx_journey_workflow_decision_journey_id ON journey_workflow_decision(journey_id);
CREATE INDEX idx_journey_workflow_decision_latest ON journey_workflow_decision(journey_id, is_latest) WHERE is_latest = TRUE;
CREATE INDEX idx_journey_person_journey_id ON journey_person(journey_id);
CREATE INDEX idx_journey_person_email ON journey_person(email);

-- Legacy table for compatibility (can be removed if not needed)
CREATE TABLE journey_query
(
    view_id             TEXT                        NOT NULL,
    version             BIGINT CHECK (version >= 0) NOT NULL,
    payload             JSON                        NOT NULL,
    PRIMARY KEY (view_id)
);

CREATE USER demo_user WITH ENCRYPTED PASSWORD 'demo_pass';
GRANT ALL PRIVILEGES ON DATABASE postgres TO demo_user;
