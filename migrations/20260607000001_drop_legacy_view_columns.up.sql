-- Drop the vestigial `current_step` column: it was only ever set by the
-- (now-removed) StepProgressed event, so it is always NULL. Read
-- WorkflowDecisionView.phase instead.
ALTER TABLE journey_view
    DROP COLUMN IF EXISTS current_step;

-- Drop the event-store indexes for the removed legacy events. The only
-- subject-bearing events now are SubjectRegistered and AttributesSet
-- (the latter covered by idx_events_attributes_set_subjects).
DROP INDEX IF EXISTS idx_events_person_captured_subject;
DROP INDEX IF EXISTS idx_events_person_details_updated_subject;
