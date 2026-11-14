mod common;
use common::*;
use drover::config::Config;
use drover::events::{append, read, Event};
use drover::runner::{Out, Real, Runner};
use serde_json::json;
use std::path::{Path, PathBuf};

const AGENT: &str = "sendmeter-impl-12";

struct Git<'a>(&'a Fake);

impl Runner for Git<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if prog == "git" || prog == "cp" {
            self.0.calls.borrow_mut().push(format!("git {}", args.join(" ")));
            return Real.run(prog, args);
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

struct Repo {
    c: drover::Ctx,
    sh: FakeShed,
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
    let sh = FakeShed::new();
    sh.attach(&c);
    Repo {
        c,
        sh,
        origin,
        local,
        nvme,
        internal,
    }
}

fn fake(r: &Repo, cwd: &Path) -> Fake {
    let wt = cwd.join("sendmeter/issue-12");
    r.sh.lane(AGENT, "working", &wt.to_string_lossy());
    spawnable()
}

fn spawnable() -> Fake {
    Fake::default()
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("which shed", "/opt/shed/bin/shed\n")
}

fn working_on_nvme(r: &Repo) -> PathBuf {
    let wt = r.nvme.join("sendmeter/issue-12");
    git(&r.local, &["worktree", "add", "-q", &wt.to_string_lossy(), "issue-12"]);
    std::fs::write(wt.join("wip.txt"), "half done\n").unwrap();
    let d = json!({"role": "impl", "agent": AGENT, "harness": "claude", "pane": "w1:p1", "workspace": "w1",
                   "worktree": wt, "branch": "issue-12", "host": "mini", "root": r.nvme});
    append(&r.c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d)).unwrap();
    wt
}

fn spawns(c: &drover::Ctx) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == "spawn")
        .collect()
}

#[test]
fn a_new_lane_lands_in_the_first_mounted_root_and_skips_a_missing_one() {
    let r = repo("roots-place");
    let f = fake(&r, &r.internal);
    let worktree = |c: &drover::Ctx| {
        let (code, out) = run(c, &f, &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"]);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["worktree"].clone()
    };
    assert_eq!(worktree(&r.c), json!(r.nvme.join("sendmeter/issue-12")));
    std::fs::remove_dir(&r.nvme).unwrap();
    assert_eq!(worktree(&r.c), json!(r.internal.join("sendmeter/issue-12")));
}

#[test]
fn eject_saves_and_restarts_a_live_lane_on_the_next_root_then_reports_safe() {
    let r = repo("roots-eject");
    let old = working_on_nvme(&r);
    let f = fake(&r, old.parent().unwrap().parent().unwrap());
    let (code, out) = run(&r.c, &Git(&f), &["hosts", "eject", "mini"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert!(out.contains(&format!("safe to unplug {}", r.nvme.display())), "{out}");
    let saved = out.lines().find_map(|l| l.strip_prefix("saved "));
    let wip = saved.and_then(|l| l.rsplit(" to ").next()).unwrap();
    assert!(wip.starts_with(&format!("wip/{AGENT}-1000000-")), "{out}");
    assert!(git(&r.origin, &["show", &format!("{wip}:wip.txt")]).contains("half done"));
    assert!(
        !old.exists()
            && r.nvme
                .read_dir()
                .unwrap()
                .all(|e| e.unwrap().file_name() == "sendmeter")
    );
    let new = r.internal.join("sendmeter/issue-12");
    assert!(r.sh.called("kill", AGENT), "the live lane stopped first");
    assert_eq!(r.sh.last("run")["cwd"], json!(new), "and ran again there");
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
    let (_, dry) = run(&r.c, &f, &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"]);
    assert!(dry.contains(&*new.to_string_lossy()), "{dry}");
}

#[test]
fn a_pulled_root_is_noted_once_and_its_lane_restarts_on_the_next_root_from_its_pushed_head() {
    let r = repo("roots-pulled");
    working_on_nvme(&r);
    std::fs::remove_dir_all(&r.nvme).unwrap();
    let f = fake(&r, &r.nvme);
    let cfg = Config::load(&r.c.config).unwrap();
    let watch = || {
        let mut out = vec![];
        drover::roots::watch(&r.c, &cfg, &r.sh.on(&Git(&f)), &mut out).unwrap();
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
    assert_eq!(watch(), "");
    assert_eq!(spawns(&r.c).len(), 2);
    let gone = read(&r.c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "root-gone")
        .count();
    assert_eq!(gone, 1);
}

#[test]
fn a_new_lane_clones_its_fleets_warmest_build_on_its_root_and_gets_the_included_files() {
    let r = repo("roots-warm");
    let b = r.nvme.join(".build");
    for (d, f) in [
        ("sendmeter-impl-9", "old"),
        ("sendmeter-rev-11-r1", "warm"),
        ("other-impl-1", "theirs"),
    ] {
        std::fs::create_dir_all(b.join(d).join("release")).unwrap();
        std::fs::write(b.join(d).join("release").join(f), f).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }
    std::fs::write(r.local.join(".drover-ignored.env"), "PORT=1\n").unwrap();
    std::fs::write(r.local.join(".git/info/exclude"), ".drover-ignored.env\n").unwrap();
    std::fs::create_dir_all(r.local.join(".drover")).unwrap();
    std::fs::write(
        r.local.join(".drover/worktree-include"),
        "# setup\n.drover-ignored.env\n../escape\nmissing\n",
    )
    .unwrap();
    let f = spawnable();
    let (code, out) = run(&r.c, &Git(&f), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out}");
    let mine = b.join(AGENT).join("release");
    let ev = read(&r.c.state).unwrap();
    let cl = ev.iter().find(|e| e.kind == "build-cloned").unwrap();
    assert_eq!(cl.str("from"), Some(&*b.join("sendmeter-rev-11-r1").to_string_lossy()));
    if reflinks(&b) {
        assert!(mine.join("warm").is_file(), "{out}");
        assert!(!mine.join("old").exists() && !mine.join("theirs").exists());
    } else {
        assert!(!mine.join("warm").exists(), "{out}");
        assert_eq!(cl.val("errors").as_array().map(Vec::len), Some(1), "{out}");
    }
    let target = format!("{}'", b.join(AGENT).display());
    let sent = r.sh.sent(AGENT);
    assert!(
        sent.iter().any(|c| c.contains("TARGET_DIR=") && c.contains(&target)),
        "{target} {sent:?}"
    );
    let wt = r.nvme.join("sendmeter/issue-12");
    assert_eq!(
        std::fs::read_to_string(wt.join(".drover-ignored.env")).unwrap(),
        "PORT=1\n"
    );
    std::fs::write(wt.join("new.txt"), "x").unwrap();
    std::fs::write(wt.join("README"), "y\n").unwrap();
    let id = ["-c", "user.name=t", "-c", "user.email=t@t"];
    git(&wt, &[&id[..], &["commit", "-qam", "local"]].concat());
    let (_, st) = run(&r.c, &Git(&f), &["status"]);
    assert!(st.contains(" · +1 ⇡1"), "{st}");
    let cfg = Config::load(&r.c.config).unwrap();
    let a = drover::term::agents(&r.sh.on(&Git(&f))).unwrap();
    assert_eq!(a[0]["cwd"], json!(wt), "{a:?}");
    let st = drover::events::states(&r.c.state).unwrap();
    let ls = drover::lanes::join(&cfg, &Git(&f), &r.c.worktrees, &ev, &st, &a);
    let l = ls.iter().find(|l| l.name == AGENT).unwrap();
    let u = l.unsaved(&Git(&f));
    assert_eq!(
        (u, drover::spawn::unsaved_text(u).as_deref()),
        (Some((1, 1)), Some("+1 ⇡1"))
    );
}

fn reflinks(d: &std::path::Path) -> bool {
    let (src, dst) = (d.join(".probe"), d.join(".probe-clone"));
    std::fs::write(&src, "p").unwrap();
    let flag = if cfg!(target_os = "macos") {
        "-c"
    } else {
        "--reflink=always"
    };
    let ok = std::process::Command::new("cp")
        .arg(flag)
        .arg(&src)
        .arg(&dst)
        .status()
        .is_ok_and(|s| s.success());
    let _ = (std::fs::remove_file(&src), std::fs::remove_file(&dst));
    ok
}

#[test]
fn a_drive_pass_restarts_a_lane_whose_root_went_away_on_the_next_root_once() {
    let r = repo("roots-drive");
    working_on_nvme(&r);
    std::fs::remove_dir_all(&r.nvme).unwrap();
    let f = fake(&r, &r.internal);
    let (code, out) = run(&r.c, &Git(&f), &["drive"]);
    assert_eq!(code, 0, "{out}");
    let sp = spawns(&r.c);
    assert_eq!(
        (sp.len(), sp.last().unwrap().str("root")),
        (2, Some(&*r.internal.to_string_lossy())),
        "{out}"
    );
    let new = r.internal.join("sendmeter/issue-12");
    assert_eq!(
        git(&new, &["rev-parse", "HEAD"]),
        git(&r.origin, &["rev-parse", "issue-12"])
    );
    let (code, out) = run(&r.c, &Git(&f), &["drive"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(spawns(&r.c).len(), 2, "{out}");
    let ev = read(&r.c.state).unwrap();
    let moved: Vec<&Event> = ev.iter().filter(|e| e.kind == "root-moved").collect();
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0].str("root"), Some(&*r.nvme.to_string_lossy()));
    assert_eq!(moved[0].str("to"), Some(&*r.internal.to_string_lossy()));
    let (code, out) = run(&r.c, &Git(&f), &["waiting"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("sendmeter #12") && !out.contains("root gone"), "{out}");
}

#[test]
fn a_failed_restart_off_a_gone_root_is_retried_on_the_next_drive_pass() {
    let r = repo("roots-retry");
    working_on_nvme(&r);
    std::fs::remove_dir_all(&r.nvme).unwrap();
    let down = Fake::default().reply("which shed", "/opt/shed/bin/shed\n");
    let (code, out) = run(&r.c, &Git(&down), &["drive"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("{AGENT}: restart failed")), "{out}");
    let moved = |c: &drover::Ctx| {
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "root-moved")
            .count()
    };
    assert_eq!((spawns(&r.c).len(), moved(&r.c)), (1, 0), "{out}");
    let (code, out) = run(&r.c, &Git(&fake(&r, &r.internal)), &["drive"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!((spawns(&r.c).len(), moved(&r.c)), (2, 1), "{out}");
}

#[test]
fn a_second_rev_round_reuses_the_first_rounds_build_folder_and_only_the_first_clones() {
    let r = repo("roots-reuse");
    let b = r.nvme.join(".build");
    std::fs::create_dir_all(b.join("sendmeter-impl-12/release")).unwrap();
    std::fs::write(b.join("sendmeter-impl-12/release/warm"), "w").unwrap();
    git(&r.local, &["branch", "-q", "rev-12-r1", "origin/issue-12"]);
    git(&r.local, &["branch", "-q", "rev-12-r2", "origin/issue-12"]);
    let f = spawnable();
    let (code, out) = run(&r.c, &Git(&f), &["spawn", "sendmeter", "12", "rev"]);
    assert_eq!(code, 0, "{out}");
    let cp = |f: &Fake| {
        f.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("git -cR") || c.starts_with("git -R"))
            .count()
    };
    assert_eq!(cp(&f), 1, "the first round clones: {:?}", f.calls.borrow());
    let ev = read(&r.c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "build-cloned") && !ev.iter().any(|e| e.kind == "build-reused"));

    r.sh.exit("sendmeter-rev-12-r1");
    let r1 = b.join("sendmeter-rev-12-r1");
    std::fs::create_dir_all(r1.join("release")).unwrap();
    std::fs::write(r1.join("release/built"), "r1").unwrap();
    let verdict = json!({"verdict": "FAIL", "sha": "a".repeat(40), "body": "FAIL"});
    append(
        &r.c.state,
        &Event::new(r.c.now, "sendmeter", Some(12), "verdict", verdict),
    )
    .unwrap();
    let f = spawnable();
    let (code, out) = run(&r.c, &Git(&f), &["spawn", "sendmeter", "12", "rev"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(cp(&f), 0, "the second round copies nothing: {:?}", f.calls.borrow());
    let r2 = b.join("sendmeter-rev-12-r2");
    assert_eq!(std::fs::read_to_string(r2.join("release/built")).unwrap(), "r1");
    assert!(!r1.exists());
    let ev = read(&r.c.state).unwrap();
    let e = ev
        .iter()
        .find(|e| e.kind == "build-reused")
        .expect("build-reused logged");
    assert_eq!(e.str("from"), Some("sendmeter-rev-12-r1"));
    assert_eq!(*e.val("dirs"), json!([r2.display().to_string()]));
    assert_eq!(ev.iter().filter(|e| e.kind == "build-cloned").count(), 1);
}

#[test]
fn eject_of_a_hung_root_returns_at_once_skips_the_saves_and_restarts_on_the_next_root() {
    let r = repo("roots-eject-hung");
    let old = working_on_nvme(&r);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let dir = r.nvme.to_string_lossy().into_owned();
    assert!(drover::runner::hung(&dir, std::time::Duration::ZERO, move |_| {
        let _ = rx.recv();
        None
    }));
    let f = fake(&r, &r.nvme);
    let t = std::time::Instant::now();
    let (code, out) = run(&r.c, &Git(&f), &["hosts", "eject", "mini"]);
    assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert!(out.contains(&format!("not saved {}", old.display())), "{out}");
    let ev = read(&r.c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "root-eject"), "the eject is logged");
    let skipped: Vec<&Event> = ev.iter().filter(|e| e.kind == "wip-skipped").collect();
    assert_eq!(skipped.len(), 1, "{skipped:#?}");
    assert_eq!(skipped[0].str("agent"), Some(AGENT));
    let calls = f.calls.borrow().clone();
    let o = old.to_string_lossy();
    assert!(
        !calls.iter().any(|c| c.starts_with("git") && c.contains(&*o)),
        "{calls:#?}"
    );
    assert!(old.join("wip.txt").exists(), "unsaved work stays on the disk");
    let new = r.internal.join("sendmeter/issue-12");
    let last = spawns(&r.c).pop().unwrap();
    assert_eq!(last.str("root"), Some(&*r.internal.to_string_lossy()));
    assert_eq!(last.str("worktree"), Some(&*new.to_string_lossy()));
    assert!(new.join("README").exists(), "checked out from its branch");
    let moved = ev.iter().find(|e| e.kind == "root-moved").expect("root-moved");
    assert!(moved.str("lost").is_some_and(|l| l.contains("root hung")), "{moved:#?}");
    drop(tx);
}
