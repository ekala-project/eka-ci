# (WIP) EkaCI

This tool is meant to provide an optimized reviewing experience for small to
large nix package repositories. In particular, the tool should provide:
- succinct PR review workflows
  - Does the eval still succeed?
  - Only inspected builds that have changed
    - Added, removed, [newly/still] succeeding, [newly/still] failing builds
  - Closure size difference
  - Retained dependency differences
  - Captured logs
  - Explore dependency failures (similar to hydra)
  - (Stretch goal) Diffoscope like diff of package outputs?
  - (Strech goal) diffs of NixOS configurations.

Ultimately, this tool is meant to answer, "should I merge this PR" in the quickest manner possible.
Curating a Nix repository should not be highly limited to manual review processes of a reviewer.
This doesn't scale well, and is error prone.

# Design

See the [design document](./DESIGN.md) for details about a high-level overview
of EkaCI.

# MVP Roadmap

Server + evaluator + build queue

- GitHub OAuth
  - [ ] Allow users to register through GitHub
- GitHub webhooks
  - [x] App registration workflow
  - [ ] Receive PR review events
  - [ ] Send check gates
- PR Review workflow
  - [ ] Git checkout
  - [ ] Evaluate derivation differences between head and base branch
  - [ ] Queue changed derivations for build
  - [ ] Allow successful builds to push outputs to attic
  - [ ] Calculate changed metrics between builds: build and runtime closure size, dependencies
- Push built artifacts
  - [ ] Allow for a time-lease to be configured for "jobsets", to enable attic integration

Frontend

- [ ] GitHub OAuth, allow users to review as github user
- Review PR portal
  - [ ] Ordered list of PRs available for review
    - [ ] Default ordering: Rebuild count, then by lines changed?
  - [ ] Link back to PR, to allow for comments and requesting changes
- PR review
  - [ ] Textual diff
  - [ ] Metrics view: build+runtime closure size and dependencies
  - [ ] Added, removed, [newly/still] building, [newly/still] failing builds
  - [ ] Allow for approvals + merges

# (Future) Evaluation modes

- "Legacy" (needed for MVP)
  - Similar to "legacy jobsets" in hydra.
  - Allows for an arbitrary expression to be evaluated returning an attrset of derivations
  - A diff of new/changed/removed attrs from the base branch to head branch will create a "check_run" for each new and changed drv
- "OfBorg" Convention
  - Read attr path from commit message to determine package rebuild to rebuild
  - (For ekapkgs and userpkgs) would like for the ability to select downstream build strategy
    - For ekapkgs, just have the package in question and immediate referrers rebuild
    - For userpkgs, just limit to the package in question. Unlikely to have dependencies to other userpkgs'
- Flake checks
  - Similar to garnix
  - Simple, "for each check, create a check_run with the status"
- Flake develop actions
  - For use with `nix develop --command <impure command>`
  - Similar to gitlab's script runner, just impurely runs code
  - Devshell is retained through gcroots
