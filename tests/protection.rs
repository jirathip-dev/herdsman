//! #4: `herdsman protection` reads branch protection through the runner and never writes.
mod common;
use common::*;

const MAIN: &str = "gh api repos/jirathip-dev/sendmeter/branches/main/protection";
const STAGING: &str = "gh api repos/jirathip-dev/sendmeter/branches/staging/protection";
const PROTECTED: &str = r#"{"url":"x","required_status_checks":{"strict":true,"contexts":["ci"]},"required_pull_request_reviews":{"required_approving_review_count":1}}"#;
const NOT_PROTECTED: &str = r#"{"message":"Branch not protected","documentation_url":"https://docs.github.com","status":"404"}"#;
const NO_BRANCH: &str = r#"{"message":"Branch not found","documentation_url":"https://docs.github.com","status":"404"}"#;
const FREE_PLAN: &str = r#"{"message":"Upgrade to GitHub Pro or make this repository public to enable this feature.","documentation_url":"https://docs.github.com","status":"403"}"#;

fn err(stdout: &str, stderr: &str) -> herdsman::runner::Out {
    herdsman::runner::Out { code: 1, stdout: stdout.into(), stderr: stderr.into() }
}

fn with(main: herdsman::runner::Out, staging: herdsman::runner::Out) -> Fake {
    let mut r = Fake::default();
    r.replies.push((MAIN.into(), main));
    r.replies.push((STAGING.into(), staging));
    r
}

fn ok(body: &str) -> herdsman::runner::Out {
    herdsman::runner::Out { code: 0, stdout: body.into(), stderr: String::new() }
}

fn check(name: &str, r: &Fake) -> (i32, String) {
    let d = tmpdir(name);
    let c = ctx(&d, CONFIG);
    let res = run(&c, r, &["protection"]);
    // Read-only: exactly the two GETs, never a write or a repo/settings change.
    assert_eq!(*r.calls.borrow(), vec![MAIN.to_string(), STAGING.to_string()]);
    res
}

#[test]
fn all_protected_exits_zero() {
    let (code, out) = check("prot-all", &with(ok(PROTECTED), ok(PROTECTED)));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("jirathip-dev/sendmeter staging: ✓ protected (required status checks: yes, required PR reviews: yes)"), "{out}");
    assert!(out.contains("jirathip-dev/sendmeter main: ✓ protected"), "{out}");
}

#[test]
fn protected_staging_reports_missing_parts() {
    let (code, out) = check("prot-staging", &with(ok(PROTECTED), ok(r#"{"url":"x"}"#)));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("staging: ✓ protected (required status checks: no, required PR reviews: no)"), "{out}");
}

#[test]
fn unprotected_staging_exits_non_zero() {
    let r = with(ok(PROTECTED), err(NOT_PROTECTED, "gh: Branch not protected (HTTP 404)"));
    let (code, out) = check("prot-unprot", &r);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("jirathip-dev/sendmeter staging: ✗ not protected (HTTP 404)"), "{out}");
}

#[test]
fn private_free_plan_is_unavailable_with_status() {
    let e = || err(FREE_PLAN, "gh: Upgrade to GitHub Pro or make this repository public to enable this feature. (HTTP 403)");
    let (code, out) = check("prot-403", &with(e(), e()));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("staging: ✗ unavailable: Upgrade to GitHub Pro or make this repository public to enable this feature. (HTTP 403)"), "{out}");
}

#[test]
fn transient_failure_is_unavailable_never_bare_error() {
    let r = with(ok(PROTECTED), err("", "gh: Server Error (HTTP 502)"));
    let (code, out) = check("prot-502", &r);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("staging: ✗ unavailable: Server Error (HTTP 502)"), "{out}");

    let r = with(ok(PROTECTED), err("", "error connecting to api.github.com"));
    let (code, out) = check("prot-net", &r);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("staging: ✗ unavailable: no HTTP status (gh exit 1: error connecting to api.github.com)"), "{out}");
}

#[test]
fn repo_without_main_is_skipped() {
    let r = with(err(NO_BRANCH, "gh: Branch not found (HTTP 404)"), ok(PROTECTED));
    let (code, out) = check("prot-nomain", &r);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("jirathip-dev/sendmeter main: - no such branch (HTTP 404)"), "{out}");
}

#[test]
fn one_unprotected_among_fleets_exits_non_zero() {
    let cfg = format!("{CONFIG}\n[fleet.herdsman]\nrepo = \"jirathip-dev/herdsman\"\nlocal = \"/tmp/x\"\nintegration = \"integ\"\n");
    let d = tmpdir("prot-multi");
    let c = ctx(&d, &cfg);
    let r = Fake::default()
        .reply("gh api repos/jirathip-dev/sendmeter/", PROTECTED)
        .reply("gh api repos/jirathip-dev/herdsman/branches/main/", PROTECTED)
        .reply("gh api repos/jirathip-dev/herdsman/branches/staging/", PROTECTED);
    let (code, out) = run(&c, &r, &["protection"]);
    assert_eq!(code, 1, "{out}");
    // The integration branch is checked too; unscripted here, so it fails.
    assert!(out.contains("jirathip-dev/herdsman integ: ✗ unavailable: no HTTP status"), "{out}");
    assert_eq!(out.matches("✓ protected").count(), 4, "{out}");
    assert!(r.calls.borrow().iter().all(|c| c.starts_with("gh api repos/") && !c.contains("-X")), "{:?}", r.calls.borrow());
}
