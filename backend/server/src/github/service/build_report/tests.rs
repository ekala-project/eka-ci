//! Tests for rendering build reports.

use super::*;

const DRV: &str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";

fn hello() -> BuildReport {
    let mut report = BuildReport {
        package: Some("hello 2.12.1".to_string()),
        system: "x86_64-linux".to_string(),
        built_locally: true,
        duration_secs: Some(83),
        outputs: vec![
            OutputReport {
                name: "man".to_string(),
                path: "/nix/store/bbb-hello-2.12.1-man".to_string(),
                nar_size: Some(2048),
                closure_size: Some(2048),
                closure_paths: Some(1),
            },
            OutputReport {
                name: "out".to_string(),
                path: "/nix/store/aaa-hello-2.12.1".to_string(),
                nar_size: Some(256 * 1024),
                closure_size: Some(32 * 1024 * 1024),
                closure_paths: Some(6),
            },
        ],
        total_output_size: Some(258 * 1024),
        total_closure_size: Some(32 * 1024 * 1024 + 2048),
        comparisons: vec![],
    };
    report.sort_outputs();
    report
}

fn comparison(metric: SizeMetric, baseline: u64, current: u64) -> SizeComparison {
    SizeComparison {
        metric,
        base_branch: "master".to_string(),
        attr: "hello".to_string(),
        baseline_size: baseline,
        current_size: current,
        increase_percent: (current as f64 - baseline as f64) / baseline as f64 * 100.0,
        threshold_percent: 10.0,
    }
}

#[test]
fn success_shows_outputs_and_sizes() {
    let (title, summary) = hello().render(DRV);
    assert_eq!(title, "✅ Built in 1m 23s · closure 32.00 MB");
    assert!(
        summary.contains("| **Package** | hello 2.12.1 |"),
        "{summary}"
    );
    assert!(
        summary.contains("| **System** | `x86_64-linux` |"),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("| **Derivation** | `{DRV}` |")),
        "{summary}"
    );
    assert!(
        summary.contains("| **Output size** | 258.00 KB |"),
        "{summary}"
    );
    assert!(
        summary.contains("| out | `/nix/store/aaa-hello-2.12.1` | 256.00 KB | 32.00 MB | 6 |"),
        "{summary}"
    );
    // `out` is listed before the other outputs.
    let out = summary.find("| out |").unwrap();
    let man = summary.find("| man |").unwrap();
    assert!(out < man, "{summary}");
    assert!(!summary.contains("Compared with"), "{summary}");
}

#[test]
fn comparison_within_threshold_stays_success() {
    let mut report = hello();
    report.comparisons = vec![comparison(SizeMetric::Output, 1000, 1050)];
    assert!(report.warnings().is_empty());
    let (title, summary) = report.render(DRV);
    assert!(title.starts_with("✅"), "{title}");
    assert!(
        summary.contains("### Compared with base branch"),
        "{summary}"
    );
    assert!(
        summary.contains("| `hello` | Output size (`master`) | 1000 bytes | 1.03 KB | +50 bytes (+5.0%) | 10.0% |"),
        "{summary}"
    );
}

#[test]
fn exceeded_threshold_leads_with_the_warning() {
    let mut report = hello();
    report.comparisons = vec![
        comparison(SizeMetric::Output, 1000, 1050),
        comparison(SizeMetric::Closure, 1000, 2000),
    ];
    assert_eq!(report.warnings().len(), 1);
    let (title, summary) = report.render(DRV);
    assert!(
        title.starts_with("⚠️ Closure size increased by 100.0%"),
        "{title}"
    );
    let warning = summary.find("Size Comparison").unwrap();
    let outputs = summary.find("### Outputs").unwrap();
    assert!(warning < outputs, "{summary}");
    assert!(summary.contains("| ⚠️ Closure size"), "{summary}");
}

#[test]
fn substituted_build_without_sizes() {
    let mut report = hello();
    report.built_locally = false;
    report.duration_secs = None;
    report.total_output_size = None;
    report.total_closure_size = None;
    for output in &mut report.outputs {
        output.nar_size = None;
        output.closure_size = None;
        output.closure_paths = None;
    }
    let (title, summary) = report.render(DRV);
    assert_eq!(title, "✅ Substituted from cache");
    assert!(
        summary.contains("Substituted from a binary cache"),
        "{summary}"
    );
    assert!(
        summary.contains("| out | `/nix/store/aaa-hello-2.12.1` | – | – | – |"),
        "{summary}"
    );
    assert!(!summary.contains("**Output size**"), "{summary}");
}

#[test]
fn durations_are_readable() {
    assert_eq!(format_duration(0), "0s");
    assert_eq!(format_duration(42), "42s");
    assert_eq!(format_duration(185), "3m 05s");
    assert_eq!(format_duration(3720), "1h 02m");
    assert_eq!(format_duration(-5), "0s");
}

/// The report is journaled with the task; one with fields missing still
/// deserializes.
#[test]
fn journaled_report_tolerates_missing_fields() {
    let json = r#"{"ReportBuildSuccess":{
        "drv_id":"jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv",
        "report":{"outputs":[{"name":"out","path":"/nix/store/aaa-hello"}]}}}"#;
    let task: crate::github::GitHubTask = serde_json::from_str(json).unwrap();
    match task {
        crate::github::GitHubTask::ReportBuildSuccess { report, .. } => {
            assert_eq!(report.outputs[0].nar_size, None);
            assert!(!report.built_locally);
        },
        other => panic!("unexpected task {:?}", other),
    }
}
