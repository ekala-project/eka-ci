//! The `direct-referrers` selection algorithm.
//!
//! 1. **Rebuilt** jobs are new attrs and jobs whose outputs differ from the
//!    base jobset (compared by output-path hash, falling back to the drv path
//!    for base rows recorded before output hashes existed).
//! 2. **Seeds** are rebuilt jobs that are new, whose `meta.position` file was
//!    edited, whose version changed, or that are dependency-graph roots: no
//!    other rebuilt job is among their direct `inputDrvs`.
//! 3. **Selected** jobs are the seeds plus every rebuilt job that lists a
//!    seed's drv among its direct `inputDrvs`, together with every attr that
//!    shares a drv with one of them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::{RebuildSelection, Seed, SeedReason, outputs_hash, position_file};
use crate::db::github::BaseJobOutputs;
use crate::nix::NixEvalDrv;

/// Everything the selection needs, gathered by the caller.
pub struct SelectionInput<'a> {
    /// Head evaluation results.
    pub jobs: &'a [NixEvalDrv],
    /// The base jobset's jobs.
    pub base: &'a [BaseJobOutputs],
    /// Repo-relative paths changed between the base and head commits.
    pub changed_files: &'a HashSet<String>,
    /// Worktree roots to strip from `meta.position` values.
    pub repo_roots: &'a [PathBuf],
}

type BaseIndex<'a> = HashMap<&'a str, &'a BaseJobOutputs>;

/// `/nix/store/<hash>-<name>.drv` → `<hash>-<name>.drv`, so full store paths
/// and `DrvId` strings compare equal.
fn store_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn input_drv_names(job: &NixEvalDrv) -> impl Iterator<Item = &str> {
    job.input_drvs
        .iter()
        .flat_map(|inputs| inputs.keys())
        .map(|path| store_name(path))
}

/// Pick the jobs to build, or explain why the strategy cannot be applied
/// (the caller then builds everything).
pub fn select_direct_referrers(input: &SelectionInput) -> Result<RebuildSelection, String> {
    let base = index_base(input.base)?;
    check_input_drvs(input.jobs)?;

    let rebuilt: Vec<&NixEvalDrv> = input
        .jobs
        .iter()
        .filter(|job| is_rebuilt(job, &base))
        .collect();
    let rebuilt_drvs: HashSet<&str> = rebuilt.iter().map(|j| store_name(&j.drv_path)).collect();

    let (seeds, seed_drvs) = find_seeds(&rebuilt, input, &base, &rebuilt_drvs);

    let mut selected_drvs = seed_drvs.clone();
    for job in &rebuilt {
        if input_drv_names(job).any(|dep| seed_drvs.contains(dep)) {
            selected_drvs.insert(store_name(&job.drv_path));
        }
    }

    let selected = input
        .jobs
        .iter()
        .filter(|job| selected_drvs.contains(store_name(&job.drv_path)))
        .map(|job| job.attr.clone())
        .collect();
    Ok(RebuildSelection {
        selected,
        seeds: sorted(seeds),
        rebuilt: rebuilt.len(),
    })
}

/// The seeds among the rebuilt jobs, and the drvs they cover.
fn find_seeds<'a>(
    rebuilt: &[&'a NixEvalDrv],
    input: &SelectionInput,
    base: &BaseIndex,
    rebuilt_drvs: &HashSet<&str>,
) -> (Vec<Seed>, HashSet<&'a str>) {
    let mut seeds = Vec::new();
    let mut seed_drvs = HashSet::new();
    for job in rebuilt {
        if let Some(reason) = seed_reason(job, input, base, rebuilt_drvs) {
            seed_drvs.insert(store_name(&job.drv_path));
            seeds.push(Seed {
                attr: job.attr.clone(),
                reason,
            });
        }
    }
    (seeds, seed_drvs)
}

fn index_base(base: &[BaseJobOutputs]) -> Result<BaseIndex<'_>, String> {
    if base.is_empty() {
        return Err("the base commit has no evaluated jobset for this job".to_string());
    }
    if base.iter().all(|job| job.outputs_hash.is_none()) {
        return Err("the base jobset was evaluated before output paths were recorded".to_string());
    }
    Ok(base.iter().map(|job| (job.name.as_str(), job)).collect())
}

fn check_input_drvs(jobs: &[NixEvalDrv]) -> Result<(), String> {
    if !jobs.is_empty() && jobs.iter().all(|job| job.input_drvs.is_none()) {
        return Err("the evaluation did not report inputDrvs".to_string());
    }
    Ok(())
}

fn is_rebuilt(job: &NixEvalDrv, base: &BaseIndex) -> bool {
    match base.get(job.attr.as_str()) {
        None => true,
        Some(base_job) => match &base_job.outputs_hash {
            Some(hash) => *hash != outputs_hash(&job.outputs),
            None => store_name(&base_job.drv_path) != store_name(&job.drv_path),
        },
    }
}

fn seed_reason(
    job: &NixEvalDrv,
    input: &SelectionInput,
    base: &BaseIndex,
    rebuilt_drvs: &HashSet<&str>,
) -> Option<SeedReason> {
    let Some(base_job) = base.get(job.attr.as_str()) else {
        return Some(SeedReason::New);
    };
    if is_edited(job, input) {
        return Some(SeedReason::Edited);
    }
    if version_changed(job, base_job) {
        return Some(SeedReason::VersionChanged);
    }
    let own = store_name(&job.drv_path);
    if !input_drv_names(job).any(|dep| dep != own && rebuilt_drvs.contains(dep)) {
        return Some(SeedReason::Root);
    }
    None
}

fn is_edited(job: &NixEvalDrv, input: &SelectionInput) -> bool {
    let position = job
        .meta
        .as_ref()
        .and_then(|meta| meta.position.as_deref())
        .or(job.pos.as_deref());
    position
        .and_then(|pos| position_file(pos, input.repo_roots))
        .is_some_and(|file| input.changed_files.contains(&file))
}

fn version_changed(job: &NixEvalDrv, base_job: &BaseJobOutputs) -> bool {
    match (base_job.version.as_deref(), job.version_or_heuristic()) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    }
}

fn sorted(mut seeds: Vec<Seed>) -> Vec<Seed> {
    seeds.sort_by(|a, b| a.attr.cmp(&b.attr));
    seeds
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
