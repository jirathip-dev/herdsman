//! AC6: the event log survives kill -9 mid-write.
mod common;
use common::tmpdir;
use herdsman::events::{append, fold, read, Event};
use serde_json::json;
use std::io::Write;

fn ev(i: u64) -> Event {
    Event::new(i, "sendmeter", Some(7), "spawn", json!({"role": "impl", "agent": format!("a{i}"), "pad": "x".repeat(4000)}))
}

#[test]
fn torn_last_line_is_ignored_and_next_append_is_clean() {
    let d = tmpdir("torn");
    let p = d.join("events.jsonl");
    append(&p, &ev(1)).unwrap();
    append(&p, &ev(2)).unwrap();
    // Simulate a writer killed half-way through record 3.
    let line = serde_json::to_string(&ev(3)).unwrap();
    std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(&line.as_bytes()[..100]).unwrap();
    assert_eq!(read(&p).unwrap().len(), 2);
    append(&p, &ev(4)).unwrap();
    let all = read(&p).unwrap();
    assert_eq!(all.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![1, 2, 4]);
    assert_eq!(fold(&all)[&("sendmeter".to_string(), 7)].lane.as_deref(), Some("a4"));
}

/// Child mode: append forever until killed.
#[test]
fn kill9_child_writer() {
    let Ok(p) = std::env::var("FLEET_KILL_CHILD") else { return };
    let mut i = 0;
    loop {
        append(std::path::Path::new(&p), &ev(i)).unwrap();
        i += 1;
    }
}

#[test]
fn kill9_mid_append_never_corrupts() {
    let d = tmpdir("kill9");
    let p = d.join("events.jsonl");
    for round in 0..3 {
        let start = read(&p).unwrap().len();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "kill9_child_writer", "--nocapture"])
            .env("FLEET_KILL_CHILD", &p)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Wait (bounded) for the child's first record, then kill it mid-stream.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while read(&p).unwrap().len() <= start && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.kill().unwrap(); // SIGKILL
        child.wait().unwrap();
        let before = read(&p).unwrap();
        assert!(before.len() > start, "round {round}: child wrote nothing in 10 s");
        append(&p, &ev(999_000 + round)).unwrap();
        let after = read(&p).unwrap();
        assert_eq!(after.len(), before.len() + 1, "round {round}");
        assert_eq!(after.last().unwrap().ts, 999_000 + round);
    }
    // Every complete line in the file parses; at most the torn ones were dropped.
    let text = std::fs::read_to_string(&p).unwrap();
    let bad = text.lines().filter(|l| serde_json::from_str::<Event>(l).is_err()).count();
    assert!(bad <= 3, "only torn tails may be unparseable, got {bad}");
}

/// Child mode: read the log so the parent can inspect stderr.
#[test]
fn read_child() {
    let Ok(p) = std::env::var("FLEET_READ_CHILD") else { return };
    println!("count={}", read(std::path::Path::new(&p)).unwrap().len());
}

fn read_stderr(p: &std::path::Path) -> (String, String) {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "read_child", "--nocapture", "--test-threads=1"])
        .env("FLEET_READ_CHILD", p)
        .output()
        .unwrap();
    assert!(out.status.success());
    (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
}

#[test]
fn corrupt_middle_line_warns_torn_tail_is_silent() {
    let d = tmpdir("corrupt");
    let p = d.join("events.jsonl");
    let line = |i| serde_json::to_string(&ev(i)).unwrap();
    // Torn final line: silent.
    std::fs::write(&p, format!("{}\n{}\n{}", line(1), line(2), &line(3)[..100])).unwrap();
    let (out, err) = read_stderr(&p);
    assert!(out.contains("count=2"), "stdout: {out}");
    assert!(!err.contains("corrupt"), "torn tail must be silent: {err}");
    // Garbage on line 2 of 3 (plus a blank line): exactly one warning naming it.
    std::fs::write(&p, format!("{}\nnot json\n\n{}\n", line(1), line(2))).unwrap();
    let (out, err) = read_stderr(&p);
    assert!(out.contains("count=2"), "stdout: {out}");
    let warns: Vec<&str> = err.lines().filter(|l| l.contains("corrupt")).collect();
    assert_eq!(warns.len(), 1, "stderr: {err}");
    assert!(warns[0].contains("line 2:"), "{}", warns[0]);
}
