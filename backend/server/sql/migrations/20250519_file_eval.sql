-- Represents a unique commit for a PR
--
-- Different CI jobs will be spawned in reference to this
CREATE TABLE IF NOT EXISTS GitCheckout (
    git_repo TEXT NOT NULL,
    git_commit TEXT NOT NULL,
);

-- This is similar to hydra's "legacy evaluation" model
-- You target a file with the ability to pass additional args. This will
-- create a [deeply nested] attset of drvs. For EkaCI, we pass this to nix-eval-jobs
-- and then consume the output to determine our drv graphs.
CREATE TABLE IF NOT EXISTS FileEval (
    -- Identifier used to mark an eval job. To find the "previous" evaluation,
    -- we should attempt to find the evaluation job with the same name for the commit
    -- on the parent branch
    job_name TEXT NOT NULL;
    file_path TEXT NOT NULL,
    -- These are arguments to be passed to nix-eval-jobs
    -- TBD whether exposing this is even desirable. For CI, may be preferrable
    -- to just have people use different files.
    -- Hydra + NixOS use this to pass things like supportedSystems or git revision
    args TEXT NOT NULL,
    -- TBD, whether this should be it's own table, as eventually we will need to
    -- to register EkaCI as a GitHub app. Also don't want to over optimise for
    -- just the GitHub assumptions
    git_checkout_id INTEGER NOT NULL,
    FOREIGN KEY (git_checkout_id) REFERENCES GitCheckout(row_id) ON DELETE CASCADE,
);

