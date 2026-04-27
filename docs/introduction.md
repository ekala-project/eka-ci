# Introduction

**Eka CI** is a Continuous Integration server purpose-built for Nix projects. It is designed
to make reviewing Nix-based pull requests fast and trustworthy, especially for repositories
that are too large to be reviewed by hand on every change.

The goal is to answer one question as quickly and as reliably as possible:

> *Should I merge this PR?*

To do that, Eka CI focuses on the things that actually matter for a Nix repository:

- **Does evaluation still succeed?**
- **Which packages were added, removed, newly succeed, or newly fail?**
- **What is the closure-size and dependency impact of the change?**
- **What does the rebuild blast radius look like across systems?**

Manual review processes do not scale to repositories the size of Nixpkgs. Eka CI replaces
that workflow with a small set of strong signals attached directly to each pull request.

## What Eka CI provides

- **GitHub App integration** — webhook-based event handling, check runs, merge queue support,
  and fine-grained credential management.
- **Nix-aware build orchestration** — dependency graph tracking with an LRU cache, multi-tier
  build queues, dedicated FOD queue, remote builders, and `requiredSystemFeatures` support.
- **Binary cache integration** — S3, Cachix, and Attic, with credential sources ranging from
  environment variables to Vault, AWS Secrets Manager, and `systemd-creds`.
- **Change summaries and rebuild impact** — per-PR diffs of which packages changed and how
  many derivations have to rebuild, posted as a single GitHub check.
- **Build metrics** — output (NAR) size and closure size tracked over time, compared against
  the base branch, with configurable thresholds.
- **PR comment commands** — `@eka-ci merge` and friends for queueing merges from a comment.

## Components

Eka CI is a Cargo workspace with two main binaries:

- `eka-ci-server` — the long-running CI server that talks to GitHub and orchestrates builds.
- `ekaci` — a CLI client that talks to the server over a Unix socket.

A web frontend (Elm) lives alongside but is partially implemented; the HTTP API and
WebSocket endpoints are the supported integration surface today.

## How to read these docs

If you are setting Eka CI up for the first time, start with [Quick Start](./quick-start.md)
and then work through [Installation](./installation.md) and
[GitHub App Setup](./github-app-setup.md).

If you are operating an existing deployment, the
[LRU Cache Tuning](./lru-cache-tuning.md) and
[Monitoring & Metrics](./monitoring.md) pages are the most useful starting points.

For a deeper picture of how the server is built, see [Architecture](./architecture.md).
