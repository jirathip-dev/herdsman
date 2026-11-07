//! #630: workspace roots (temp folders here): a new lane takes the first mounted root, `hosts eject` saves and
//! restarts a lane on the next one, a pulled root's lane restarts once from its pushed head, and `migrate` moves an
//! idle `~/.herdr/worktrees` worktree into `<root>/<repo>/<lane>`. git is real; herdr, gh and the host probes are fakes.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, read, Event};
use herdsman::runner::{Out, Real, Runner};
use serde_json::json;
use std::path::{Path, PathBuf};

const AGENT: &str = "sendmeter-impl-12";

/// git runs for real; `herdr worktree create` is a real `git worktree add` of its branch; the rest is the fake.
struct Git<'a>(&'a Fake);

impl Runner for Git<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        if prog == "git" {
            self.0.calls.borrow_mut().push(format!("git {}", args.join(" ")));
            return Real.run(prog, args);
        }
        if let ["worktree", "create", "--cwd", local, "--branch", b, "--base", _, "--path", p, ..] = a[..] {
            git(Path::new(local), &["worktree", "add", p, b]);
            return Ok(Out {
                stdout: r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#.into(),
                ..Out::default()
            });
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs)
    }
}

fn git(dir: &Path, a: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(a)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {a:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// An origin with `staging` and a pushed `issue-12`, its clone (the fleet's `local`), and the two roots.
struct Repo {
    c: herdsman::Ctx,
    origin: PathBuf,
    local: PathBuf,
    nvme: PathBuf,
    internal: PathBuf,
}

fn repo(name: &str) -> Repo {
    let d = tmpdir(name);
    let (origin, local, nvme, internal) = (
        d.join("origin.git"),
        d.join("local"),
        d.join("nvme"),
        d.join("internal"),
    );
    for p in [&nvme, &internal] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare"]);
    git(
        &d,
        &["clone", "-q", &origin.to_string_lossy(), &local.to_string_lossy()],
    );
    git(&local, &["checkout", "-q", "-b", "staging"]);
    std::fs::write(local.join("README"), "x\n").unwrap();
    git(&local, &["add", "README"]);
    let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
    git(&local, &[&id[..], &["commit", "-q", "-m", "init"]].concat());
    git(&local, &["push", "-q", "origin", "staging", "staging:issue-12"]);
    git(&local, &["branch", "-q", "issue-12", "origin/issue-12"]);
    let roots = format!("roots = [\"{}\", \"{}\"]", nvme.display(), internal.display());
    let cfg = CONFIG
        .replace("harness  =  \"hermes\"", "harness = \"claude\"")
        .replace("/tmp/does-not-matter/sendmeter", &local.to_string_lossy())
        + &format!("\n[hosts.mini]\n{roots}\n");
    let c = ctx(&d, &cfg);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    Repo {
        c,
        origin,
        local,
        nvme,
        internal,
    }
}

/// herdr and gh as a spawn on this host needs them; the lane, once restarted, works in `cwd`.
fn fake(cwd: &Path) -> Fake {
    let wt = cwd.join("sendmeter/issue-12");
    Fake::default()
        .reply(
            "herdr agent list",
            &agents(&[(AGENT, "working", &wt.to_string_lossy(), "w9:p1")]),
        )
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("herdr pane close", "")
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr agent wait", "{}")
}

/// Issue 12's impl lane working in the first root (its worktree made there, a file left uncommitted).
fn working_on_nvme(r: &Repo) -> PathBuf {
    let wt = r.nvme.join("sendmeter/issue-12");
    git(&r.local, &["worktree", "add", "-q", &wt.to_string_lossy(), "issue-12"]);
    std::fs::write(wt.join("wip.txt"), "half done\n").unwrap();
    let d = json!({"role": "impl", "agent": AGENT, "harness": "claude", "pane": "w1:p1", "workspace": "w1",
                   "worktree": wt, "branch": "issue-12", "host": "mini", "root": r.nvme});
    append(&r.c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d)).unwrap();
    wt
}

fn spawns(c: &herdsman::Ctx) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "spawn")
        .collect()
}

#[test]
fn a_new_lane_lands_in_the_first_mounted_root_and_skips_a_missing_one() {
    let r = repo("roots-place");
    let f = fake(&r.internal);
    let worktree = |c: &herdsman::Ctx| {
        let (code, out) = run(c, &f, &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"]);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["worktree"].clone()
    };
    assert_eq!(worktree(&r.c), json!(r.nvme.join("sendmeter/issue-12")));
    std::fs::remove_dir(&r.nvme).unwrap(); // the drive is not plugged in
    assert_eq!(worktree(&r.c), json!(r.internal.join("sendmeter/issue-12")));
}

#[test]
fn eject_saves_and_restarts_a_live_lane_on_the_next_root_then_reports_safe() {
    let r = repo("roots-eject");
    let old = working_on_nvme(&r);
    let f = fake(&r.internal);
    let (code, out) = run(&r.c, &Git(&f), &["hosts", "eject", "mini"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert!(out.contains(&format!("safe to unplug {}", r.nvme.display())), "{out}");
    // Saved: the uncommitted file is on `wip/<agent>-<stamp>` at origin, and the worktree is off the drive.
    let wip = format!("wip/{AGENT}-1000000");
    assert!(git(&r.origin, &["show", &format!("{wip}:wip.txt")]).contains("half done"));
    assert!(
        !old.exists()
            && r.nvme
                .read_dir()
                .unwrap()
                .all(|e| e.unwrap().file_name() == "sendmeter")
    );
    // Restarted from that save on the next root.
    let new = r.internal.join("sendmeter/issue-12");
    let sp = spawns(&r.c);
    let last = sp.last().unwrap();
    assert_eq!((sp.len(), last.str("root")), (2, Some(&*r.internal.to_string_lossy())));
    assert_eq!(last.str("worktree"), Some(&*new.to_string_lossy()));
    assert_eq!(std::fs::read_to_string(new.join("wip.txt")).unwrap(), "half done\n");
    let list = git(&r.local, &["worktree", "list"]);
    assert!(
        list.contains(&*new.to_string_lossy()) && !list.contains(&*old.to_string_lossy()),
        "{list}"
    );
    // The ejected root takes no new lane while it stays mounted.
    let (_, dry) = run(&r.c, &f, &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"]);
    assert!(dry.contains(&*new.to_string_lossy()), "{dry}");
}

#[test]
fn a_pulled_root_is_noted_once_and_its_lane_restarts_on_the_next_root_from_its_pushed_head() {
    let r = repo("roots-pulled");
    working_on_nvme(&r);
    std::fs::remove_dir_all(&r.nvme).unwrap(); // pulled without eject
    let f = fake(&r.internal);
    let cfg = Config::load(&r.c.config).unwrap();
    let watch = || {
        let mut out = vec![];
        herdsman::roots::watch(&r.c, &cfg, &Git(&f), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    };
    let out = watch();
    let note = format!("! root {} gone", r.nvme.display());
    assert_eq!(out.matches(&note).count(), 1, "{out}");
    let sp = spawns(&r.c);
    assert_eq!((sp.len(), sp[1].str("root")), (2, Some(&*r.internal.to_string_lossy())));
    let new = r.internal.join("sendmeter/issue-12");
    let pushed = git(&r.origin, &["rev-parse", "issue-12"]);
    assert_eq!(git(&new, &["rev-parse", "HEAD"]), pushed, "from its pushed head");
    let ev = read(&r.c.state).unwrap();
    let moved = ev.iter().find(|e| e.kind == "root-moved").unwrap();
    assert!(
        moved
            .str("lost")
            .unwrap()
            .contains("uncommitted work, 0 commit(s) not pushed"),
        "{moved:?}"
    );
    // The next look: nothing new, no loop.
    assert_eq!(watch(), "");
    assert_eq!(spawns(&r.c).len(), 2);
    let gone = read(&r.c.state)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "root-gone")
        .count();
    assert_eq!(gone, 1);
}

#[test]
fn migrate_moves_an_idle_worktree_into_the_new_layout() {
    let r = repo("roots-migrate");
    let old = r.c.home.join(".herdr/worktrees/sendmeter");
    let (idle, live) = (old.join("issue-12"), old.join("issue-13"));
    git(
        &r.local,
        &["worktree", "add", "-q", &idle.to_string_lossy(), "issue-12"],
    );
    git(
        &r.local,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "issue-13",
            &live.to_string_lossy(),
            "staging",
        ],
    );
    let cache = r.c.home.join(".cache/herdsman/target/sendmeter");
    std::fs::create_dir_all(&cache).unwrap();
    let f = Fake::default().reply(
        "herdr agent list",
        &agents(&[("sendmeter-impl-13", "working", &live.to_string_lossy(), "w2:p1")]),
    );
    let (code, out) = run(&r.c, &Git(&f), &["migrate"]);
    assert_eq!(code, 0, "{out}");
    let to = r.c.worktrees.join("sendmeter/issue-12");
    assert!(to.join("README").is_file() && !idle.exists(), "{out}");
    let list = git(&r.local, &["worktree", "list"]);
    assert!(
        list.contains(&*to.to_string_lossy()) && !list.contains(&*idle.to_string_lossy()),
        "{list}"
    );
    assert!(live.exists() && out.contains("live, left for later"), "{out}");
    assert!(
        cache.exists(),
        "the old build caches stay while a worktree is left: {out}"
    );
    // Once that lane has ended, the next migrate moves it and drops the old caches.
    let (code, out) = run(
        &r.c,
        &Git(&Fake::default().reply("herdr agent list", NO_AGENTS)),
        &["migrate"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(
        r.c.worktrees.join("sendmeter/issue-13/README").is_file() && !cache.exists(),
        "{out}"
    );
}
