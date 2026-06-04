-- GIN index for locating journeys that touched a given data subject via
-- a SetAttributes command.
--
-- The `subjects` array (`payload -> 'AttributesSet' -> 'subjects'`) is
-- populated automatically by the crypto layer when encrypting partitions;
-- each element is the string UUID of one data subject whose secret attributes
-- were updated by the command.
--
-- Query pattern (see view_repository::find_journeys_by_subject):
--   payload -> 'AttributesSet' -> 'subjects' @> jsonb_build_array($1::text)
CREATE INDEX idx_events_attributes_set_subjects
    ON events USING GIN ((payload -> 'AttributesSet' -> 'subjects'))
    WHERE event_type = 'AttributesSet';
