//! #361: `herdsman reclaim`: preview by default; only rebuildable folders, only in worktrees idle 7 days.
mod common;
use common::*;
use herdsman::events::{append, Event};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

/// A lane's spawn event and its worktree, with `target/`, `node_modules/`, `.cache/`, `build/` and `src/` in it.
fn lane(c: &herdsman::Ctx, issue: u64) -> PathBuf {
    let w = c.worktrees.join("sendmeter").join(format!("issue-{issue}"));
    for d in ["target", "node_modules", ".cache", "build", "src"] {
        std::fs::create_dir_all(w.join(d)).unwrap();
        std::fs::write(w.join(d).join("blob"), vec![0u8; 4096]).unwrap();
    }
    let agent = format!("sendmeter-impl-{issue}");
    let d = json!({"role": "impl", "agent": agent, "worktree": w, "pane": "p1"});
    append(&c.state, &Event::new(10, "sendmeter", Some(issue), "spawn", d)).unwrap();
    w
}

fn touch(p: &Path, at: u64) {
    let f = std::fs::File::open(p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(at)).unwrap();
}

/// HEAD last moved `head_at`; `status` is git's porcelain -z output.
fn git(r: Fake, w: &Path, head_at: u64, status: &str) -> Fake {
    let w = w.display();
    r.reply(&format!("git -C {w} log -g"), &format!("HEAD@{{{head_at}}}\n"))
        .reply(&format!("git -C {w} status"), status)
}

fn setup(name: &str) -> herdsman::Ctx {
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "build_dirs = [\"build\"]\n\n[fleet.sendmeter.role.impl]",
    );
    let mut c = ctx(&tmpdir(name), &cfg);
    c.now = NOW;
    c
}

fn gone(w: &Path) -> Vec<bool> {
    ["target", "node_modules", ".cache", "build", "src"]
        .map(|d| !w.join(d).exists())
        .to_vec()
}

#[test]
fn preview_changes_nothing_and_apply_removes_only_build_folders() {
    let c = setup("reclaim-apply");
    let w = lane(&c, 12);
    let old = NOW - 8 * DAY;
    std::fs::write(w.join("notes.md"), "x").unwrap();
    touch(&w.join("notes.md"), old);
    touch(&w, NOW); // the worktree folder's own mtime is no edit: the tests hold whatever the real clock says
    let r = || {
        git(Fake::default(), &w, old, "?? notes.md\0")
            .reply("herdr agent list", NO_AGENTS)
            .reply("lsof", "")
            .reply("git -C", "")
    };
    let (code, out) = run(&c, &r(), &["reclaim"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would remove") && out.contains("nothing changed"), "{out}");
    assert!(out.contains("APFS clones may free less"), "{out}");
    assert_eq!(gone(&w), [false; 5], "preview removes nothing");

    let (code, out) = run(&c, &r(), &["reclaim", "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("freed"), "{out}");
    assert_eq!(gone(&w), [true, true, true, true, false], "src stays");
    assert!(w.join("notes.md").exists(), "untracked work stays");
}

/// A `y` on stdin applies the preview.
#[test]
fn a_y_answer_applies() {
    let c = setup("reclaim-y");
    let w = lane(&c, 12);
    touch(&w, NOW);
    let r = git(Fake::default(), &w, NOW - 8 * DAY, "")
        .reply("herdr agent list", NO_AGENTS)
        .reply("lsof", "")
        .reply("git -C", "");
    let mut out = Vec::new();
    let argv = ["herdsman", "reclaim"].map(String::from).to_vec();
    let code = herdsman::main_with(argv, &c, &r, &mut out, &mut &b"y\n"[..]);
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&out));
    assert_eq!(gone(&w), [true, true, true, true, false]);
}

/// A worktree with a process in it, a live lane, a recent edit or deletion or a recent HEAD move keeps its build folders.
#[test]
fn busy_or_recent_worktrees_are_skipped() {
    let c = setup("reclaim-skip");
    let (proc_wt, live, edited, moved) = (lane(&c, 1), lane(&c, 2), lane(&c, 3), lane(&c, 4));
    let deleted = lane(&c, 5);
    let old = NOW - 8 * DAY;
    std::fs::write(edited.join("src/lib.rs"), "x").unwrap();
    touch(&edited.join("src/lib.rs"), NOW - DAY);
    touch(&deleted.join("src"), NOW - DAY); // the folder a deletion bumps
    let lsof = format!("p42\ncnode\nn{}/src\n", proc_wt.display());
    let r = git(Fake::default(), &proc_wt, old, "");
    let r = git(r, &live, old, "");
    let r = git(r, &edited, old, " M src/lib.rs\0");
    let r = git(r, &deleted, old, " D src/gone.rs\0");
    let r = git(r, &moved, NOW - 2 * DAY, "")
        .reply(
            "herdr agent list",
            &agents(&[("sendmeter-impl-2", "idle", "/elsewhere", "p1")]),
        )
        .reply("lsof", &lsof)
        .reply("git -C", "");
    let (code, out) = run(&c, &r, &["reclaim", "--apply"]);
    assert_eq!(code, 0, "{out}");
    for (w, why) in [
        (&proc_wt, "a process runs there"),
        (&live, "a live lane"),
        (&edited, "touched"),
        (&deleted, "touched"),
        (&moved, "touched"),
    ] {
        assert!(out.contains(&format!("skip {}: {why}", w.display())), "{out}");
        assert_eq!(gone(w), [false; 5], "{}", w.display());
    }
    assert!(out.contains("0.00 GB freed"), "{out}");
}

/// #438: over `build_budget_gb` the shared build folder's profiles go, but never one cargo holds locked.
#[test]
fn the_shared_build_folder_is_trimmed_past_its_budget_except_a_building_profile() {
    let d = tmpdir("reclaim-shared");
    let root = d.join("worktrees/.build"); // #630: the root's build folder
    let cfg = CONFIG.replace("build_root = \"\"", "build_budget_gb = 0");
    let c = ctx(&d, &cfg);
    let profile = |p: &str| {
        let dir = root.join(p);
        std::fs::create_dir_all(dir.join("deps")).unwrap();
        std::fs::write(dir.join("deps/blob"), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.join(".cargo-lock"), "").unwrap();
        dir
    };
    let (building, idle) = (profile("sendmeter/debug"), profile("other/debug"));
    let held = std::fs::File::open(building.join(".cargo-lock")).unwrap();
    held.lock().unwrap(); // a cargo build running there
    let r = || Fake::default().reply("herdr agent list", NO_AGENTS).reply("lsof", "");
    let (code, out) = run(&c, &r(), &["reclaim"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("would trim") && idle.exists(),
        "preview removes nothing: {out}"
    );
    let (code, out) = run(&c, &r(), &["reclaim", "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(!idle.exists(), "{out}");
    assert!(
        building.join("deps/blob").exists() && out.contains("cargo is building"),
        "{out}"
    );
}
