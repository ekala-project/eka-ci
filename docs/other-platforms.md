# GitLab and Gitea support plan

## Why this is a real refactor

eka-ci has GitHub assumptions baked into eight surfaces:

| Surface | GitHub-specific bit |
|---|---|
| Schema | `GitHubJobSets` table, `GitHubAppConfig` |
| Auth | GitHub App JWT + installation tokens (`octocrab`) |
| Webhooks | GitHub event payload shapes, signature header `X-Hub-Signature-256` |
| Check posting | GitHub Checks API (CheckRun, Annotations) |
| Repo metadata | `octocrab::Repository`, html_url-derived domain |
| PR comment merge | `gh api ...pulls/N/merge`, GitHub-specific PR review API |
| Worktree path | hard-coded `"github.com"` in `change_summary::resolve_options_for_jobset` |
| Identity | `(owner, repo)` pair without domain |

Each needs a platform-aware seam.

## Strategy

Introduce a `Platform` value (enum, not trait) that every persisted record
carries. Behind that, use a small trait for the *narrow* subset of operations
that genuinely differ — check posting, webhook signature verification, and
PR-merge actions. Most code paths (DB writes, BFS, change-summary classify,
render) are platform-neutral and stay as-is.

### `Platform` enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    GitHub,
    GitLab,
    Gitea,
}

impl Platform {
    pub fn default_domain(self) -> &'static str { ... }
    pub fn signature_header(self) -> &'static str { ... }
}
```

Stored in the DB as a `TEXT` column with a `CHECK` constraint.

### Trait for platform-narrow operations

```rust
#[async_trait]
pub trait PlatformOps: Send + Sync {
    async fn post_check_run(&self, ...) -> Result<CheckRunRef>;
    async fn update_check_run(&self, ...) -> Result<()>;
    async fn merge_pr(&self, ...) -> Result<()>;
    async fn fetch_repo_metadata(&self, ...) -> Result<RepoMeta>;
    fn verify_webhook_signature(&self, body: &[u8], headers: &HeaderMap) -> Result<()>;
}
```

Three impls: `GitHubOps` (octocrab), `GitLabOps` (custom HTTP, GitLab has no
direct Checks-API equivalent — use commit statuses + Markdown comments),
`GiteaOps` (similar to GitLab).

## Step-by-step plan

### 1. Schema unification

Migration `<date>_jobsets_platform_aware.sql`:

- Rename `GitHubJobSets` → `JobSets`.
- Add `platform TEXT NOT NULL DEFAULT 'github' CHECK (platform IN ('github','gitlab','gitea'))`.
- Add `domain TEXT NOT NULL DEFAULT 'github.com'`.
- Update existing rows to `('github', 'github.com')`.
- Drop the index on the old name; recreate.

Same treatment for `GitHubAppConfigs` (becomes per-platform credential rows).

Update every `sqlx::query` call site that references the old table name —
roughly 30 sites; mechanical.

### 2. Platform-aware identity types

In `src/git/types.rs` `GitRepo` already has a `domain` field — promote it to
first-class. Add `platform: Platform` to:

- `GitRepo`
- `CICheckInfo`
- The webhook ingress payload
- Anything that serializes a repo identity

### 3. `PlatformOps` trait + GitHub impl

Move existing `octocrab`-using code in `github/actions.rs` and the
checks-posting paths in `github/service.rs` behind `GitHubOps: PlatformOps`.
This is a refactor with zero behaviour change; keep the GitHub-specific
helper functions as private methods on the impl.

### 4. GitLab impl

Implement `GitLabOps` against the GitLab v4 REST API:

- **Auth**: project access tokens or OAuth2 (PAT for self-hosted; OAuth for
  gitlab.com). No App-style JWT.
- **Check equivalent**: commit statuses (`/projects/:id/statuses/:sha`)
  for green/red, plus a sticky merge-request comment for the markdown
  body. GitLab has no rich Checks UI, so the markdown lands as a comment.
- **Webhooks**: GitLab webhook bodies use header `X-Gitlab-Token` (shared
  secret, not HMAC). Signature verification = constant-time string compare.
- **PR merge**: `PUT /projects/:id/merge_requests/:iid/merge`.
- **Repo metadata**: `GET /projects/:id?statistics=false`.

New file: `src/gitlab/service.rs` mirroring `github/service.rs`. Webhook
ingress: `src/web.rs` gets a `/v1/webhooks/gitlab` route. Reuse the
existing `IngressTask` channel.

### 5. Gitea impl

Gitea's API is GitHub-compatible by design:

- **Auth**: PAT or OAuth2.
- **Check equivalent**: Gitea added Checks API support recently; if the
  installed version supports it, use it. Otherwise fall back to commit
  statuses + comments (same as GitLab).
- **Webhooks**: HMAC-SHA256 with `X-Gitea-Signature` header — payload
  shape is GitHub-compatible.
- **PR merge**: `POST /repos/:owner/:repo/pulls/:idx/merge`.

This impl is mostly a copy of `GitHubOps` with different base URLs and
header names. New file: `src/gitea/service.rs`.

### 6. Resolver platform-awareness

`change_summary::resolve_options_for_jobset`:

```rust
let row: Option<(String, String, String, String)> = sqlx::query_as(
    "SELECT platform, domain, owner, repo_name FROM JobSets WHERE sha = ? AND job = ?",
)...
```

Pass `domain` (and ignore `platform` here — the on-disk worktree layout is
already domain-keyed: `<root>/<domain>/<owner>/<repo>/...`).

### 7. Webhook routing

Single ingress endpoint per platform — shared dispatch logic, separate
signature verification:

```
POST /v1/webhooks/github   → GitHubOps::verify + GitHub event parser
POST /v1/webhooks/gitlab   → GitLabOps::verify + GitLab event parser
POST /v1/webhooks/gitea    → GiteaOps::verify + Gitea event parser
```

Shared `IngressTask` enum gains a `Platform` field; downstream services
dispatch on it.

### 8. Configuration

`config.toml` gains per-platform credential blocks:

```toml
[[github.app]]
app_id = 123
private_key_path = "..."

[[gitlab.host]]
url = "https://gitlab.example.com"
token_env = "EKACI_GITLAB_TOKEN"

[[gitea.host]]
url = "https://gitea.example.com"
token_env = "EKACI_GITEA_TOKEN"
```

`AppState` carries an `Arc<HashMap<(Platform, String), Box<dyn PlatformOps>>>`
keyed on `(platform, domain)`.

### 9. Tests

- Per-impl unit tests for signature verification (use canned payloads).
- Per-impl mocked check-posting (mockito server).
- End-to-end integration test for GitLab + Gitea mirroring the GitHub
  ingress test in `tests/web_integration.rs`.

### 10. Documentation

- Update `architecture.md` with the `Platform` seam.
- New `docs/gitlab-setup.md` mirroring `docs/github-app-setup.md`.
- New `docs/gitea-setup.md`.

## Out of scope (for now)

- **Bitbucket / Azure DevOps**. Same plan extends to them; defer until
  demand exists.
- **Cross-platform PR mirroring** (a PR opened on GitLab is reflected on
  GitHub). Unrelated and much larger.
- **Per-platform UX in the frontend**. The dashboard treats all platforms
  uniformly; that's fine for now.
- **Platform-specific merge-queue semantics**. Keep the existing logic;
  treat GitLab's "merge train" as a future enhancement on top of
  `MergeQueue` records.

## Risks

| Risk | Mitigation |
|---|---|
| GitLab has no rich Checks UI — output rendering differs | Document the comment-based fallback up front; users see a sticky markdown comment instead of a check tab |
| Self-hosted instance variance (GitLab CE vs EE, Gitea version drift) | Probe API capability at startup; degrade gracefully |
| Webhook signature semantics differ across platforms | Keep verification inside `PlatformOps::verify_webhook_signature`; never roll our own dispatch |
| `octocrab` is GitHub-only — must build GitLab/Gitea HTTP clients ourselves | Use `reqwest` with shared connection pool; small, focused client modules |
| Schema migration touches every `GitHubJobSets` query | Mechanical but high-risk; do as one focused PR with no other changes |

## Estimated effort

- Schema unification + GitHub trait extraction: 2–3 days.
- GitLab impl with statuses + sticky comment: 4–5 days.
- Gitea impl (mostly GitHub-shape clone): 2–3 days.
- Webhook routing + auth scaffolding: 2 days.
- Tests + docs: 2 days.

Total: ~2–3 weeks for one engineer; can parallelise GitLab and Gitea
once the trait + schema are in.

## Suggested ordering

1. Schema unification + `Platform` enum (lands a no-op migration).
2. Extract `PlatformOps` with GitHub as the only impl (refactor, zero
   behaviour change).
3. Gitea (smaller delta from GitHub) — proves the abstraction.
4. GitLab (largest delta — different check model).
5. Frontend updates if needed.
