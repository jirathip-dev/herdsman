//! #560: one UI audit at a time on the host: scripts/ui-lock.sh (ui-audit.sh and the WebKit tests' `swift` run under
//! it) holds ~/.cache/herdsman/ui-audit.lock; a second lane waits for it and says who holds it.
mod common;
use common::tmpdir;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

const LOCK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/ui-lock.sh");

/// `ui-lock.sh <args>` as lane `lane`, with its lock under `home`, its pipes open.
fn audit(home: &std::path::Path, lane: &str, args: &[&str]) -> Child {
    Command::new(LOCK)
        .args(args)
        .env("UI_AUDIT_LOCK", home.join(".cache/herdsman/ui-audit.lock"))
        .env("HERDSMAN_LANE", lane)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn line(r: impl std::io::Read) -> String {
    let mut l = String::new();
    BufReader::new(r).read_line(&mut l).unwrap();
    l
}

/// Lane `herdsman:1:impl`'s audit, holding the lock until its stdin closes.
fn holder(home: &std::path::Path) -> Child {
    let mut a = audit(home, "herdsman:1:impl", &["sh", "-c", "echo a-running; read x"]);
    assert_eq!(line(a.stdout.take().unwrap()), "a-running\n");
    a
}

#[test]
fn a_second_lanes_audit_waits_on_the_lock_until_the_first_ends() {
    let home = tmpdir("ui-lock");
    let lock = home.join(".cache/herdsman/ui-audit.lock");
    let mut a = holder(&home);
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), "herdsman:1:impl\n");
    let mut b = audit(&home, "herdsman:2:rev", &["echo", "b-ran"]);
    assert_eq!(
        line(b.stderr.take().unwrap()),
        "waiting for the UI audit lock (held by herdsman:1:impl)\n"
    );
    assert!(b.try_wait().unwrap().is_none(), "b runs only once a lets go");
    // a ends: b takes the lock and runs.
    a.stdin.take().unwrap().write_all(b"\n").unwrap();
    assert!(a.wait().unwrap().success());
    let out = b.wait_with_output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b-ran\n");
    // b ended too: a third audit runs at once.
    let c = audit(&home, "herdsman:3:impl", &["true"]).wait_with_output().unwrap();
    assert!(c.status.success() && c.stderr.is_empty(), "{c:?}");
}

#[test]
fn a_dead_holder_lets_go_at_once() {
    let home = tmpdir("ui-lock-dead");
    let mut a = holder(&home);
    a.kill().unwrap();
    a.wait().unwrap();
    let b = audit(&home, "herdsman:2:rev", &["true"]).wait_with_output().unwrap();
    assert!(b.status.success() && b.stderr.is_empty(), "{b:?}");
}

#[test]
fn a_lock_held_ten_minutes_is_run_past() {
    let home = tmpdir("ui-lock-stale");
    let lock = home.join(".cache/herdsman/ui-audit.lock");
    let mut a = holder(&home);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(601);
    let f = std::fs::File::options().write(true).open(&lock).unwrap();
    f.set_modified(old).unwrap();
    let out = audit(&home, "herdsman:2:rev", &["echo", "b-ran"])
        .wait_with_output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "b-ran\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err,
        "taking over the UI audit lock (held by herdsman:1:impl for 10 min)\n"
    );
    a.kill().unwrap();
    a.wait().unwrap();
}

#[test]
fn the_audits_run_under_the_lock() {
    let audit = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/ui-audit.sh")).unwrap();
    assert!(audit.contains(r#"exec "$(dirname "$0")/ui-lock.sh" "$0" "$@""#));
    let home = tmpdir("ui-lock-swift");
    let mut a = holder(&home);
    // The WebKit tests' `swift` waits on the same lock.
    let mut c = common::swift();
    let mut s = c
        .arg("--version")
        .env("UI_AUDIT_LOCK", home.join(".cache/herdsman/ui-audit.lock"))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert_eq!(
        line(s.stderr.take().unwrap()),
        "waiting for the UI audit lock (held by herdsman:1:impl)\n"
    );
    s.kill().unwrap();
    s.wait().unwrap();
    a.kill().unwrap();
    a.wait().unwrap();
}
