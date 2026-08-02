-- Findings first: they reference scans, which reference policies. Dropping a
-- table drops its indexes and constraints with it.
DROP TABLE IF EXISTS inspector_findings;
DROP TABLE IF EXISTS inspector_scans;
DROP TABLE IF EXISTS inspector_policies;
