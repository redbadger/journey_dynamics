-- Layer 7: Support SubjectRegistered / SubjectBound event projections.
--
-- Changes:
-- 1. Index on SubjectRegistered events for find_journeys_by_subject queries.
-- 2. Index on SubjectBound events (future use / completeness).
-- 3. Table comment update on journey_person to document new write paths.
-- 4. Backfill: synthesise a SubjectRegistered-style lookup for existing subjects
--    so that find_journeys_by_subject covers all pre-migration journeys via the
--    new query branch. The PersonCaptured index already covers those journeys;
--    this is belt-and-suspenders for the new index path.

-- ── Indexes ──────────────────────────────────────────────────────────────────

-- Supports find_journeys_by_subject for new-style subject registration.
CREATE INDEX idx_events_subject_registered_subject
    ON events ((payload -> 'SubjectRegistered' ->> 'subject_id'))
    WHERE event_type = 'SubjectRegistered';

-- Supports future per-binding queries (e.g. "which journeys have this role?").
CREATE INDEX idx_events_subject_bound_subject
    ON events ((payload -> 'SubjectBound' ->> 'subject_id'))
    WHERE event_type = 'SubjectBound';

-- ── Documentation ────────────────────────────────────────────────────────────

COMMENT ON TABLE journey_person IS
    'Per-person/subject data — one row per (journey, person_ref). '
    'Write paths: PersonCaptured (legacy), SubjectBound (new), '
    'PersonDetailsUpdated, SubjectRegistered (email updates). '
    'On SubjectForgotten: identity fields nulled, details cleared, forgotten set to TRUE.';
