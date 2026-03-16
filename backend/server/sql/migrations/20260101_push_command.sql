-- Add push_command column to GitHubJobSets table to support post-build hooks
-- This allows users to execute custom commands after successful builds (e.g., cache pushing)
ALTER TABLE GitHubJobSets ADD COLUMN push_command TEXT NULL;
