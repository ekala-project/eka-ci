#[derive(Debug, Clone)]
pub enum GitHubTask {
    CreateJobSet { commit: String, jobs: Vec<NixEvalDrv> },
}
/// This service will receive a repo checkout and determine what CI jobs need
/// to be ran.
///
/// In particular, this involves reading the content of the .ekaci/ directory,
/// and determining if there's legacy CI jobs or flake outputs
#[allow(dead_code)]
pub struct GitHubService {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    db_service: DbService,
    octocrab: Octocrab,
}

impl RepoReader {
    pub fn new(db_service: DbService) -> anyhow::Result<Self> {
        Ok(Self {
            db_service,
        })
    }

    pub fn run(
        self,
        eval_sender: mpsc::Sender<EvalTask>,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            cancel_token
                .run_until_cancelled(repo_tasks_loop(self.repo_receiver, eval_sender))
                .await;
        })
    }
}


