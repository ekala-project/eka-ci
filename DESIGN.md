# EkaCI Design

The main distinguishing feature of EkaCI will be a global drv status cache which
will be cheap to query whether a build for a drv has been attempted or queued.
Since derivation paths are unique, and nix is highly reproducible, we should only
ever need to attempt a build once (save for transient failures such as OOM or out of disk space).

The EkaCI tool will provide two "interfaces". One as a CI/CD tool which can emit
PR check information. The other interface will be a web SPA which acts as review
portal for maintainers to easily review PRs.

## EkaCI, the CI experience

"As a PR Author, I would like to be informed as to when my PR fails eval, its build, or causes regressions"

Terms:
- check_run: these are "github check_runs", which are the "PR gates"/"checks" for a PR

EkaCI for use with corepkgs is intended to have a very simple workflow:
- Register EkaCI as a github app
  - Enables EkaCI to emit check_runs as well as subscribe to pull request events
- Determine drvs which need to be built
  - This should be a mapping of attr path to drv
  - drvs to build are determined by subtracting the base branch drvs from the drvs on the head branch
    - This should scale the number of checks to the number of downstream rebuilds
- Traverse the drv graph, attempt the builds, and memoize the build results:
  - If a drv dependency has already failed, then all downstream drvs are also considered failures
  - Queue all unattempted drvs into a build queue
    - Emit a CI "check_run" for each drv which has an associated attr path.
      - Initially, this should be pending status
    - If an unattempted drv has all of its dependencies successfully buildling, enqueue the unattempted drv to be built
    - When a build completes:
      - If it failed, then propagate failed build status to all downstream drvs
        - Because a failure could be transient, we should have the ability to restart a build
      - It if was successful, scan all immediate referrers (downstream drvs), and see if all of their dependencies are now building; if they are, then enqueue the drv to be built.
  - When builds reach a successful or failed state, the associated check_run state should be updated with respect to the build status
    - "check_runs" should link back to the build logs for a drv
- Provide a link to the "review portal" for individuals who want to self-review their PR.

## EkaCI, the review portal experience

"As a reviewer of packaging PRs, I would like to make a quick determination if I can merge a PR"

- When reviewing a PR, I should see:
  - PR message
  - PR comments
  - rebuild count
  - diff of the code (Similar to "Files changed" tab in github)
  - Added, removed, (newly/still) succeeding/failing builds.
  - Per build break down of metrics:
    - diff of realized output file structure
      - (future goal) diffoscope-like diff of realized outputs
    - (stretch goal metrics)
      - evaluation time difference?
        - this would mean having a fresh eval cache, which may not be desirable
      - build time
        - Would need to ensure that all dependencies are present to avoid building/substituting dependencies doesn't dominate this measure
          - Doing nix-shell/"nix develop" and immediately exiting would be one hack to constitute all dependencies
      - closure size difference (base vs head), if build corresponds to an attr path
    - diff of upstream source code? (when src has also changed)

# Additional considerations

- Would be nice to be able to configure lifetimes for built artifacts to live for pushing to cache
  - Would allow for use of [attic](https://github.com/zhaofengli/attic), which can garbage collect the caches
