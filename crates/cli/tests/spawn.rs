mod common;
use common::*;
use drover::events::{append, fold, read, Event};
use drover::spawn::lane_port;
use serde_json::{json, Value};

fn port12() -> String {
    format!("DROVER_PORT={}", lane_port(None, "sendmeter/issue-12", &[]))
}

fn plan(c: &drover::Ctx, r: &Fake, role: &str) -> Value {
    let (code, out) = run(c, r, &["spawn", "sendmeter", "12", role, "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

fn on_shed(c: &drover::Ctx) -> FakeShed {
    let sh = FakeShed::new();
    sh.attach(c);
    sh
}

fn shed_fake() -> Fake {
    Fake::default().reply("which shed", "/opt/shed/bin/shed\n")
}

fn requests(p: &Value) -> Vec<(String, Value)> {
    let cmds = p["commands"].as_array().unwrap();
    let shed = cmds.iter().filter(|c| c[0] == "shed" && c[2].is_string());
    shed.map(|c| {
        let params = serde_json::from_str(c[2].as_str().unwrap()).unwrap_or_default();
        (c[1].as_str().unwrap().to_string(), params)
    })
    .collect()
}

fn typed(p: &Value, head: &str) -> String {
    let sends = requests(p).into_iter().filter(|r| r.0 == "send");
    let texts = sends.map(|r| r.1["text"].as_str().unwrap().trim_end_matches('\r').to_string());
    texts.into_iter().find(|t| t.starts_with(head)).unwrap()
}

fn export(p: &Value) -> Vec<String> {
    raw_export(p).into_iter().filter(|w| !w.contains("/.build/")).collect()
}

fn low() -> Vec<String> {
    drover::spawn::BACKGROUND.split_whitespace().map(String::from).collect()
}

fn raw_export(p: &Value) -> Vec<String> {
    typed(p, "export ").split_whitespace().map(String::from).collect()
}

fn launch(p: &Value) -> Vec<String> {
    let (mut out, mut w, mut quoted, mut any) = (vec![], String::new(), false, false);
    for ch in typed(p, "exec ").chars() {
        match ch {
            '\'' => (quoted, any) = (!quoted, true),
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut w));
                }
                any = false;
            }
            c => (w, any) = (w + &c.to_string(), true),
        }
    }
    out.extend(any.then_some(w));
    out.into_iter().filter(|w| !w.contains("/.build/")).collect()
}

#[test]
fn claude_role_dry_run_uses_synced_profile() {
    let d = tmpdir("spawn-claude");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let sh = on_shed(&c);
    let r = shed_fake();
    let p = plan(&c, &r, "impl");
    let run = &requests(&p)[0];
    assert_eq!(run.0, "run");
    assert_eq!(
        (&run.1["name"], &run.1["labels"]["harness"]),
        (&json!("sendmeter-impl-12"), &json!("claude"))
    );
    assert_eq!(
        export(&p),
        [
            "export",
            "DROVER_LANE='sendmeter:12:impl'",
            &port12(),
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1",
            "DISABLE_AUTOUPDATER=1",
            "&&",
            "unset",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ]
        .map(String::from)
        .into_iter()
        .chain(low())
        .collect::<Vec<_>>()
    );
    let start = launch(&p);
    let prof = c.config.parent().unwrap().join("claude/fleet-impl");
    let (settings, role) = (prof.join("settings.json"), prof.join("role.md"));
    let expect = [
        "exec",
        "claude",
        "--dangerously-skip-permissions",
        "--settings",
        settings.to_str().unwrap(),
        "--append-system-prompt-file",
        role.to_str().unwrap(),
        "--model",
        "opus",
    ];
    assert_eq!(&start[..expect.len()], &expect);
    let kick = start.last().unwrap();
    assert!(
        kick.starts_with("Read ") && kick.ends_with("issue-12/.brief.md and execute it fully."),
        "{kick}"
    );
    assert_eq!(p["branch"], "issue-12");
    assert_eq!(p["base"], "origin/staging");
    let wait = requests(&p).pop().unwrap();
    assert_eq!(
        (wait.0.as_str(), &wait.1["name"], &wait.1["until"]),
        ("wait", &json!("sendmeter-impl-12"), &json!(["working", "blocked"]))
    );
    assert_eq!(*r.calls.borrow(), ["which shed"], "dry-run calls nothing");
    assert!(sh.methods().is_empty(), "dry-run calls nothing");
    assert!(read(&c.state).unwrap().is_empty(), "dry-run logs nothing");
}

#[test]
fn profiles_sync_removes_a_stale_generated_profile() {
    let c = ctx(&tmpdir("spawn-stale"), CONFIG);
    let dir = c.config.parent().unwrap().join("claude");
    for (p, f) in [("fleet-orch", "role.md"), ("mine", "notes.md")] {
        std::fs::create_dir_all(dir.join(p)).unwrap();
        std::fs::write(dir.join(p).join(f), "x").unwrap();
    }
    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed stale {}", dir.join("fleet-orch").display())),
        "{out}"
    );
    assert!(!dir.join("fleet-orch").exists() && dir.join("mine/notes.md").exists());
    assert!(dir.join("fleet-impl/role.md").exists() && dir.join("fleet-rev/role.md").exists());
}

#[test]
fn profiles_sync_gives_a_verify_profile_its_own_prompt() {
    let verify = "\n[fleet.sendmeter.role.verify]\nharness = \"claude\"\nprofile = \"fleet-verify\"\n";
    let c = ctx(&tmpdir("spawn-verify"), &format!("{CONFIG}{verify}"));
    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    let dir = c.config.parent().unwrap().join("claude/fleet-verify");
    assert!(out.contains(&format!("synced {} (verify)", dir.display())), "{out}");
    let role = std::fs::read_to_string(dir.join("role.md")).unwrap();
    assert!(role.starts_with("# ROLE: VERIFIER"), "{role}");
    let s = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(s.contains("hook --role rev"), "{s}");
}

#[test]
fn profiles_turn_attribution_off_and_role_md_has_the_rule() {
    let c = ctx(&tmpdir("spawn-attr"), CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0, "{out}");
    let dir = c.config.parent().unwrap().join("claude");
    let s: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("fleet-impl/settings.json")).unwrap()).unwrap();
    assert_eq!(s["attribution"], serde_json::json!({"commit": "", "pr": ""}));
    assert_eq!(s["includeCoAuthoredBy"], false);
    assert_eq!(s["cleanupPeriodDays"], 30);
    for p in ["typescript-lsp", "pyright-lsp", "swift-lsp", "vercel"] {
        assert_eq!(
            s["enabledPlugins"][format!("{p}@claude-plugins-official")],
            false,
            "{p}"
        );
    }
    let role = std::fs::read_to_string(dir.join("fleet-impl/role.md")).unwrap();
    assert!(role.contains("No AI co-author or 'generated with' trailers in commits or PR bodies"));
    assert!(role.contains("## For humans"));
    assert!(drover::spawn::role_prompt("rev").contains("## For humans"));
}

#[test]
fn rev_role_flags_new_commit_trailers_only() {
    let rev = drover::spawn::role_prompt("rev");
    assert!(rev.contains("new commits only, never force-push"), "{rev}");
    assert!(drover::spawn::role_prompt("impl").contains("never rewrite history or force-push"));
}

#[test]
fn cap_asks_only_when_your_own_pr_crosses() {
    let d = tmpdir("help-429");
    let (_, help) = run(&ctx(&d, CONFIG), &Fake::default(), &["help", "--agent"]);
    let impl_ = drover::spawn::role_prompt("impl").replace("\n  ", " ");
    for t in [impl_.as_str(), help.as_str()] {
        for w in [
            "The cap in `ci/cap.toml` is a total for the repo",
            "ask only when your own PR, merged onto current integration, would put the total over `line_cap`; trim first, then ask `--kind owner`.",
            "Being near the cap, or another PR's growth, is never a reason to ask.",
            "A `STAGING_WOULD_BREAK` from the cap after another merge is a fix round: trim, or ask under the same rule.",
            "Never edit `ci/cap.toml` (the hook refuses it",
            "`drover cap <fleet>`'s queue-wide view",
            "in one orchestrator-filed issue (recommended), or trim.",
        ] {
            assert!(t.contains(w), "{w}");
        }
    }
}

#[test]
fn conflict_predicted_is_not_a_hold() {
    let d = tmpdir("help-320");
    let (_, help) = run(&ctx(&d, CONFIG), &Fake::default(), &["help", "--agent"]);
    let impl_ = drover::spawn::role_prompt("impl");
    for t in [impl_, help.as_str()] {
        assert!(t.contains("`conflict-predicted` note means another open PR touches the same files"));
        assert!(t.contains("drover gives you an update round"));
    }
    assert!(impl_.contains("Don't wait for it.") && help.contains("don't wait for it"));
    for t in ["impl", "rev", "design"]
        .map(drover::spawn::role_prompt)
        .into_iter()
        .chain([help.as_str()])
    {
        assert!(!t.contains("to land") && !t.contains("lands."), "{t}");
    }
}

#[test]
fn fleet_env_is_exported_into_the_lane() {
    let d = tmpdir("spawn-env");
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "[fleet.sendmeter.env]\nCARGO_TARGET_DIR = \"~/.cache/drover/target/sendmeter\"\nnpm_config_cache = \"/c/npm\"\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&d, &cfg);
    let _sh = on_shed(&c);
    let p = plan(&c, &shed_fake(), "impl");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        export(&p),
        [
            vec![
                "export".to_string(),
                "DROVER_LANE='sendmeter:12:impl'".into(),
                port12(),
                format!("CARGO_TARGET_DIR='{home}/.cache/drover/target/sendmeter'"),
                "npm_config_cache='/c/npm'".into(),
            ],
            low()
        ]
        .concat()
    );
}

#[test]
fn shared_cache_is_exported_into_the_lane() {
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "shared_cache = { CARGO_TARGET_DIR = \"/c/cache/sendmeter/target\" }\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&tmpdir("spawn-shared-cache"), &cfg);
    let _sh = on_shed(&c);
    assert_eq!(
        export(&plan(&c, &shed_fake(), "impl"))[3..],
        [vec!["CARGO_TARGET_DIR='/c/cache/sendmeter/target'".to_string()], low()].concat()
    );
}

#[test]
fn each_lane_gets_its_own_cargo_target_under_its_root() {
    let other = "[fleet.other]\nrepo = \"jirathip-dev/other\"\nlocal = \"/tmp/x/other\"\nintegration = \"staging\"\n\n\
                 [fleet.other.role.impl]\nharness = \"hermes\"\nprofile = \"fleet-impl\"\n\n";
    let cfg = other.to_string() + CONFIG + "\n[hosts.mini]\nroot = \"/c\"\n";
    let c = ctx(&tmpdir("spawn-shared-target"), &cfg);
    let _sh = on_shed(&c);
    let target = |fleet: &str, n: &str| {
        let (code, out) = run(&c, &shed_fake(), &["spawn", fleet, n, "impl", "--dry-run", "--json"]);
        assert_eq!(code, 0, "{out}");
        let p: Value = serde_json::from_str(&out).unwrap();
        let w = raw_export(&p).into_iter().find(|w| w.contains("TARGET_DIR="));
        w.and_then(|w| Some(w.split_once('=')?.1.to_string()))
    };
    assert_eq!(
        target("sendmeter", "12").as_deref(),
        Some("'/c/.build/sendmeter-impl-12'")
    );
    assert_eq!(
        target("sendmeter", "13").as_deref(),
        Some("'/c/.build/sendmeter-impl-13'")
    );
    assert_eq!(target("other", "12").as_deref(), Some("'/c/.build/other-impl-12'"));
}

#[test]
fn each_lane_gets_a_private_scratch_dir_under_its_roots_build_folder() {
    let c = ctx(
        &tmpdir("spawn-scratch"),
        &(CONFIG.to_string() + "\n[hosts.mini]\nroot = \"/c\"\n"),
    );
    let _sh = on_shed(&c);
    let scratch = |c: &drover::Ctx, role: &str| {
        raw_export(&plan(c, &shed_fake(), role))
            .into_iter()
            .find(|w| w.starts_with("DROVER_SCRATCH_TARGET="))
    };
    assert_eq!(
        scratch(&c, "impl").as_deref(),
        Some("DROVER_SCRATCH_TARGET='/c/.build/.scratch/sendmeter-impl-12'")
    );
    assert_eq!(
        scratch(&c, "rev").as_deref(),
        Some("DROVER_SCRATCH_TARGET='/c/.build/.scratch/sendmeter-rev-12-r1'")
    );
    let bare = ctx(&tmpdir("spawn-scratch-bare"), CONFIG);
    let _bare_sh = on_shed(&bare);
    let want = bare.worktrees.join(".build/.scratch/sendmeter-impl-12");
    assert_eq!(
        scratch(&bare, "impl"),
        Some(format!("DROVER_SCRATCH_TARGET='{}'", want.display()))
    );
}

#[test]
fn lane_port_is_stable_per_fleet_and_branch() {
    let p = lane_port(None, "sendmeter/issue-12", &[]);
    assert_eq!(p, lane_port(None, "sendmeter/issue-12", &[]));
    assert!((41000..=41999).contains(&p), "{p}");
    assert_ne!(
        p,
        lane_port(None, "other/issue-12", &[]),
        "the fleet is part of the key"
    );
    assert!((5000..=5009).contains(&lane_port(Some("5000-5009"), "a/b", &[])));
    let c = ctx(&tmpdir("spawn-port"), CONFIG);
    let _sh = on_shed(&c);
    assert_eq!(plan(&c, &shed_fake(), "impl")["port"], p);
}

#[test]
fn lane_port_collision_takes_the_next_free_one() {
    let p = lane_port(Some("7000-7001"), "f/b", &[]);
    let other = if p == 7000 { 7001 } else { 7000 };
    assert_eq!(lane_port(Some("7000-7001"), "f/b", &[p]), other);
    assert_eq!(
        lane_port(Some("7000-7001"), "f/b", &[7000, 7001]),
        p,
        "a full range keeps the hash"
    );

    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "port_range = \"7000-7001\"\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&tmpdir("spawn-port-taken"), &cfg);
    let _sh = on_shed(&c);
    let mine = plan(&c, &shed_fake(), "impl")["port"].as_u64().unwrap();
    let spawn = |n: u64, branch: &str| {
        let d = json!({"role": "impl", "agent": format!("sendmeter-impl-{n}"), "branch": branch, "port": mine});
        append(&c.state, &Event::new(10, "sendmeter", Some(n), "spawn", d)).unwrap();
    };
    spawn(12, "issue-12");
    assert_eq!(plan(&c, &shed_fake(), "impl")["port"], mine, "its own lane");
    spawn(13, "issue-13");
    assert_eq!(plan(&c, &shed_fake(), "impl")["port"], 14001 - mine);
}

#[test]
fn fleet_env_expands_drover_port() {
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "[fleet.sendmeter.env]\nPORT = \"$DROVER_PORT\"\nVITE = \"http://127.0.0.1:$DROVER_PORT/\"\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&tmpdir("spawn-env-port"), &cfg);
    let _sh = on_shed(&c);
    let p = lane_port(None, "sendmeter/issue-12", &[]);
    assert_eq!(
        export(&plan(&c, &shed_fake(), "impl"))[3..],
        [
            vec![format!("PORT='{p}'"), format!("VITE='http://127.0.0.1:{p}/'")],
            low()
        ]
        .concat()
    );
}

#[test]
fn lockfiles_match_needs_an_identical_lockfile() {
    let d = tmpdir("lockfiles");
    let (local, wt) = (d.join("local"), d.join("wt"));
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    let m = || drover::spawn::lockfiles_match(&["package-lock.json", "pnpm-lock.yaml"], &local, &wt);
    assert!(!m(), "no lockfile anywhere is no match");
    std::fs::write(local.join("package-lock.json"), "a").unwrap();
    assert!(!m(), "missing in the lane");
    std::fs::write(wt.join("package-lock.json"), "a").unwrap();
    assert!(m());
    std::fs::write(wt.join("pnpm-lock.yaml"), "b").unwrap();
    assert!(!m(), "a lockfile only the lane has");
    std::fs::write(local.join("pnpm-lock.yaml"), "c").unwrap();
    assert!(!m(), "differing lockfiles");
}

#[test]
fn clone_build_clones_on_a_lockfile_match_and_logs_failures() {
    let d = tmpdir("clone-build");
    let (local, wt) = (d.join("local"), d.join("wt"));
    std::fs::create_dir_all(local.join("target")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(local.join("Cargo.lock"), "a").unwrap();
    std::fs::write(wt.join("Cargo.lock"), "b").unwrap();
    let dirs = ["target".to_string(), "node_modules".into()];
    let clone = |r: &Fake| drover::spawn::clone_build(r, &dirs, &local, &wt);
    let r = Fake::default().reply("cp ", "");
    assert_eq!(clone(&r), None, "mismatch: no clone");
    assert!(!r.called("cp "));
    std::fs::write(wt.join("Cargo.lock"), "a").unwrap();
    std::fs::create_dir(wt.join(".target.clone")).unwrap();
    let d = clone(&r).unwrap();
    assert_eq!((d["dirs"].clone(), d["errors"].clone()), (json!(["target"]), json!([])));
    assert!(wt.join("target").is_dir() && !wt.join(".target.clone").exists());
    let calls = r.calls.borrow().clone();
    assert_eq!(calls.len(), 1, "node_modules has no lockfile: {calls:?}");
    let (src, tmp) = (local.join("target"), wt.join(".target.clone"));
    assert!(calls[0].ends_with(&format!(" {} {}", src.display(), tmp.display())));
    let flag = if cfg!(target_os = "macos") {
        "cp -cR "
    } else {
        "cp -R --reflink=always "
    };
    assert!(calls[0].starts_with(flag), "{}", calls[0]);
    assert_eq!(clone(&r), None, "the lane has its own target: never cloned over");
    std::fs::remove_dir(wt.join("target")).unwrap();
    let d = clone(&Fake::default()).unwrap();
    assert_eq!(d["dirs"], json!([]));
    assert!(d["errors"][0].as_str().unwrap().starts_with("target: "), "{d}");
    let d = drover::spawn::clone_build(&r, &["targt".into()], &local, &wt).unwrap();
    assert_eq!(d["errors"], json!(["targt: not target or node_modules"]));
    assert_eq!(r.count("cp "), 1);
}

#[test]
fn reopen_spawn_clones_no_build_folder() {
    let d = tmpdir("clone-reopen");
    let local = d.join("local");
    let cfg = CONFIG
        .replace("/tmp/does-not-matter/sendmeter", &local.to_string_lossy())
        .replace(
            "integration = \"staging\"",
            "integration = \"staging\"\nclone_build = [\"target\"]",
        );
    let c = ctx(&d, &cfg);
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(local.join("target")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(local.join("Cargo.lock"), "a").unwrap();
    std::fs::write(wt.join("Cargo.lock"), "a").unwrap();
    let sh = on_shed(&c);
    let r = shed_fake()
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply("cp ", "");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert!(sh.called("run", "sendmeter-impl-12"), "the lane started");
    let calls = r.calls.borrow();
    assert!(
        !calls.iter().any(|c| c.contains(" worktree add ")),
        "reopened: {calls:?}"
    );
    drop(calls);
    assert!(!r.called("cp "));
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "spawn") && !ev.iter().any(|e| e.kind == "build-cloned"));
}

struct Checkout<'a> {
    f: &'a Fake,
    wt: std::path::PathBuf,
    sh: &'a FakeShed,
    beside: Shared<bool>,
    cloned: Shared<bool>,
    typed: Shared<usize>,
}

const LANE: &str = "sendmeter-impl-12";

fn within(ok: impl Fn() -> bool) -> bool {
    let t = std::time::Instant::now();
    while !ok() && t.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    ok()
}

impl drover::runner::Runner for Checkout<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "git" && args.get(2..4) == Some(&["worktree".into(), "add".into()]) {
            std::fs::create_dir_all(&self.wt).unwrap();
            std::fs::write(self.wt.join("Cargo.lock"), "a").unwrap();
        }
        if prog == "cp" {
            self.beside.set(within(|| !self.sh.all("wait").is_empty()));
            self.typed.set(self.sh.sent(LANE).len());
            self.sh.lock().waits.remove(LANE);
            self.cloned.set(true);
        }
        self.f.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        within(|| self.cloned.get());
        self.f.sleep(secs)
    }
}

#[test]
fn failed_clone_is_logged_and_the_spawn_succeeds() {
    let d = tmpdir("clone-fail");
    let local = d.join("local");
    std::fs::create_dir_all(local.join("target")).unwrap();
    std::fs::write(local.join("Cargo.lock"), "a").unwrap();
    let cfg = CONFIG
        .replace("/tmp/does-not-matter/sendmeter", &local.to_string_lossy())
        .replace(
            "integration = \"staging\"",
            "integration = \"staging\"\nclone_build = [\"target\"]",
        );
    let c = ctx(&d, &cfg);
    let sh = on_shed(&c);
    sh.wait_ends(LANE, "timeout");
    let wt = c.worktrees.join("sendmeter/issue-12");
    let f = shed_fake()
        .once(&format!("git -C {} rev-parse --verify", local.display()), 1, "")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .once_err("cp ", 1, "clonefile failed");
    let ck = Checkout {
        f: &f,
        wt,
        sh: &sh,
        beside: Shared::new(false),
        cloned: Shared::new(false),
        typed: Shared::new(0),
    };
    let (code, out) = run(&c, &ck, &["spawn", "sendmeter", "12", "impl"]);
    assert!(ck.beside.get(), "the clone runs beside the kick, not before it");
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert_eq!(f.count("cp "), 1, "one clone attempt, never a fallback copy");
    assert!(
        ck.typed.get() >= 2,
        "the lane starts (its exports and harness typed) before the clone"
    );
    let ev = read(&c.state).unwrap();
    let e = ev
        .iter()
        .find(|e| e.kind == "build-cloned")
        .expect("build-cloned logged");
    assert_eq!(*e.val("dirs"), json!([]));
    assert!(
        e.val("errors")[0].as_str().unwrap().contains("clonefile failed"),
        "{e:?}"
    );
    assert_eq!(fold(&ev)[&("sendmeter".to_string(), 12)].state, "impl");
}

#[test]
fn hermes_role_dry_run_kicks_prompt_after_boot() {
    let d = tmpdir("spawn-hermes");
    let c = ctx(&d, CONFIG);
    let _sh = on_shed(&c);
    let p = plan(&c, &shed_fake(), "rev");
    assert_eq!(
        export(&p),
        [
            vec![
                "export".to_string(),
                "DROVER_LANE='sendmeter:12:rev'".into(),
                format!("DROVER_PORT={}", lane_port(None, "sendmeter/rev-12-r1", &[])),
            ],
            low()
        ]
        .concat()
    );
    let start = launch(&p);
    assert_eq!(
        start[start.len() - 3..],
        ["-p".to_string(), "fleet-rev".into(), "--accept-hooks".into()]
    );
    assert_eq!(start[..2], ["exec", "hermes"].map(String::from));
    let q = requests(&p);
    let at = q.iter().position(|r| r.0 == "wait").unwrap();
    assert_eq!(
        (&q[at].1["name"], &q[at].1["output"]),
        (&json!("sendmeter-rev-12-r1"), &json!("❯"))
    );
    assert_eq!(q[at + 1].0, "send");
    assert_eq!(q[at + 1].1, json!({"name": "sendmeter-rev-12-r1", "text": p["kick"]}));
    assert_eq!(p["base"], "origin/issue-12");
}

#[test]
fn real_claude_spawn_writes_brief_and_logs_event() {
    let d = tmpdir("spawn-real");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let sh = on_shed(&c);
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let (code, _) = run(&c, &shed_fake(), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 1);
    assert!(!sh.called("run", LANE));
    let (code, _) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0);
    let settings = std::fs::read_to_string(c.config.parent().unwrap().join("claude/fleet-impl/settings.json")).unwrap();
    let s: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "'/opt/drover/bin/drover' hook --role impl"
    );
    let r = shed_fake()
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("git -C", "");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let sent = sh.sent(LANE);
    assert!(
        sent.len() == 2
            && sent[0].starts_with("export DROVER_LANE='sendmeter:12:impl'")
            && sent[1].starts_with("exec 'claude'"),
        "{sent:?}"
    );
    assert!(
        sent[1].contains("'--name' 'sendmeter · impl #12 · Fix the meter' '--mcp-config'"),
        "{}",
        sent[1]
    );
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains("# ROLE: IMPLEMENTER") && brief.contains("AC: it works") && brief.contains("Closes #12"));
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(
        (st.state.as_str(), st.lane.as_deref()),
        ("impl", Some("sendmeter-impl-12"))
    );
    let ev = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(ev.val("port"), lane_port(None, "sendmeter/issue-12", &[]));
    assert_eq!(ev.val("confirmed"), true);
}

#[test]
fn unconfirmed_hermes_kick_presses_enter_then_exits_4() {
    let d = tmpdir("spawn-unconfirmed");
    let c = ctx(&d, CONFIG);
    let sh = on_shed(&c);
    sh.wait_ends(LANE, "timeout");
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let r = shed_fake()
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "");
    let (code, _) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 4);
    let prompts = sh.sent(LANE).into_iter().filter(|t| t.starts_with("Read "));
    assert_eq!(prompts.count(), 3, "3 attempts");
    let enters = sh
        .all("keys")
        .into_iter()
        .filter(|k| k["name"] == LANE && k["keys"] == json!(["Enter"]));
    assert_eq!(enters.count(), 3);
    assert_eq!(r.count("sleep 10"), 2, "attempts spaced ≥10 s apart");
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.last().unwrap().val("confirmed"), false);
}

#[test]
fn help_agent_doc_is_short_and_complete() {
    let d = tmpdir("help");
    let c = ctx(&d, CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["help", "--agent"]);
    assert_eq!(code, 0);
    assert!(
        out.lines().count() <= drover::HELP_AGENT_MAX,
        "{} lines",
        out.lines().count()
    );
    for w in [
        "drover spawn",
        "drover merge",
        "drover verdict",
        "drover set",
        "drover status",
        "drover profiles sync",
        "drover timer install",
        "drover check",
        "drover clean",
        "drover adopt",
        "--force",
        "--supersede",
        "--yes",
        "MAIN_TARGET",
        "HOST_BUSY",
        "LANE_LIVE",
        "WORKTREE_DIRTY",
        "review(n)",
        "--serial",
        "SERIAL_BUSY",
        "SERIAL_UNMARKED",
        "CAP",
        "BAD_VALUE",
        "design",
        "owner-only",
        "drover stop",
        "drover kill",
        "--lane",
        "SECRET_IN_WIP",
    ] {
        assert!(out.contains(w), "missing {w}");
    }
    assert!(!out.contains("drover hook"));
}

#[test]
fn effort_maps_to_each_harness_flag() {
    let d = tmpdir("effort-claude");
    let cfg = CONFIG
        .replace(
            "harness  =  \"hermes\"    # toggled in tests",
            "harness = \"claude\"\neffort = \"high\"",
        )
        .replace("review_rounds = 3", "review_rounds = 3\neffort = \"xhigh\"");
    let c = ctx(&d, &cfg);
    let _sh = on_shed(&c);
    let p = plan(&c, &shed_fake(), "impl");
    let start = launch(&p);
    let at = start.iter().position(|w| w == "--effort").expect("--effort");
    assert_eq!(start[at + 1], "high");
    assert_eq!(p["effort"], "high");
    let start = launch(&plan(&c, &shed_fake(), "rev"));
    assert_eq!(start[start.len() - 2..], ["--reasoning", "xhigh"].map(String::from));

    let d = tmpdir("effort-none");
    let c = ctx(
        &d,
        &cfg.replace("effort = \"high\"", "effort = \"none\"")
            .replace("effort = \"xhigh\"", "effort = \"none\""),
    );
    let _sh = on_shed(&c);
    assert!(!launch(&plan(&c, &shed_fake(), "impl")).contains(&"--effort".to_string()));
    let start = launch(&plan(&c, &shed_fake(), "rev"));
    assert_eq!(start[start.len() - 2..], ["--reasoning", "none"].map(String::from));

    let d = tmpdir("effort-unset");
    let c = ctx(&d, CONFIG);
    let _sh = on_shed(&c);
    let start = launch(&plan(&c, &shed_fake(), "rev"));
    assert!(!start.contains(&"--reasoning".to_string()), "{start:?}");
}

#[test]
fn invalid_effort_is_refused_at_config_load() {
    for (cfg, want) in [
        (
            CONFIG.replace("profile = \"fleet-rev\"", "profile = \"fleet-rev\"\neffort = \"max\""),
            "fleet.sendmeter.role.rev: effort \"max\" must be one of",
        ),
        (
            CONFIG.replace("review_rounds = 3", "review_rounds = 3\neffort = \"ultra\""),
            "limits.effort: \"ultra\" must be one of",
        ),
        (
            CONFIG.replace(
                "harness = \"hermes\"\nprofile = \"fleet-rev\"",
                "harness = \"dsh\"\nprofile = \"fleet-rev\"\neffort = \"high\"",
            ),
            "fleet.sendmeter.role.rev: harness dsh does not support effort \"high\"",
        ),
    ] {
        let d = tmpdir("effort-bad");
        let c = ctx(&d, &cfg);
        let err = drover::config::Config::load(&c.config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(want), "{msg}");
        let (code, _) = run(&c, &Fake::default(), &["spawn", "sendmeter", "12", "rev", "--dry-run"]);
        assert_eq!(code, 1);
    }
}

#[test]
fn claude_dry_run_names_every_role() {
    let d = tmpdir("spawn-name");
    let cfg = CONFIG
        .replace("harness = \"hermes\"", "harness = \"claude\"")
        .replace("harness  =  \"hermes\"", "harness = \"claude\"")
        + "\n[fleet.sendmeter.role.design]\nharness = \"claude\"\nprofile = \"fleet-design\"\n";
    let c = ctx(&d, &cfg);
    let _sh = on_shed(&c);
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("issue-sendmeter-12.json"),
        r#"{"title":"feat(meter): fix the meter","body":""}"#,
    )
    .unwrap();
    let name = |args: &[&str]| {
        let (code, out) = run(&c, &shed_fake(), args);
        assert_eq!(code, 0, "{out}");
        let p: Value = serde_json::from_str(&out).unwrap();
        let start = launch(&p);
        let at = start.iter().position(|w| w == "--name").expect("--name");
        assert_eq!(start[at + 1], p["title"].as_str().unwrap());
        start[at + 1].clone()
    };
    let dry = |n: &str, role: &str| name(&["spawn", "sendmeter", n, role, "--dry-run", "--json"]);
    assert_eq!(dry("12", "impl"), "sendmeter · impl #12 · fix the meter");
    assert_eq!(dry("12", "rev"), "sendmeter · rev #12 r1 · fix the meter");
    assert_eq!(dry("12", "design"), "sendmeter · design #12 · fix the meter");
    assert_eq!(dry("13", "impl"), "sendmeter · impl #13");
}

#[test]
fn lane_title_strips_prefixes_and_cuts_at_words() {
    use drover::spawn::lane_title;
    let h = "synergy · impl #2840";
    assert_eq!(
        lane_title(h, "M- prefix for Mango numbers"),
        "synergy · impl #2840 · M- prefix for Mango numbers"
    );
    assert_eq!(
        lane_title(h, "P0: feat(api)!: drop v1"),
        "synergy · impl #2840 · drop v1"
    );
    assert_eq!(
        lane_title(h, "spawn: informative titles"),
        "synergy · impl #2840 · informative titles"
    );
    assert_eq!(lane_title(h, "Fix 10:30 cron"), "synergy · impl #2840 · Fix 10:30 cron");
    assert_eq!(lane_title(h, "  "), h);
    let long = lane_title(
        h,
        "spawn: informative lane titles instead of Claude auto-titles from the brief prompt",
    );
    assert_eq!(long, "synergy · impl #2840 · informative lane titles instead of…");
    assert!(long.chars().count() <= 60);
    assert_eq!(lane_title(h, "แก้ไขปุ่มชำระเงิน"), "synergy · impl #2840 · แก้ไขปุ่มชำระเงิน");
    let thai = lane_title(h, &"ภาษาไทย".repeat(10));
    assert!(thai.ends_with('…') && thai.chars().count() <= 60, "{thai}");
    assert!(thai.starts_with("synergy · impl #2840 · ภาษาไทยภาษาไทย"));
}

#[test]
fn a_spawned_lanes_build_command_runs_in_the_background_but_a_design_lanes_does_not() {
    let stages = "\n[fleet.sendmeter.role.design]\nharness = \"hermes\"\n\n[fleet.sendmeter.role.architect]\nharness = \"hermes\"\n";
    let c = ctx(&tmpdir("spawn-nice"), &format!("{CONFIG}{stages}"));
    let _sh = on_shed(&c);
    let line = typed(&plan(&c, &shed_fake(), "impl"), "export ");
    let build = "sh -c 'ps -o pri= -o nice= -p $$'";
    let out = std::process::Command::new("sh")
        .args(["-c", &format!("{line} && {build}")])
        .output()
        .unwrap();
    let got: Vec<i32> = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(|w| w.parse().unwrap())
        .collect();
    let background = if cfg!(target_os = "macos") {
        got[0] <= 4
    } else {
        got[1] >= 10
    };
    assert!(background, "the build's pri, nice: {got:?}");
    for role in ["design", "architect"] {
        let line = typed(&plan(&c, &shed_fake(), role), "export ");
        assert!(
            !line.contains("taskpolicy") && !line.contains("renice"),
            "{role}: {line}"
        );
    }
}
