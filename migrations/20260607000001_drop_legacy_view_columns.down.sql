-- Restore the legacy event indexes.
CREATE INDEX idx_events_person_captured_subject
    ON events ((payload -> 'PersonCaptured' ->> 'subject_id'))
    WHERE event_type = 'PersonCaptured';

CREATE INDEX idx_events_person_details_updated_subject
    ON events ((payload -> 'PersonDetailsUpdated' ->> 'subject_id'))
    WHERE event_type = 'PersonDetailsUpdated';

-- Restore the `current_step` column.
ALTER TABLE journey_view
    ADD COLUMN current_step TEXT;
