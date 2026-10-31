//! `gate::ci_green`, moved out of `src/gate.rs`.
use herdsman::gate::ci_green;
use serde_json::{json, Value};

fn run(name: &str, conclusion: &str, started: &str) -> Value {
    json!({"__typename": "CheckRun", "name": name, "status": "COMPLETED", "conclusion": conclusion, "startedAt": started})
}
fn req() -> Vec<String> {
    vec!["quality".into()]
}

#[test]
fn required_quality_ignores_narrowed_failure() {
    let r = json!([run("quality", "SUCCESS", "2026-10-31T10:00:00Z"), run("quality-narrowed", "FAILURE", "2026-10-31T09:00:00Z")]);
    assert_eq!(ci_green(&r, &req()), Ok(()));
}

#[test]
fn required_quality_missing_is_refused() {
    let r = json!([run("quality-narrowed", "SUCCESS", "2026-10-31T09:00:00Z")]);
    assert_eq!(ci_green(&r, &req()), Err("required check `quality` is missing".into()));
    assert_eq!(ci_green(&json!([]), &req()), Err("required check `quality` is missing".into()));
}

#[test]
fn newer_success_supersedes_older_failure() {
    let r = json!([run("quality", "SUCCESS", "2026-10-31T10:00:00Z"), run("quality", "CANCELLED", "2026-10-31T09:00:00Z")]);
    assert_eq!(ci_green(&r, &req()), Ok(()));
}

#[test]
fn newer_failure_is_refused() {
    let r = json!([run("quality", "SUCCESS", "2026-10-31T09:00:00Z"), run("quality", "FAILURE", "2026-10-31T10:00:00Z")]);
    assert_eq!(ci_green(&r, &req()), Err("required check `quality` is failed".into()));
}

#[test]
fn pending_required_is_refused() {
    let r = json!([{"__typename": "CheckRun", "name": "quality", "status": "IN_PROGRESS", "startedAt": "2026-10-31T10:00:00Z"}]);
    assert_eq!(ci_green(&r, &req()), Err("required check `quality` is pending".into()));
}

#[test]
fn unset_checks_every_check() {
    let r = json!([run("quality", "SUCCESS", "2026-10-31T10:00:00Z"), run("quality-narrowed", "FAILURE", "2026-10-31T09:00:00Z")]);
    assert_eq!(ci_green(&r, &[]), Err("check `quality-narrowed` is failed".into()));
    assert_eq!(ci_green(&json!([run("quality", "SUCCESS", "")]), &[]), Ok(()));
    assert_eq!(ci_green(&json!([]), &[]), Err("no checks reported".into()));
}
