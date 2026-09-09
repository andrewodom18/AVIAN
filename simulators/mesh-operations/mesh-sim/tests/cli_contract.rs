use std::process::Command;

fn run(args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_mesh-sim"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn summary_and_saved_reports_retain_deterministic_evidence() {
    let first = run(&["--validate", "--summary", "--seed", "42"]);
    let repeated = run(&["--validate", "--summary", "--seed", "42"]);
    assert_eq!(first["passed"], true);
    assert_eq!(first, repeated);
    for scenario in first["scenarios"].as_array().unwrap() {
        assert!(scenario["events"].as_array().unwrap().is_empty());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/report.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mesh-sim"))
        .args(["--validate", "--summary", "--seed", "42", "--output"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved, first);
}

#[test]
fn reference_trace_and_real_loopback_modes_are_executable() {
    assert!(run(&[]).is_object());
    assert!(run(&["--trace"])["steps"]
        .as_array()
        .is_some_and(|steps| !steps.is_empty()));
    let report = run(&["--validate-peat"]);
    let rows = report.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        assert_eq!(row["passed"], true);
        assert_eq!(row["hardware_validated"], false);
        assert_eq!(row["nodes"], row["converged_nodes"]);
    }
}
