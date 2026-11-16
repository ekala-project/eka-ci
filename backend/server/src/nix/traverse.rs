//! Drv-closure traversal for evaluated jobs.
//!
//! Before an evaluated job can be scheduled, every drv in its closure
//! needs a `Drv` row (system, features, FOD) and its dependency edges in
//! the DB and the build graph. Both come from `nix derivation show`: the
//! `inputDrvs` it prints are the edges. One call per [`BATCH_SIZE`] drvs
//! replaces the previous pair of subprocesses per drv (`nix derivation
//! show` + `nix-store --query --references`), which ran 300 processes at
//! once for every 150-drv chunk.

use std::collections::HashSet;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use tracing::{debug, info, warn};

use super::{DrvInfo, EvalService, NixEvalDrv};
use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::db::model::{Reference, Referrer};
use crate::graph::GraphCommand;

/// Drvs per `nix derivation show` / `nix-store --query --requisites`
/// call. Store paths are ~90 bytes, so argv stays around 45 KiB.
const BATCH_SIZE: usize = 500;

/// `nix derivation show` processes run at once.
const SHOW_CONCURRENCY: usize = 4;

/// How long `batch_traverse` waits for the graph to apply its insert.
/// The graph processes 30k drvs in ~2 min; past this the ingress
/// cascade picks the drvs up once the graph catches up.
const GRAPH_INSERT_WAIT: Duration = Duration::from_secs(300);

type ShowResult = std::result::Result<(DrvId, DrvInfo), (DrvId, anyhow::Error)>;

/// Drv rows and edges for a set of drvs, plus the drvs that could not
/// be read.
#[derive(Default)]
struct FetchedDrvs {
    drvs: Vec<Drv>,
    refs: Vec<(Referrer, Reference)>,
    failures: Vec<(DrvId, anyhow::Error)>,
}

impl EvalService {
    /// Traverse all evaluated drvs in a single batch: collect the union
    /// of their closures, read every new drv, insert into DB and graph in
    /// one shot, then update the LRU cache. Drvs shared between jobs are
    /// read once. Failures are logged and skipped per drv.
    pub(super) async fn batch_traverse(&self, jobs: &[NixEvalDrv]) -> Result<()> {
        let mut roots = Vec::new();
        for job in jobs {
            let Ok(drv_id) = DrvId::from_str(&job.drv_path) else {
                continue;
            };
            if !self.already_visited_drv(&drv_id).await {
                roots.push(drv_id);
            }
        }

        let closure = requisites_lenient(&roots).await;
        let new_drvids = self.uncached(closure).await;
        let fetched = fetch_drvs(&new_drvids, &self.db_service).await;
        for (drv, e) in &fetched.failures {
            warn!("Issue while traversing {}: {:#}", drv.store_path(), e);
        }
        if fetched.drvs.is_empty() {
            return Ok(());
        }

        info!(
            "Batch traverse: {} new drvs, {} refs",
            fetched.drvs.len(),
            fetched.refs.len()
        );
        self.record_drvs(fetched.drvs, fetched.refs, Some(GRAPH_INSERT_WAIT))
            .await
    }

    /// Traverse the closure of a single drv. Unlike `batch_traverse`, any
    /// drv that cannot be read fails the whole traversal.
    pub(super) async fn deep_traverse(&self, drv_path: &str) -> Result<()> {
        debug!("Traversing drv tree for {}", drv_path);
        let closure = requisites(&[drv_path.to_string()]).await?;
        let new_drvids = self.uncached(closure).await;
        debug!("Found {} new drvs", new_drvids.len());

        let mut fetched = fetch_drvs(&new_drvids, &self.db_service).await;
        if let Some((drv, e)) = fetched.failures.pop() {
            return Err(e.context(format!("failed to read {}", drv.store_path())));
        }
        // Don't wait for the graph: deep_traverse runs once per eval'd
        // drv and back-pressure on the 50k graph channel is enough.
        self.record_drvs(fetched.drvs, fetched.refs, None).await
    }

    /// Drop drvs that are already in the LRU cache.
    async fn uncached(&self, drvs: Vec<DrvId>) -> Vec<DrvId> {
        let mut drv_map = self.drv_map.lock().await;
        drvs.into_iter()
            .filter(|x| drv_map.get(x).is_none())
            .collect()
    }

    /// Persist drvs and edges to the DB and the graph, then cache them.
    /// `graph_wait` bounds how long to wait for the graph to apply the
    /// insert, so the ingress sees the edges before checking buildability;
    /// `None` does not wait.
    async fn record_drvs(
        &self,
        drvs: Vec<Drv>,
        refs: Vec<(Referrer, Reference)>,
        graph_wait: Option<Duration>,
    ) -> Result<()> {
        self.db_service
            .insert_drvs_and_references(&drvs, &refs)
            .await?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::InsertDrvs {
            drvs: crate::graph_compat::to_shared_drvs(&drvs)?,
            refs: refs
                .iter()
                .map(|(r, d)| {
                    Ok((
                        crate::graph_compat::to_shared_drv_id(r)?,
                        crate::graph_compat::to_shared_drv_id(d)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
            response: tx,
        };
        self.graph_command_sender.send(cmd).await?;
        if let Some(wait) = graph_wait {
            match tokio::time::timeout(wait, rx).await {
                Ok(Ok(())) => info!("Graph InsertDrvs completed for batch traverse"),
                Ok(Err(_)) => warn!("Graph InsertDrvs oneshot dropped"),
                Err(_) => warn!("Graph InsertDrvs timed out after {:?}, proceeding", wait),
            }
        }

        let mut drv_map = self.drv_map.lock().await;
        for drv in drvs {
            drv_map.put(drv.drv_path.clone(), drv);
        }
        Ok(())
    }
}

/// The union of the closures of `roots`, as drv ids.
async fn requisites(roots: &[String]) -> Result<Vec<DrvId>> {
    super::drv_requisites_many(roots)
        .await?
        .iter()
        .map(|s| DrvId::from_str(s))
        .collect::<Result<Vec<DrvId>, _>>()
        .context("failed to parse drv path from requisites output")
}

/// Like [`requisites`], batched, and skipping roots whose closure cannot
/// be read: a failed batch is retried root by root.
async fn requisites_lenient(roots: &[DrvId]) -> Vec<DrvId> {
    let mut closure = HashSet::new();
    for batch in roots.chunks(BATCH_SIZE) {
        let paths: Vec<String> = batch.iter().map(DrvId::store_path).collect();
        match requisites(&paths).await {
            Ok(drvs) => closure.extend(drvs),
            Err(e) => {
                debug!(
                    "requisites failed for a batch of {}, retrying each: {:#}",
                    paths.len(),
                    e
                );
                for path in paths {
                    match requisites(std::slice::from_ref(&path)).await {
                        Ok(drvs) => closure.extend(drvs),
                        Err(e) => warn!("Issue while traversing {} drv: {:?}", path, e),
                    }
                }
            },
        }
    }
    closure.into_iter().collect()
}

/// Read `drv_ids` with batched `nix derivation show` calls and turn the
/// output into `Drv` rows and edges. A drv already in the DB keeps its
/// row (and build state); only unseen drvs start out queued.
async fn fetch_drvs(drv_ids: &[DrvId], db: &DbService) -> FetchedDrvs {
    let shown: Vec<Vec<ShowResult>> = stream::iter(drv_ids.chunks(BATCH_SIZE))
        .map(show_batch)
        .buffer_unordered(SHOW_CONCURRENCY)
        .collect()
        .await;

    let mut fetched = FetchedDrvs::default();
    for result in shown.into_iter().flatten() {
        let (drv_id, info) = match result {
            Ok(shown) => shown,
            Err(failure) => {
                fetched.failures.push(failure);
                continue;
            },
        };
        let drv = match db.get_drv(&drv_id).await {
            Ok(Some(drv)) => drv,
            Ok(None) => Drv::from_info(drv_id.clone(), &info),
            Err(e) => {
                fetched.failures.push((drv_id, e));
                continue;
            },
        };
        fetched.drvs.push(drv);
        fetched.refs.extend(input_edges(&drv_id, &info));
    }
    fetched
}

/// `(drv, input drv)` edges from a drv's `inputDrvs`.
fn input_edges(drv_id: &DrvId, info: &DrvInfo) -> Vec<(Referrer, Reference)> {
    info.input_drvs
        .iter()
        .filter_map(|input| match DrvId::from_str(input) {
            Ok(dep) => Some((drv_id.clone(), dep)),
            Err(_) => {
                warn!(
                    "Ignoring invalid input drv {} of {}",
                    input,
                    drv_id.store_path()
                );
                None
            },
        })
        .collect()
}

/// Show one batch. Nix rejects the whole call if any drv in it is
/// unreadable, so a failed batch is split in half until the bad drvs
/// are isolated; the rest are still read.
fn show_batch(batch: &[DrvId]) -> BoxFuture<'_, Vec<ShowResult>> {
    Box::pin(async move {
        let paths: Vec<String> = batch.iter().map(DrvId::store_path).collect();
        match super::derivation_show::drv_outputs(&paths).await {
            Ok(infos) => infos.into_iter().filter_map(parse_shown).collect(),
            Err(e) if batch.len() == 1 => vec![Err((batch[0].clone(), e))],
            Err(e) => {
                debug!(
                    "derivation show failed for {} drvs, splitting: {:#}",
                    batch.len(),
                    e
                );
                let (left, right) = batch.split_at(batch.len() / 2);
                let mut results = show_batch(left).await;
                results.extend(show_batch(right).await);
                results
            },
        }
    })
}

fn parse_shown((key, info): (String, DrvInfo)) -> Option<ShowResult> {
    match DrvId::from_str(&key) {
        Ok(drv_id) => Some(Ok((drv_id, info))),
        Err(_) => {
            warn!("Ignoring unexpected drv {} from nix derivation show", key);
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    /// Instantiate a two-drv closure (`top` depends on `dep`) and return
    /// their store paths. Builders are never run.
    async fn instantiate_pair() -> (String, String) {
        let expr = r#"
            let
              dep = derivation { name = "traverse-dep"; system = "x86_64-linux"; builder = "/bin/sh"; };
            in {
              inherit dep;
              top = derivation { name = "traverse-top"; system = "x86_64-linux"; builder = "/bin/sh"; inherit dep; };
            }
        "#;
        let out = tokio::process::Command::new("nix-instantiate")
            .args(["-E", expr, "-A", "top", "-A", "dep"])
            .output()
            .await
            .expect("nix-instantiate");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        let mut lines = stdout.lines().map(str::to_string);
        (lines.next().unwrap(), lines.next().unwrap())
    }

    #[tokio::test]
    #[ignore] // Requires Nix to instantiate real derivations
    async fn fetch_drvs_reads_rows_and_edges() {
        let (top, dep) = instantiate_pair().await;
        let db = DbService::new_in_memory().await.unwrap();

        let closure = requisites(std::slice::from_ref(&top)).await.unwrap();
        assert_eq!(closure.len(), 2);

        let fetched = fetch_drvs(&closure, &db).await;
        assert!(fetched.failures.is_empty());
        assert_eq!(fetched.drvs.len(), 2);
        let top_id = DrvId::from_str(&top).unwrap();
        let dep_id = DrvId::from_str(&dep).unwrap();
        assert_eq!(fetched.refs, vec![(top_id, dep_id)]);
    }

    #[tokio::test]
    #[ignore] // Requires Nix to instantiate real derivations
    async fn fetch_drvs_isolates_unreadable_drvs() {
        let (top, dep) = instantiate_pair().await;
        let db = DbService::new_in_memory().await.unwrap();
        let missing =
            DrvId::from_str("/nix/store/00000000000000000000000000000000-missing.drv").unwrap();
        let ids = vec![
            DrvId::from_str(&top).unwrap(),
            missing.clone(),
            DrvId::from_str(&dep).unwrap(),
        ];

        let fetched = fetch_drvs(&ids, &db).await;
        assert_eq!(fetched.drvs.len(), 2);
        assert_eq!(fetched.failures.len(), 1);
        assert_eq!(fetched.failures[0].0, missing);
    }
}
