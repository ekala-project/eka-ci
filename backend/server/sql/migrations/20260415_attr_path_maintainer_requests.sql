-- Migration: Attr Path Maintainer Requests
-- Created: 2026-04-15
-- Description: Add table for tracking requests to become an attr path maintainer

CREATE TABLE IF NOT EXISTS AttrPathMaintainerRequests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    attr_path TEXT NOT NULL,
    github_user_id INTEGER NOT NULL,
    requested_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    status TEXT NOT NULL CHECK(status IN ('pending', 'approved', 'rejected')) DEFAULT 'pending',
    reviewed_by_user_id INTEGER,
    reviewed_at TIMESTAMP,
    FOREIGN KEY (github_user_id) REFERENCES AuthenticatedUsers(github_user_id) ON DELETE CASCADE,
    FOREIGN KEY (reviewed_by_user_id) REFERENCES AuthenticatedUsers(github_user_id) ON DELETE SET NULL,
    UNIQUE(attr_path, github_user_id)
);

-- Index for querying pending requests
CREATE INDEX IF NOT EXISTS idx_attr_path_maintainer_requests_status ON AttrPathMaintainerRequests(status);

-- Index for querying requests by user
CREATE INDEX IF NOT EXISTS idx_attr_path_maintainer_requests_user ON AttrPathMaintainerRequests(github_user_id);

-- Index for querying requests by attr_path
CREATE INDEX IF NOT EXISTS idx_attr_path_maintainer_requests_attr_path ON AttrPathMaintainerRequests(attr_path);
