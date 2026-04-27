# GitHub PR Comment Commands

eka-ci listens for comments on pull requests that mention the bot and
dispatches supported commands. This document lists the commands that are
currently recognized, the conditions under which they succeed, and the
feedback users should expect.

## Summary

| Command | Purpose |
|---|---|
| `@eka-ci merge` | Queue the PR to be merged once CI passes, using the repository's default merge method (or the configured default `squash`). |
| `@eka-ci merge merge` | Same, with an explicit `merge` (merge-commit) method. |
| `@eka-ci merge squash` | Same, with an explicit `squash` method. |
| `@eka-ci merge rebase` | Same, with an explicit `rebase` method. |
| `@eka-ci merge cancel` | Withdraw a previously-issued `@eka-ci merge` request. |

The bot mention is case-insensitive (`@eka-ci`, `@Eka-CI`, `@EKA-CI` all
work). The command verb and method are also case-insensitive.

## Where commands are accepted

The comment must be on a **pull request** (comments on plain issues are
ignored). Additionally:

- Only **newly-created** comments trigger commands — edits and deletions
  do not revoke or re-issue commands. If you want to cancel, post
  `@eka-ci merge cancel` as a new comment.
- Bot-authored comments are ignored (the `User.type` field on the
  comment author must not be `Bot`).
- The `@eka-ci` mention must be the **first non-whitespace token of a
  line**. Mentions embedded in prose — e.g. `cc @eka-ci please help` —
  are deliberately ignored to prevent accidental triggers.
- A single comment may span multiple lines; the first line that parses
  as a command wins. Other lines are treated as prose.

## Parser behavior

- The bot handle match is case-insensitive (ASCII).
- The command verb (`merge`) must immediately follow the mention.
- Unknown verbs (e.g. `@eka-ci rebuild`) are silently ignored.
- Unknown methods (e.g. `@eka-ci merge squashh`) fall back to a bare
  `@eka-ci merge` rather than rejecting the whole comment. This
  permissive behavior means typos still queue a merge.
- Trailing tokens after the method are ignored:
  `@eka-ci merge squash please thanks` parses as a squash-merge request.

## `@eka-ci merge [method]`

Queues the PR for auto-merge once all CI gates pass.

### Authorization

The comment author must satisfy **at least one** of:

1. **Repo permission of `write`, `maintain`, or `admin`** on the
   repository the PR targets, OR
2. Be a **registered maintainer of every package** whose source is
   changed by the PR (per the eka-ci maintainers table).

If neither condition holds:

- The bot reacts `-1` to the command comment.
- The bot posts a reply explaining that the command was denied.
- No merge request is recorded.

### Push-time (force-push) protection

To guard against commits landing between when a reviewer types the
command and when the webhook is processed, the bot performs a
best-effort timestamp check before accepting the request:

- It fetches the current head commit via the GitHub API and reads the
  `committer.date`.
- If that timestamp is more than **30 seconds** after the
  `created_at` of the triggering comment, the bot refuses:
  - Reacts `-1` on the command comment.
  - Posts a reply naming the head commit and asking the user to
    review the new changes and re-issue the command.

The 30-second grace window absorbs clock skew between GitHub's event
recorder and the commit-metadata service. If the API call fails or the
committer date cannot be parsed, the check falls open — the bot
proceeds and relies on the post-acceptance SHA-drift check (below) as a
second line of defense.

Caveat: Because the signal is the commit's own `committer.date`, a
force-push of a much older, cherry-picked commit will not trigger this
check. The post-acceptance SHA-drift hook still catches that case.

### Request recording and acknowledgement

On acceptance, the bot:

1. Records a pending comment-merge request pinned to the **current
   head SHA**, together with the requested merge method (if any), the
   requester's GitHub user id/login, and the comment id.
2. Reacts `+1` on the command comment as a visible ack.
3. Kicks the auto-merge evaluator immediately so that if CI gates are
   already green, the merge lands right away.

### Merge method selection

When the auto-merger eventually runs, it selects the merge method in
this order of preference:

1. The method explicitly given in the comment (`merge` / `squash` /
   `rebase`), if any.
2. The PR's stored merge-method preference (set via the UI), if any.
3. `squash` (the default fallback).

If the selected method is disabled in the repository's merge settings,
the bot logs a warning and skips auto-merge. No comment is posted in
that case — the requester is expected to re-issue with an allowed
method.

### Post-acceptance SHA-drift protection

After the comment-merge is recorded, the bot continues to monitor the
PR. If any new commit lands on the head branch before the merge
completes (PR `Synchronize` webhook), the request is cancelled:

1. A `:confused:` reaction is added to the original command comment.
2. A reply is posted naming the **expected** (pinned) and **current**
   head SHAs, and instructing the user to re-issue the command against
   the updated PR.
3. The pending merge request is cleared from the database.

This is a hard guarantee: the merge bot will never land a commit that
the requester did not explicitly target.

### Gates the merge still has to pass

`@eka-ci merge` is **not** a force-merge. It opts the PR into the
auto-merge evaluator, which still requires:

- The head commit's jobset has fully concluded with no failing
  new-or-changed jobs (`pr_head_build_succeeded`).
- The merge method selected is allowed by the repository settings.
- Any other CI gates configured on the commit are passing (these are
  enforced by GitHub's own branch-protection rules independently of
  eka-ci).

Note that the package-maintainer approval gate used for UI-triggered
auto-merges is **skipped** for comment-driven merges, because the
requester's authorization was already verified at command time.

## `@eka-ci merge cancel`

Withdraws an outstanding comment-merge request.

### Behavior when nothing is pending

If no `@eka-ci merge` is currently pending on the PR, the bot silently
no-ops. It does not react, does not post, and does not write to the
database. This is intentional: it denies unauthorized commenters any
signal about bot state.

### Authorization

The comment author must satisfy **at least one** of:

1. Be the **original requester** of the pending merge (identified by
   GitHub user id). This is a fast path that skips the permission API
   call.
2. Have **`write`, `maintain`, or `admin`** on the repository.
3. Be a **maintainer of every changed package** in the PR.

If none of these hold:

- The bot reacts `-1` on the cancel comment.
- The bot posts a reply explaining why the cancel was denied.
- The pending merge request remains in place.

This prevents random commenters from griefing pending maintainer
merges.

### On acceptance

- The pending merge request is cleared from the database.
- The bot reacts `+1` on the cancel comment.
- Any subsequent auto-merge evaluation proceeds as if no comment-merge
  had ever been issued (ambient auto-merge remains in effect if it was
  separately enabled via the UI).

## Rate limiting

To protect the installation's shared GitHub API budget (5000 req/hr),
commands are rate-limited **per `(user_id, owner, repo)` triple** at
the webhook boundary:

- Minimum interval: **5 seconds** between accepted commands from the
  same user on the same repository.
- Rejections are **silent**: no reaction, no comment, no DB write.
  Feedback would itself amplify the spam the limit is designed to
  contain.
- The rate-limit state is process-local and non-persistent; it resets
  on server restart. It protects against burst spam only; sustained
  abuse is left to GitHub's own abuse-detection systems.

If you legitimately need to correct a just-issued command (e.g. wrong
method), wait 5 seconds before re-issuing, or use
`@eka-ci merge cancel` followed by the new command.

## User-visible reactions

The bot uses the following reactions on the triggering comment as a
compact status signal:

| Reaction | Meaning |
|---|---|
| `+1` | Command accepted (merge queued, or cancel recorded). |
| `-1` | Command denied (unauthorized, or refused due to push-time drift). |
| `rocket` | The requested merge succeeded (added after the PR merges). |
| `confused` | A previously-accepted comment-merge was cancelled due to post-acceptance SHA drift. |

## Examples

Queue a squash-merge:

```
Looks good to me!
@eka-ci merge squash
```

Queue the repository-default merge method:

```
@eka-ci merge
```

Withdraw a pending request:

```
Actually, hold off — I want to add another commit.
@eka-ci merge cancel
```

Multi-line comment where prose precedes the command:

```
LGTM after the last round of fixes.
@eka-ci merge rebase
Thanks for the reviews!
```

## Things that are NOT supported

These are intentionally out of scope as of this writing:

- Commands from comment **edits** or **deletions** — only newly-created
  comments trigger anything.
- Commands embedded inside prose (`cc @eka-ci please merge`). The
  mention must be the first non-whitespace token on its line.
- Verbs other than `merge` (`@eka-ci rebuild`, `@eka-ci retry`, etc.)
  are parsed and silently ignored. They may be added in future
  versions.
- Queueing multiple merge requests against the same PR — the most
  recent accepted request overwrites the previous one.
- Explicit SHA arguments (`@eka-ci merge <sha>`). The current head SHA
  is always captured implicitly at command time.
