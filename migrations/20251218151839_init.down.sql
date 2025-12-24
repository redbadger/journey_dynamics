-- Revoke privileges
REVOKE ALL PRIVILEGES ON DATABASE journey_dynamics FROM postgres;

-- Drop indexes
DROP INDEX IF EXISTS idx_journey_person_email;
DROP INDEX IF EXISTS idx_journey_person_journey_id;
DROP INDEX IF EXISTS idx_journey_workflow_decision_latest;
DROP INDEX IF EXISTS idx_journey_workflow_decision_journey_id;
DROP INDEX IF EXISTS idx_journey_accumulated_data;

-- Drop tables in reverse order of creation (due to foreign key dependencies)
DROP TABLE IF EXISTS journey_query;
DROP TABLE IF EXISTS journey_person;
DROP TABLE IF EXISTS journey_workflow_decision;

DROP TABLE IF EXISTS journey_view;
DROP TABLE IF EXISTS events;
