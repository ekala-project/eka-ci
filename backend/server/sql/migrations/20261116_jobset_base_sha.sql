-- Record which base commit a pull-request jobset was evaluated against.
-- When no base-branch baseline exists for a changed attr, size checks fall
-- back to the base commit's derivation of that attr, measuring it from the
-- local store or the substituters, and record it as the baseline.
-- NULL for base-branch jobsets and for jobsets created before this migration.
ALTER TABLE GitHubJobSets ADD COLUMN base_sha TEXT;
