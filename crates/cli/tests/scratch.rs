//! #536: lanes' private scratch under `build_root`: drive deletes an ended lane's and trims the oldest over budget;
//! `clean` removes stray Cargo folders in the stray roots and a removed worktree's Claude temp dir.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, Event};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

fn ev(c: &herdsman::Ctx, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(10, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn spawn(c: &herdsman::Ctx, issue: u64, role: &str, agent: &str) {
    let w = c.worktrees.join("sendmeter").join(agent);
    std::fs::create_dir_all(&w).unwrap();
    let d = json!({"role": role, "agent": agent, "worktree": w, "pane": format!("{agent}:p1"), "workspace": agent});
    ev(c, issue, "spawn", d);
}

fn merged(c: &herdsman::Ctx, issue: u64) {
    ev(
        c,
        issue,
        "merged",
        json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
    );
}

/// `<dir>/<rel>` holding a 4096-byte blob.
fn blob(dir: &Path, rel: &str) -> PathBuf {
    let p = dir.join(rel);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("blob"), vec![0u8; 4096]).unwrap();
    p
}

/// `p` and its entries last modified at `at`.
fn age(p: &Path, at: u64) {
    for e in std::fs::read_dir(p).unwrap().flatten() {
        let f = std::fs::File::open(e.path()).unwrap();
        f.set_modified(UNIX_EPOCH + Duration::from_secs(at)).unwrap();
    }
    let f = std::fs::File::open(p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(at)).unwrap();
}

fn setup(name: &str, budget_bytes: u64) -> (herdsman::Ctx, PathBuf) {
    let d = tmpdir(name);
    let root = d.join("worktrees/.build"); // #630: the root's build folder
    let cfg = CONFIG.replace(
        "build_root = \"\"",
        &format!("build_budget_gb = {}", budget_bytes as f64 / 1_073_741_824.0),
    );
    (ctx(&d, &cfg), root.join(".scratch"))
}

fn budget(c: &herdsman::Ctx, r: &Fake) -> String {
    let mut out = vec![];
    herdsman::check::budget(c, &Config::load(&c.config).unwrap(), r, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

/// An ended lane's scratch goes at drive's pass (merged, superseded by a later round, or unknown); an open issue's
/// active lane, a live herdr agent and a building one keep theirs.
#[test]
fn drive_deletes_an_ended_lanes_scratch() {
    let (c, s) = setup("scratch-sweep", 1 << 40);
    spawn(&c, 12, "impl", "sendmeter-impl-12");
    merged(&c, 12);
    spawn(&c, 13, "impl", "sendmeter-impl-13");
    spawn(&c, 13, "rev", "sendmeter-rev-13-r1");
    spawn(&c, 13, "rev", "sendmeter-rev-13-r2");
    let dirs = [
        "sendmeter-impl-12",
        "sendmeter-rev-13-r1",
        "sendmeter-rev-13-r2",
        "sendmeter-impl-14",
        "sendmeter-rev-15-r1",
        "stray",
    ]
    .map(|a| {
        blob(&s, &format!("{a}/base/target/debug"))
            .ancestors()
            .nth(3)
            .unwrap()
            .to_path_buf()
    });
    let lock = s.join("sendmeter-rev-15-r1/base/target/debug/.cargo-lock");
    std::fs::write(&lock, "").unwrap();
    let held = std::fs::File::open(&lock).unwrap();
    held.lock().unwrap(); // a cargo build running there
    let r = Fake::default().reply("herdr agent list", &agents(&[("sendmeter-impl-14", "idle", "/x", "p")]));
    let out = budget(&c, &r);
    let gone = dirs.each_ref().map(|d| !d.exists());
    assert_eq!(
        gone,
        [true, true, false, false, false, true],
        "merged, superseded and unknown go; active, live and building stay: {out}"
    );
    assert!(out.contains("(lane ended)"), "{out}");
}

/// Over `build_budget_gb`, scratch dirs count and the oldest unlocked one goes first.
#[test]
fn the_oldest_scratch_is_trimmed_over_budget() {
    let (c, s) = setup("scratch-trim", 6000);
    spawn(&c, 13, "impl", "sendmeter-impl-13");
    let (old, new) = (blob(&s, "sendmeter-rev-9-r1"), blob(&s, "sendmeter-impl-13"));
    age(&old, 1_000);
    let r = Fake::default().reply(
        "herdr agent list",
        &agents(&[("sendmeter-rev-9-r1", "idle", "/x", "p")]),
    );
    let out = budget(&c, &r);
    assert!(!old.exists() && new.exists(), "{out}");
    assert!(out.contains("builds: trimmed"), "{out}");
}

/// `clean` lists stray Cargo folders older than a day in the stray roots; `--yes` removes them and the removed
/// worktree's Claude temp dir.
#[test]
fn clean_removes_stray_cargo_folders_and_claude_temp_dirs() {
    let d = tmpdir("scratch-clean");
    let tmp = d.join("tmp");
    let cfg = CONFIG.replace("stray_roots = []", &format!("stray_roots = [\"{}\"]", tmp.display()));
    let mut c = ctx(&d, &cfg);
    c.now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let old = c.now - 2 * 86_400;
    let cargo = |name: &str, marker: &str, at: u64| {
        let p = blob(&tmp, &format!("{name}/debug")).parent().unwrap().to_path_buf();
        std::fs::write(p.join(marker), "").unwrap();
        age(&p, at);
        p
    };
    let stray = cargo("t344-target", "CACHEDIR.TAG", old);
    let info = cargo("rev447t", ".rustc_info.json", old);
    let fresh = cargo("h370-base-target", "CACHEDIR.TAG", c.now);
    let building = cargo("rev502-target", "CACHEDIR.TAG", old);
    let lock = building.join("debug/.cargo-lock");
    std::fs::write(&lock, "").unwrap();
    let held = std::fs::File::open(&lock).unwrap();
    held.lock().unwrap();
    age(&building, old);
    let plain = blob(&tmp, "notes-target");
    age(&plain, old);

    spawn(&c, 12, "impl", "sendmeter-impl-12");
    merged(&c, 12);
    let wt = c
        .worktrees
        .join("sendmeter")
        .join("sendmeter-impl-12")
        .display()
        .to_string();
    let key: String = wt
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let claude = blob(&tmp, &format!("claude-501/{key}"));
    let other = blob(&tmp, "claude-501/-Users-me-elsewhere");

    let r = || {
        Fake::default()
            .reply("herdr agent list", NO_AGENTS)
            .reply(&format!("git -C {wt} rev-list"), "0\n")
            .reply("git -C", "")
            .reply("herdr", "{}")
    };
    let (code, dry) = run(&c, &r(), &["clean"]);
    assert_eq!(code, 0, "{dry}");
    for p in [&stray, &info] {
        assert!(
            dry.contains(&format!(
                "would remove stray build folder {}",
                p.canonicalize().unwrap().display()
            )),
            "{dry}"
        );
    }
    for p in [&fresh, &building, &plain] {
        assert!(
            !dry.contains(&p.file_name().unwrap().to_string_lossy().into_owned()),
            "{dry}"
        );
    }
    assert!(
        stray.exists() && info.exists() && claude.exists(),
        "a dry run removes nothing"
    );

    let (code, done) = run(&c, &r(), &["clean", "--yes"]);
    assert_eq!(code, 0, "{done}");
    assert!(!stray.exists() && !info.exists(), "{done}");
    assert!(fresh.exists() && building.exists() && plain.exists(), "{done}");
    assert!(done.contains(&format!("removed {wt}")), "{done}");
    assert!(
        !claude.exists() && other.exists(),
        "only the removed worktree's Claude temp dir goes"
    );
}
