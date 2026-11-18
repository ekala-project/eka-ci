use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::*;
use crate::nix::NixEvalMeta;

const ROOT: &str = "/ws/github.com/o/r/worktrees/head";

fn job(attr: &str, drv: &str, out: &str, inputs: &[&str]) -> NixEvalDrv {
    NixEvalDrv {
        attr: attr.to_string(),
        attr_path: attr.split('.').map(str::to_string).collect(),
        drv_path: format!("/nix/store/{drv}.drv"),
        input_drvs: Some(
            inputs
                .iter()
                .map(|i| (format!("/nix/store/{i}.drv"), vec!["out".to_string()]))
                .collect(),
        ),
        name: attr.to_string(),
        outputs: HashMap::from([("out".to_string(), format!("/nix/store/{out}"))]),
        system: "x86_64-linux".to_string(),
        meta: None,
        pos: None,
    }
}

fn with_meta(mut job: NixEvalDrv, position: Option<&str>, version: Option<&str>) -> NixEvalDrv {
    job.meta = Some(NixEvalMeta {
        position: position.map(|p| format!("{ROOT}/{p}:12")),
        version: version.map(str::to_string),
        ..Default::default()
    });
    job
}

fn base_of(job: &NixEvalDrv, version: Option<&str>) -> BaseJobOutputs {
    BaseJobOutputs {
        name: job.attr.clone(),
        drv_path: store_name(&job.drv_path).to_string(),
        outputs_hash: Some(outputs_hash(&job.outputs)),
        version: version.map(str::to_string),
    }
}

fn select(
    head: &[NixEvalDrv],
    base: &[BaseJobOutputs],
    changed: &[&str],
) -> Result<RebuildSelection, String> {
    let changed_files: HashSet<String> = changed.iter().map(|f| f.to_string()).collect();
    let roots = vec![PathBuf::from(ROOT)];
    select_direct_referrers(&SelectionInput {
        jobs: head,
        base,
        changed_files: &changed_files,
        repo_roots: &roots,
    })
}

fn names(set: &HashSet<String>) -> Vec<&str> {
    let mut v: Vec<&str> = set.iter().map(String::as_str).collect();
    v.sort();
    v
}

/// openssl gets a version bump; curl, git and python consume it directly,
/// requests only through python, zlib and hello are unaffected.
fn openssl_world() -> (Vec<BaseJobOutputs>, Vec<NixEvalDrv>) {
    let base = vec![
        base_of(&job("openssl", "o1", "O1", &["z1"]), Some("3.0.1")),
        base_of(&job("curl", "c1", "C1", &["o1", "z1"]), None),
        base_of(&job("git", "g1", "G1", &["c1", "o1"]), None),
        base_of(&job("python", "p1", "P1", &["o1"]), None),
        base_of(&job("requests", "r1", "R1", &["p1"]), None),
        base_of(&job("zlib", "z1", "Z1", &[]), None),
        base_of(&job("hello", "h1", "H1", &[]), None),
    ];
    let head = vec![
        with_meta(
            job("openssl", "o2", "O2", &["z1"]),
            Some("pkgs/openssl/default.nix"),
            Some("3.0.2"),
        ),
        job("curl", "c2", "C2", &["o2", "z1"]),
        job("git", "g2", "G2", &["c2", "o2"]),
        job("python", "p2", "P2", &["o2"]),
        job("requests", "r2", "R2", &["p2"]),
        job("zlib", "z1", "Z1", &[]),
        job("hello", "h1", "H1", &[]),
    ];
    (base, head)
}

#[test]
fn builds_updated_package_and_its_direct_referrers() {
    let (base, head) = openssl_world();
    let selection = select(&head, &base, &["pkgs/openssl/default.nix"]).unwrap();

    assert_eq!(selection.rebuilt, 5);
    assert_eq!(
        names(&selection.selected),
        ["curl", "git", "openssl", "python"]
    );
    assert_eq!(
        selection.seeds,
        vec![Seed {
            attr: "openssl".to_string(),
            reason: SeedReason::Edited,
        }]
    );
}

#[test]
fn graph_root_is_a_seed_without_edits() {
    // The change lives in a non-job file (e.g. a shared builder), so only
    // the dependency graph identifies openssl as the origin.
    let (base, mut head) = openssl_world();
    head[0].meta = None;
    let selection = select(&head, &base, &["pkgs/build-support/x.nix"]).unwrap();

    assert_eq!(selection.seeds.len(), 1);
    assert_eq!(selection.seeds[0].reason, SeedReason::Root);
    assert_eq!(
        names(&selection.selected),
        ["curl", "git", "openssl", "python"]
    );
}

#[test]
fn co_edited_package_is_seeded() {
    let (base, mut head) = openssl_world();
    head[4] = with_meta(
        job("requests", "r2", "R2", &["p2"]),
        Some("pkgs/requests/default.nix"),
        None,
    );
    let selection = select(
        &head,
        &base,
        &["pkgs/openssl/default.nix", "pkgs/requests/default.nix"],
    )
    .unwrap();

    assert!(selection.selected.contains("requests"));
    assert_eq!(selection.seeds.len(), 2);
}

#[test]
fn version_change_is_a_seed() {
    let (mut base, mut head) = openssl_world();
    base[3] = base_of(&job("python", "p1", "P1", &["o1"]), Some("3.12.0"));
    head[3] = with_meta(job("python", "p2", "P2", &["o2"]), None, Some("3.12.1"));
    // requests now refers to a seed
    let selection = select(&head, &base, &["pkgs/openssl/default.nix"]).unwrap();

    let python = selection.seeds.iter().find(|s| s.attr == "python").unwrap();
    assert_eq!(python.reason, SeedReason::VersionChanged);
    assert!(selection.selected.contains("requests"));
}

#[test]
fn new_attrs_are_seeds() {
    let (base, mut head) = openssl_world();
    head.push(job("newpkg", "n1", "N1", &["o2"]));
    let selection = select(&head, &base, &["pkgs/openssl/default.nix"]).unwrap();

    let new = selection.seeds.iter().find(|s| s.attr == "newpkg").unwrap();
    assert_eq!(new.reason, SeedReason::New);
    assert!(selection.selected.contains("newpkg"));
}

#[test]
fn drv_churn_with_identical_outputs_is_not_rebuilt() {
    // curl's drv path changed (e.g. a fetcher FOD changed) but its outputs
    // did not: nothing needs building.
    let base = vec![base_of(&job("curl", "c1", "C1", &["f1"]), None)];
    let head = vec![job("curl", "c2", "C1", &["f2"])];
    let selection = select(&head, &base, &[]).unwrap();

    assert_eq!(selection.rebuilt, 0);
    assert!(selection.selected.is_empty());
}

#[test]
fn aliases_sharing_a_drv_are_selected_together() {
    let (mut base, mut head) = openssl_world();
    base.push(base_of(&job("openssl_3", "o1", "O1", &["z1"]), None));
    head.push(job("openssl_3", "o2", "O2", &["z1"]));
    let selection = select(&head, &base, &["pkgs/openssl/default.nix"]).unwrap();

    assert!(selection.selected.contains("openssl_3"));
    assert!(!selection.selected.contains("requests"));
}

#[test]
fn base_rows_without_hash_compare_drv_paths() {
    let (mut base, head) = openssl_world();
    base[5].outputs_hash = None; // zlib: same drv, unchanged
    base[4].outputs_hash = None; // requests: drv changed, rebuilt
    let selection = select(&head, &base, &["pkgs/openssl/default.nix"]).unwrap();

    assert_eq!(selection.rebuilt, 5);
    assert!(!selection.selected.contains("zlib"));
}

#[test]
fn falls_back_without_usable_base_or_input_drvs() {
    let (base, mut head) = openssl_world();
    assert!(select(&head, &[], &[]).is_err());

    let hashless: Vec<BaseJobOutputs> = base
        .iter()
        .cloned()
        .map(|mut b| {
            b.outputs_hash = None;
            b
        })
        .collect();
    assert!(select(&head, &hashless, &[]).is_err());

    for job in &mut head {
        job.input_drvs = None;
    }
    assert!(select(&head, &base, &[]).is_err());
}
