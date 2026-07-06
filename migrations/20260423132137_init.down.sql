-- Reverse the up migration: drop everything in dependency order.
-- Indexes are dropped automatically with their tables.

DROP TABLE IF EXISTS subject_lookup            CASCADE;
DROP TABLE IF EXISTS journey_workflow_decision CASCADE;
DROP TABLE IF EXISTS journey_person            CASCADE;
DROP TABLE IF EXISTS journey_view              CASCADE;
DROP TABLE IF EXISTS subject_encryption_keys   CASCADE;
DROP TABLE IF EXISTS events                    CASCADE;
