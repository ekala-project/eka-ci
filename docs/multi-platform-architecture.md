# Multi-Platform Architecture

## Overview

EkaCI supports multiple Git hosting platforms: **GitHub**, **GitLab**, and **Gitea**. The architecture is designed with intentional platform separation - each platform has its own database tables, service implementation, and webhook handlers, with shared business logic extracted into reusable modules.

## Design Principles

### 1. Platform Separation (Intentionally Not DRY)

Each platform maintains its own:
- **Database tables**: `GitHubJobSets`, `GitLabJobSets`, `GiteaJobSets` (separate tables, not normalized)
- **Service implementation**: `GitHubService`, `GitLabService`, `GiteaService`
- **Task enum**: `GitHubTask`, `GitLabTask`, `GiteaTask`
- **Webhook handlers**: Platform-specific event parsing and processing

**Rationale**: Platform APIs differ significantly in their event models, authentication mechanisms, and features. Separate implementations prevent coupling and allow each platform to evolve independently.

### 2. Shared Business Logic

Common functionality is extracted into platform-agnostic modules:
- **`JobsetData`**: Platform-agnostic jobset metadata struct
- **`change_summary`**: Rebuild impact analysis and change summarization
- **`auto_merge`**: Merge eligibility evaluation logic
- **`graph`**: In-memory build dependency graph

**Rationale**: Core CI logic (dependency tracking, build scheduling, impact analysis) is identical across platforms. Shared modules eliminate duplication while maintaining platform independence.

### 3. AsyncService Pattern

All platform services implement the `AsyncService<T>` trait:

```rust
pub trait AsyncService<T: std::fmt::Debug + 'static>: Sized
where
    T: Send,
    Self: Send + 'static,
{
    fn get_sender(&self) -> mpsc::Sender<T>;
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<T>>;
    fn handle_task(&self, task: T) -> impl Future<Output = Result<()>> + Send;
    fn handle_failure(&mut self, error: Error) -> impl Future<Output = ()> + Send;
    fn handle_closure(&mut self) -> impl Future<Output = ()> + Send;
}
```

Services use interior mutability (`Mutex<>`) to allow `&self` methods, enabling concurrent task processing.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                         Web Service                              │
│  ┌───────────────┐  ┌───────────────┐  ┌────────────────┐      │
│  │ /github       │  │ /gitlab       │  │ /gitea         │      │
│  │   /webhook    │  │   /webhook    │  │   /webhook     │      │
│  └───────┬───────┘  └───────┬───────┘  └────────┬───────┘      │
└──────────┼──────────────────┼───────────────────┼───────────────┘
           │                  │                   │
           ▼                  ▼                   ▼
  ┌────────────────┐  ┌────────────────┐  ┌────────────────┐
  │ GitHubService  │  │ GitLabService  │  │ GiteaService   │
  │ (AsyncService) │  │ (AsyncService) │  │ (AsyncService) │
  ├────────────────┤  ├────────────────┤  ├────────────────┤
  │ • Octocrab API │  │ • GitLab REST  │  │ • Gitea API    │
  │ • Check Runs   │  │ • Commit Status│  │ • Dual API*    │
  │ • PR comments  │  │ • MR comments  │  │ • PR comments  │
  └────────┬───────┘  └────────┬───────┘  └────────┬───────┘
           │                  │                   │
           ├──────────────────┼───────────────────┤
           │                  │                   │
           ▼                  ▼                   ▼
  ┌────────────────────────────────────────────────────┐
  │            Platform-Specific Tables                 │
  ├─────────────────┬──────────────────┬───────────────┤
  │ GitHubJobSets   │ GitLabJobSets    │ GiteaJobSets  │
  │ GitHubJob       │ GitLabJob        │ GiteaJob      │
  │ GitHub          │ GitLab           │ Gitea         │
  │ PullRequests    │ MergeRequests    │ PullRequests  │
  │ GitHubCheckRuns │ GitLabCommit     │ GiteaCheckRuns│
  │                 │ Statuses         │               │
  └─────────────────┴──────────────────┴───────────────┘
           │                  │                   │
           └──────────────────┴───────────────────┘
                              │
                              ▼
  ┌──────────────────────────────────────────────────────┐
  │           Shared Business Logic                      │
  ├──────────────────────────────────────────────────────┤
  │  • JobsetData (platform-agnostic metadata)           │
  │  • change_summary (rebuild impact analysis)          │
  │  • auto_merge (merge eligibility logic)              │
  │  • graph (dependency tracking)                       │
  │  • scheduler (build orchestration)                   │
  └──────────────────────────────────────────────────────┘

* Gitea: Newer versions use Check Runs API, older versions use Commit Statuses
```

## Data Flow

### Webhook → Job Creation

1. **Webhook received** at platform-specific endpoint
2. **Authentication** via platform-specific mechanism:
   - GitHub: HMAC-SHA256 signature (`X-Hub-Signature-256`)
   - GitLab: Token auth (`X-Gitlab-Token`)
   - Gitea: Token auth (`X-Gitea-Token`) or HMAC signature
3. **Event routing** to platform webhook handler
4. **Payload parsing** extracts metadata (owner, repo, commit, PR/MR number)
5. **Task creation**:
   - Create `GitHubTask::CreateJobSet` / `GitLabTask::CreateJobSet` / etc.
   - Send task to platform service via mpsc channel
6. **Service processes task**:
   - Insert jobset into platform-specific table
   - Trigger Nix evaluation
   - Create initial status checks

### Change Summary Generation

1. **Trigger**: PR/MR head commit jobset created
2. **JobsetData creation**: Platform service creates `JobsetData` with metadata
3. **Platform-agnostic functions**:
   ```rust
   let (opts, status) = resolve_options_from_jobset_data(&jobset_data, metrics).await;
   let summary = build_change_summary_from_jobset_ids(
       pool, graph, head_jobset_id, base_jobset_id,
       &jobset_data, base_sha, &opts, &status, metrics
   ).await?;
   ```
4. **Platform-specific posting**:
   - GitHub: Update check run with markdown annotation
   - GitLab: Post MR comment with summary
   - Gitea: Update check run (newer) or post PR comment (older)

## Database Schema

### Platform-Specific Tables

Each platform has its own set of tables to avoid cross-platform coupling:

**GitHub:**
- `GitHubJobSets`: Jobsets for GitHub repos
- `GitHubJob`: Individual jobs in a jobset
- `GitHubPullRequests`: PR metadata and merge state
- `GitHubCheckRuns`: Check run IDs for status updates

**GitLab:**
- `GitLabJobSets`: Jobsets for GitLab projects
- `GitLabJob`: Individual jobs in a jobset
- `GitLabMergeRequests`: MR metadata and merge state
- `GitLabCommitStatuses`: Commit status IDs for updates
- `GitLabProjects`: Project metadata (project_id, domain)

**Gitea:**
- `GiteaJobSets`: Jobsets for Gitea repos
- `GiteaJob`: Individual jobs in a jobset
- `GiteaPullRequests`: PR metadata and merge state
- `GiteaCheckRuns`: Check run IDs (for newer instances)
- `GiteaInstances`: Instance metadata (domain, version)
- `GiteaRepositories`: Repository metadata (owner, repo, domain)

### Shared Job Storage

The `Job` table stores platform-agnostic build job data and is referenced by all platform-specific `*Job` tables via foreign keys.

## Service Lifecycle

### Startup

```rust
// Create platform services
let github_service = GitHubService::new(db, octocrab, graph, metrics).await?;
let gitlab_service = GitLabService::new(db, graph, metrics).await?;
let gitea_service = GiteaService::new(db, graph, metrics).await?;

// Extract senders for passing to webhook handlers
let github_sender = Some(github_service.get_sender());
let gitlab_sender = Some(gitlab_service.get_sender());
let gitea_sender = Some(gitea_service.get_sender());

// Spawn service tasks
let github_handle = github_service.run(cancellation_token.clone());
let gitlab_handle = gitlab_service.run(cancellation_token.clone());
let gitea_handle = gitea_service.run(cancellation_token.clone());
```

### Task Processing

Each service runs an async event loop:

```rust
loop {
    tokio::select! {
        _ = cancel_token.cancelled() => {
            self.handle_closure().await;
            break;
        },
        Some(task) = receiver.recv() => {
            if let Err(e) = self.handle_task(task).await {
                self.handle_failure(e).await;
            }
        },
    }
}
```

### Shutdown

Graceful shutdown via `CancellationToken`:
1. Signal received (SIGTERM/SIGINT)
2. Cancellation token cancelled
3. Services complete current tasks
4. `handle_closure()` called for cleanup
5. All service tasks joined

## Platform-Specific Features

### GitHub
- **API**: Octocrab library (GitHub REST/GraphQL)
- **Status reporting**: Check Runs API
- **Merge**: Merge queue support, PR approvals
- **Auth**: GitHub Apps with JWT tokens

### GitLab
- **API**: GitLab REST API (TODO: implement client)
- **Status reporting**: Commit Status API
- **Merge**: MR approvals, project-level config
- **Auth**: Project/group access tokens
- **Self-hosted**: Domain-based multi-instance support

### Gitea
- **API**: GitHub-compatible API (TODO: implement client)
- **Status reporting**: Check Runs (v1.13+) or Commit Statuses (older)
- **Merge**: PR approvals, repo-level config
- **Auth**: Access tokens or webhook secrets
- **Self-hosted**: Domain-based multi-instance support
- **Version detection**: Automatic API feature detection

## Webhook Endpoints

### GitHub
- **Path**: `POST /github/webhook`
- **Authentication**: HMAC-SHA256 signature
- **Header**: `X-Hub-Signature-256: sha256=...`
- **Events**: `pull_request`, `push`, `issue_comment`, `pull_request_review`

### GitLab
- **Path**: `POST /gitlab/webhook`
- **Authentication**: Token-based
- **Header**: `X-Gitlab-Token: <secret>`
- **Events**: `Merge Request Hook`, `Push Hook`, `Note Hook`

### Gitea
- **Path**: `POST /gitea/webhook`
- **Authentication**: Token-based (TODO: HMAC-SHA256)
- **Header**: `X-Gitea-Token: <secret>`
- **Events**: `pull_request`, `push`, `issue_comment`, `pull_request_review`

## Extension Points

### Adding a New Platform

To add support for a new platform (e.g., Bitbucket):

1. **Create migration**: `sql/migrations/YYYYMMDD_bitbucket_tables.sql`
   ```sql
   CREATE TABLE BitbucketJobSets ( ... );
   CREATE TABLE BitbucketJob ( ... );
   CREATE TABLE BitbucketPullRequests ( ... );
   ```

2. **Define types**: `src/bitbucket/types.rs`
   ```rust
   pub struct BitbucketCIInfo { ... }
   pub enum BitbucketTask { ... }
   ```

3. **Implement service**: `src/bitbucket/service.rs`
   ```rust
   impl AsyncService<BitbucketTask> for BitbucketService { ... }
   ```

4. **Create webhook handler**: `src/bitbucket/webhook/mod.rs`
   ```rust
   pub async fn handle_webhook_payload(...) { ... }
   ```

5. **Add routes**: Update `src/web.rs`
   ```rust
   .nest("/bitbucket", bitbucket_routes())
   ```

6. **Initialize service**: Update `src/services/mod.rs`
   ```rust
   let bitbucket_service = BitbucketService::new(...).await?;
   let bitbucket_handle = bitbucket_service.run(cancellation_token.clone());
   ```

## Security

### Webhook Verification

All platforms verify webhook authenticity:
- **GitHub**: HMAC-SHA256 signature verification (implemented)
- **GitLab**: Token-based authentication (implemented)
- **Gitea**: Token-based authentication (HMAC TODO)

Verification can be disabled for development via `allow_insecure_webhooks=true` (not recommended for production).

### Rate Limiting

All webhook endpoints have per-IP rate limiting:
- **Rate**: 60 requests/second
- **Burst**: 100 requests
- **Response**: 429 Too Many Requests when exceeded

## Implementation Status

### Completed ✅
- Platform-specific database tables
- AsyncService trait and implementations
- Service initialization and lifecycle management
- Webhook routes and authentication
- Platform-agnostic change summary functions
- JobsetData abstraction
- Auto-merge eligibility logic extraction

### In Progress 🚧
- Webhook payload parsing (GitHub complete, GitLab/Gitea TODO)
- API client integrations (GitLab REST API, Gitea API)
- Status posting (GitHub complete, GitLab/Gitea TODO)

### Planned 📋
- Comment-based merge commands for GitLab/Gitea
- Multi-instance configuration UI
- Platform-specific metrics dashboards
- Comprehensive integration tests

## Future Platforms

Platforms under consideration:
- Azure DevOps
- Bitbucket Cloud
- Bitbucket Server
- Gerrit
