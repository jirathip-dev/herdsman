//! AC5: spawn launches the right harness with the synced profile.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use herdsman::spawn::lane_port;
use serde_json::{json, Value};

/// #187: issue 12's impl lane port with nothing taken.
fn port12() -> String {
    format!("HERDSMAN_PORT={}", lane_port(None, "sendmeter/issue-12", &[]))
}

fn plan(c: &herdsman::Ctx, r: &Fake, role: &str) -> Value {
    let (code, out) = run(c, r, &["spawn", "sendmeter", "12", role, "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

fn words(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn claude_role_dry_run_uses_synced_profile() {
    let d = tmpdir("spawn-claude");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let r = Fake::default();
    let p = plan(&c, &r, "impl");
    assert_eq!(
        words(&p["commands"][2])[1..],
        [
            "pane",
            "run",
            "<pane>",
            "export",
            "HERDSMAN_LANE='sendmeter:12:impl'",
            &port12(),
            // #248: no updater, telemetry or other background traffic in a Claude lane.
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1",
            "DISABLE_AUTOUPDATER=1"
        ]
    );
    let start = words(&p["commands"][3]);
    let prof = c.config.parent().unwrap().join("claude/fleet-impl");
    let (settings, role) = (prof.join("settings.json"), prof.join("role.md"));
    let expect = [
        "herdr",
        "agent",
        "start",
        "sendmeter-impl-12",
        "--kind",
        "claude",
        "--pane",
        "<pane>",
        "--timeout",
        "90000",
        "--",
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
    assert!(words(&p["commands"][4]).starts_with(&["herdr".into(), "agent".into(), "wait".into()]));
    assert!(r.calls.borrow().is_empty(), "dry-run calls nothing");
    assert!(read(&c.state).unwrap().is_empty(), "dry-run logs nothing");
}

/// #155: `profiles sync` removes a generated Claude profile no role names (the retired `fleet-orch`), nothing else.
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

/// #577: a verify role's own profile gets verify's prompt, still with the read-only check hook.
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

/// #198: lanes add no AI co-author: Claude's attribution is off, and every lane's role text says so.
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
    // #248: lane transcripts are kept `[limits] transcript_days` (30, Claude's default: lanes share ~/.claude, so no lower).
    assert_eq!(s["cleanupPeriodDays"], 30);
    // #223: lanes don't inherit the user's IDE/LSP plugins (tsserver ~2 GB per lane).
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
    assert!(herdsman::spawn::role_prompt("rev").contains("## For humans"));
}

/// #226: a reviewer never FAILs on an older commit's trailer; the fix would be a forbidden force-push.
#[test]
fn rev_role_flags_new_commit_trailers_only() {
    let rev = herdsman::spawn::role_prompt("rev");
    assert!(rev.contains("new commits only, never force-push"), "{rev}");
    assert!(herdsman::spawn::role_prompt("impl").contains("never rewrite history or force-push"));
}

/// #320: a predicted conflict is information, not a hold; no generated prompt tells a lane to wait for a merge.
/// #429: the cap is a repo total; a lane asks only when its own PR would cross it.
#[test]
fn cap_asks_only_when_your_own_pr_crosses() {
    let d = tmpdir("help-429");
    let (_, help) = run(&ctx(&d, CONFIG), &Fake::default(), &["help", "--agent"]);
    let impl_ = herdsman::spawn::role_prompt("impl").replace("\n  ", " ");
    for t in [impl_.as_str(), help.as_str()] {
        for w in [
            "The cap in `ci/cap.toml` is a total for the repo",
            "ask only when your own PR, merged onto current integration, would put the total over `line_cap`; trim first, then ask `--kind owner`.",
            "Being near the cap, or another PR's growth, is never a reason to ask.",
            "A `STAGING_WOULD_BREAK` from the cap after another merge is a fix round: trim, or ask under the same rule.",
        ] {
            assert!(t.contains(w), "{w}");
        }
    }
}

#[test]
fn conflict_predicted_is_not_a_hold() {
    let d = tmpdir("help-320");
    let (_, help) = run(&ctx(&d, CONFIG), &Fake::default(), &["help", "--agent"]);
    let impl_ = herdsman::spawn::role_prompt("impl");
    for t in [impl_, help.as_str()] {
        assert!(t.contains("`conflict-predicted` note means another open PR touches the same files"));
        assert!(t.contains("herdsman gives you an update round"));
    }
    assert!(impl_.contains("Don't wait for it.") && help.contains("don't wait for it"));
    for t in ["impl", "rev", "design"]
        .map(herdsman::spawn::role_prompt)
        .into_iter()
        .chain([help.as_str()])
    {
        assert!(!t.contains("to land") && !t.contains("lands."), "{t}");
    }
}

/// #103: `[fleet.<n>] env` is exported into the lane's shell after HERDSMAN_LANE, `~/` expanded.
#[test]
fn fleet_env_is_exported_into_the_lane() {
    let d = tmpdir("spawn-env");
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "[fleet.sendmeter.env]\nCARGO_TARGET_DIR = \"~/.cache/herdsman/target/sendmeter\"\nnpm_config_cache = \"/c/npm\"\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&d, &cfg);
    let p = plan(&c, &Fake::default(), "impl");
    let home = std::env::var("HOME").unwrap();
    assert_eq!(
        words(&p["commands"][2])[4..],
        [
            "export".to_string(),
            "HERDSMAN_LANE='sendmeter:12:impl'".into(),
            port12(),
            format!("CARGO_TARGET_DIR='{home}/.cache/herdsman/target/sendmeter'"),
            "npm_config_cache='/c/npm'".into(),
        ]
    );
}

/// #361: `shared_cache` is exported like `env`.
#[test]
fn shared_cache_is_exported_into_the_lane() {
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "shared_cache = { CARGO_TARGET_DIR = \"/c/cache/sendmeter/target\" }\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&tmpdir("spawn-shared-cache"), &cfg);
    assert_eq!(
        words(&plan(&c, &Fake::default(), "impl")["commands"][2])[7..],
        ["CARGO_TARGET_DIR='/c/cache/sendmeter/target'".to_string()]
    );
}

/// #438: lanes of one repo on a host share one Cargo build folder; another repo's lanes get their own.
#[test]
fn lanes_share_one_cargo_target_per_repo() {
    let other = "[fleet.other]\nrepo = \"jirathip-dev/other\"\nlocal = \"/tmp/x/other\"\nintegration = \"staging\"\n\n\
                 [fleet.other.role.impl]\nharness = \"hermes\"\nprofile = \"fleet-impl\"\n\n";
    let cfg = other.to_string() + &CONFIG.replace("build_root = \"\"", "build_root = \"/c/builds/\"");
    let c = ctx(&tmpdir("spawn-shared-target"), &cfg);
    let target = |fleet: &str, n: &str| {
        let (code, out) = run(
            &c,
            &Fake::default(),
            &["spawn", fleet, n, "impl", "--dry-run", "--json"],
        );
        assert_eq!(code, 0, "{out}");
        let p: Value = serde_json::from_str(&out).unwrap();
        words(&p["commands"][2])
            .into_iter()
            .find(|w| w.starts_with("CARGO_TARGET_DIR="))
    };
    let one = target("sendmeter", "12");
    assert_eq!(one.as_deref(), Some("CARGO_TARGET_DIR='/c/builds/sendmeter'"));
    assert_eq!(target("sendmeter", "13"), one);
    assert_eq!(
        target("other", "12").as_deref(),
        Some("CARGO_TARGET_DIR='/c/builds/other'")
    );
}

/// #536: each lane gets its own scratch dir under `build_root`, named for its agent; a reviewer's per round.
#[test]
fn each_lane_gets_a_private_scratch_dir_under_build_root() {
    let cfg = CONFIG.replace("build_root = \"\"", "build_root = \"/c/builds/\"");
    let c = ctx(&tmpdir("spawn-scratch"), &cfg);
    let scratch = |role: &str| {
        words(&plan(&c, &Fake::default(), role)["commands"][2])
            .into_iter()
            .find(|w| w.starts_with("HERDSMAN_SCRATCH_TARGET="))
    };
    assert_eq!(
        scratch("impl").as_deref(),
        Some("HERDSMAN_SCRATCH_TARGET='/c/builds/.scratch/sendmeter-impl-12'")
    );
    assert_eq!(
        scratch("rev").as_deref(),
        Some("HERDSMAN_SCRATCH_TARGET='/c/builds/.scratch/sendmeter-rev-12-r1'")
    );
    let off = ctx(&tmpdir("spawn-scratch-off"), CONFIG);
    let none = words(&plan(&off, &Fake::default(), "impl")["commands"][2]);
    assert!(
        !none.iter().any(|w| w.contains("SCRATCH")),
        "no build_root, no scratch: {none:?}"
    );
}

/// #187: the same fleet and branch always hash to the same port, inside the range; the dry run shows it.
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
    assert_eq!(plan(&c, &Fake::default(), "impl")["port"], p);
}

/// #187: a port a live lane holds moves to the next free one (wrapping); the lane's own earlier spawn is no collision.
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
    let mine = plan(&c, &Fake::default(), "impl")["port"].as_u64().unwrap();
    let spawn = |n: u64, branch: &str| {
        let d = json!({"role": "impl", "agent": format!("sendmeter-impl-{n}"), "branch": branch, "port": mine});
        append(&c.state, &Event::new(10, "sendmeter", Some(n), "spawn", d)).unwrap();
    };
    spawn(12, "issue-12");
    assert_eq!(plan(&c, &Fake::default(), "impl")["port"], mine, "its own lane");
    spawn(13, "issue-13");
    assert_eq!(plan(&c, &Fake::default(), "impl")["port"], 14001 - mine);
}

/// #187: `$HERDSMAN_PORT` in a fleet's env expands to the lane's port.
#[test]
fn fleet_env_expands_herdsman_port() {
    let cfg = CONFIG.replace(
        "[fleet.sendmeter.role.impl]",
        "[fleet.sendmeter.env]\nPORT = \"$HERDSMAN_PORT\"\nVITE = \"http://127.0.0.1:$HERDSMAN_PORT/\"\n\n[fleet.sendmeter.role.impl]",
    );
    let c = ctx(&tmpdir("spawn-env-port"), &cfg);
    let p = lane_port(None, "sendmeter/issue-12", &[]);
    assert_eq!(
        words(&plan(&c, &Fake::default(), "impl")["commands"][2])[7..],
        [format!("PORT='{p}'"), format!("VITE='http://127.0.0.1:{p}/'")]
    );
}

/// #103: `clone_node_modules` needs some lockfile, and every lockfile identical in both checkouts.
#[test]
fn lockfiles_match_needs_an_identical_lockfile() {
    let d = tmpdir("lockfiles");
    let (local, wt) = (d.join("local"), d.join("wt"));
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    let m = || herdsman::spawn::lockfiles_match(&["package-lock.json", "pnpm-lock.yaml"], &local, &wt);
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

/// #185: `clone_build` clones `target` on a matching `Cargo.lock` only; a failed clone is logged, never retried as a copy.
#[test]
fn clone_build_clones_on_a_lockfile_match_and_logs_failures() {
    let d = tmpdir("clone-build");
    let (local, wt) = (d.join("local"), d.join("wt"));
    std::fs::create_dir_all(local.join("target")).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(local.join("Cargo.lock"), "a").unwrap();
    std::fs::write(wt.join("Cargo.lock"), "b").unwrap();
    let dirs = ["target".to_string(), "node_modules".into()];
    let clone = |r: &Fake| herdsman::spawn::clone_build(r, &dirs, &local, &wt);
    let r = Fake::default().reply("cp ", "");
    assert_eq!(clone(&r), None, "mismatch: no clone");
    assert!(!r.called("cp "));
    std::fs::write(wt.join("Cargo.lock"), "a").unwrap();
    std::fs::create_dir(wt.join(".target.clone")).unwrap(); // what the faked `cp` would have made
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
    let d = herdsman::spawn::clone_build(&r, &["targt".into()], &local, &wt).unwrap();
    assert_eq!(d["errors"], json!(["targt: not target or node_modules"]));
    assert_eq!(r.count("cp "), 1);
}

/// #185: a reopened worktree keeps its own build folders.
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
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply(
            "herdr worktree open",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane send-keys w9:p1 Enter", "{}")
        .reply("cp ", "");
    run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert!(r.called("herdr agent start"), "{:?}", r.calls.borrow());
    assert!(!r.called("cp "));
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "spawn") && !ev.iter().any(|e| e.kind == "build-cloned"));
}

/// `Fake`, plus `herdr worktree create` checks out `Cargo.lock` into the new lane like the real one, and the kick's
/// `herdr agent wait` holds until `cp` ran (the third field: it did), which a clone run before or after it never does.
struct Checkout<'a>(&'a Fake, std::path::PathBuf, std::sync::atomic::AtomicBool);

impl herdsman::runner::Runner for Checkout<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<herdsman::runner::Out> {
        if prog == "herdr" && args.starts_with(&["worktree".into(), "create".into()]) {
            std::fs::create_dir_all(&self.1).unwrap();
            std::fs::write(self.1.join("Cargo.lock"), "a").unwrap();
        }
        if prog == "herdr" && args.starts_with(&["agent".into(), "wait".into()]) {
            let t = std::time::Instant::now();
            while !self.0.called("cp ") && t.elapsed().as_secs() < 5 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            self.2.store(self.0.called("cp "), std::sync::atomic::Ordering::SeqCst);
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs)
    }
}

/// #185: a fresh lane whose clone fails: the lane started first, the failure is logged, the spawn exits 0.
#[test]
fn failed_clone_is_logged_and_the_spawn_succeeds() {
    let d = tmpdir("clone-fail");
    let local = d.join("local");
    std::fs::create_dir_all(local.join("target")).unwrap();
    std::fs::write(local.join("Cargo.lock"), "a").unwrap();
    let cfg = CONFIG
        .replace("harness  =  \"hermes\"", "harness = \"claude\"")
        .replace("/tmp/does-not-matter/sendmeter", &local.to_string_lossy())
        .replace(
            "integration = \"staging\"",
            "integration = \"staging\"\nclone_build = [\"target\"]",
        );
    let c = ctx(&d, &cfg);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let wt = c.worktrees.join("sendmeter/issue-12");
    let f = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply(
            "herdr worktree create",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr agent wait", "{}")
        .once_err("cp ", 1, "clonefile failed");
    let ck = Checkout(&f, wt, Default::default());
    let (code, out) = run(&c, &ck, &["spawn", "sendmeter", "12", "impl"]);
    assert!(ck.2.into_inner(), "the clone runs beside the kick, not before it");
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert_eq!(f.count("cp "), 1, "one clone attempt, never a fallback copy");
    assert!(
        f.index("herdr agent start") < f.index("cp "),
        "the lane starts before the clone"
    );
    let ev = read(&c.state).unwrap();
    let e = ev
        .iter()
        .find(|e| e.kind == "build-cloned")
        .expect("build-cloned logged");
    assert_eq!(e.data["dirs"], json!([]));
    assert!(
        e.data["errors"][0].as_str().unwrap().contains("clonefile failed"),
        "{e:?}"
    );
    assert_eq!(fold(&ev)[&("sendmeter".to_string(), 12)].state, "impl");
}

#[test]
fn hermes_role_dry_run_kicks_prompt_after_boot() {
    let d = tmpdir("spawn-hermes");
    let c = ctx(&d, CONFIG);
    let p = plan(&c, &Fake::default(), "rev");
    assert_eq!(
        words(&p["commands"][2])[4..],
        [
            "export".to_string(),
            "HERDSMAN_LANE='sendmeter:12:rev'".into(),
            format!("HERDSMAN_PORT={}", lane_port(None, "sendmeter/rev-12-r1", &[]))
        ]
    );
    let start = words(&p["commands"][3]);
    assert_eq!(
        start[start.len() - 4..],
        [
            "--".to_string(),
            "-p".into(),
            "fleet-rev".into(),
            "--accept-hooks".into()
        ]
    );
    assert!(start.contains(&"hermes".to_string()));
    assert!(words(&p["commands"][4]).starts_with(&["herdr".into(), "pane".into(), "wait-output".into()]));
    let kick = words(&p["commands"][5]);
    assert_eq!(
        kick[..4],
        ["herdr", "agent", "prompt", "sendmeter-rev-12-r1"].map(String::from)
    );
    assert_eq!(kick[4], p["kick"].as_str().unwrap());
    assert_eq!(p["base"], "origin/issue-12");
}

#[test]
fn real_claude_spawn_writes_brief_and_logs_event() {
    let d = tmpdir("spawn-real");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    // Without a synced profile a claude spawn is refused.
    let (code, _) = run(&c, &Fake::default(), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 1);
    let (code, _) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0);
    let settings = std::fs::read_to_string(c.config.parent().unwrap().join("claude/fleet-impl/settings.json")).unwrap();
    let s: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(
        s["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "'/opt/herdsman/bin/herdsman' hook --role impl"
    );
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("git -C", "")
        .reply(
            "herdr worktree open",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list --workspace w9",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run w9:p1 export HERDSMAN_LANE='sendmeter:12:impl'", "{}")
        .reply("herdr agent start sendmeter-impl-12 --kind claude --pane w9:p1", "{}")
        .reply("herdr agent wait sendmeter-impl-12", "{}");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    // #24: the lane's shell exports HERDSMAN_LANE before the harness starts, arming `herdsman hook`.
    assert!(r.index("herdr pane run w9:p1 export").unwrap() < r.index("herdr agent start").unwrap());
    // #194: a reopened worktree keeps the lane's name, not herdr's branch label.
    let open = r
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("herdr worktree open"))
        .cloned()
        .unwrap();
    assert!(open.ends_with("--label sendmeter-impl-12"), "{open}");
    assert!(r.called("herdr workspace rename w9 sendmeter-impl-12"));
    // #19: the title comes from the spec fetched at spawn, not a stale or missing cache.
    let calls = r.calls.borrow();
    let start = calls.iter().find(|c| c.starts_with("herdr agent start")).unwrap();
    assert!(
        start.contains("--name sendmeter · impl #12 · Fix the meter --mcp-config"),
        "{start}"
    );
    drop(calls);
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains("# ROLE: IMPLEMENTER") && brief.contains("AC: it works") && brief.contains("Closes #12"));
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(
        (st.state.as_str(), st.lane.as_deref()),
        ("impl", Some("sendmeter-impl-12"))
    );
    // #187: the spawn event records the lane's port.
    let ev = read(&c.state)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(ev.data["port"], lane_port(None, "sendmeter/issue-12", &[]));
}

#[test]
fn unconfirmed_hermes_kick_presses_enter_then_exits_4() {
    let d = tmpdir("spawn-unconfirmed");
    let c = ctx(&d, CONFIG);
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply(
            "herdr worktree open",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane send-keys w9:p1 Enter", "{}");
    let (code, _) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 4);
    assert_eq!(r.count("herdr agent prompt sendmeter-impl-12 Read "), 3, "3 attempts");
    assert_eq!(r.count("herdr pane send-keys w9:p1 Enter"), 3);
    assert_eq!(r.count("sleep 10"), 2, "attempts spaced ≥10 s apart");
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.last().unwrap().data["confirmed"], false);
}

#[test]
fn help_agent_doc_is_short_and_complete() {
    let d = tmpdir("help");
    let c = ctx(&d, CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["help", "--agent"]);
    assert_eq!(code, 0);
    assert!(
        out.lines().count() <= herdsman::HELP_AGENT_MAX,
        "{} lines",
        out.lines().count()
    );
    for w in [
        "herdsman spawn",
        "herdsman merge",
        "herdsman verdict",
        "herdsman set",
        "herdsman status",
        "herdsman profiles sync",
        "herdsman timer install",
        "herdsman check",
        "herdsman clean",
        "herdsman adopt",
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
        "herdsman stop",
        "herdsman kill",
        "--lane",
        "SECRET_IN_WIP",
    ] {
        assert!(out.contains(w), "missing {w}");
    }
    assert!(!out.contains("herdsman hook"));
}

/// #2 AC1: a role's effort (or the `[limits]` default) is mapped per harness in the launch argv.
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
    let p = plan(&c, &Fake::default(), "impl");
    let start = words(&p["commands"][3]);
    let at = start.iter().position(|w| w == "--effort").expect("--effort");
    assert_eq!(start[at + 1], "high");
    assert_eq!(p["effort"], "high");
    // rev sets none: the [limits] default applies, as Hermes `--reasoning`.
    let start = words(&plan(&c, &Fake::default(), "rev")["commands"][3]);
    assert_eq!(start[start.len() - 2..], ["--reasoning", "xhigh"].map(String::from));

    // Claude `none` omits the flag; Hermes passes it through.
    let d = tmpdir("effort-none");
    let c = ctx(
        &d,
        &cfg.replace("effort = \"high\"", "effort = \"none\"")
            .replace("effort = \"xhigh\"", "effort = \"none\""),
    );
    assert!(!words(&plan(&c, &Fake::default(), "impl")["commands"][3]).contains(&"--effort".to_string()));
    let start = words(&plan(&c, &Fake::default(), "rev")["commands"][3]);
    assert_eq!(start[start.len() - 2..], ["--reasoning", "none"].map(String::from));

    // No effort anywhere: no flag at all.
    let d = tmpdir("effort-unset");
    let c = ctx(&d, CONFIG);
    let start = words(&plan(&c, &Fake::default(), "rev")["commands"][3]);
    assert!(!start.contains(&"--reasoning".to_string()), "{start:?}");
}

/// #2 AC1: an unknown level, or one the role's harness lacks, is refused at config load.
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
        let err = herdsman::config::Config::load(&c.config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(want), "{msg}");
        let (code, _) = run(&c, &Fake::default(), &["spawn", "sendmeter", "12", "rev", "--dry-run"]);
        assert_eq!(code, 1);
    }
}

/// #19: Claude lanes get `--name "<fleet> · <role> #<issue> · <short title>"`.
#[test]
fn claude_dry_run_names_every_role() {
    let d = tmpdir("spawn-name");
    let cfg = CONFIG
        .replace("harness = \"hermes\"", "harness = \"claude\"")
        .replace("harness  =  \"hermes\"", "harness = \"claude\"")
        + "\n[fleet.sendmeter.role.design]\nharness = \"claude\"\nprofile = \"fleet-design\"\n";
    let c = ctx(&d, &cfg);
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("issue-sendmeter-12.json"),
        r#"{"title":"feat(meter): fix the meter","body":""}"#,
    )
    .unwrap();
    let name = |args: &[&str]| {
        let (code, out) = run(&c, &Fake::default(), args);
        assert_eq!(code, 0, "{out}");
        let p: Value = serde_json::from_str(&out).unwrap();
        let start = words(
            p["commands"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c[2] == "start")
                .unwrap(),
        );
        let at = start.iter().position(|w| w == "--name").expect("--name");
        assert_eq!(start[at + 1], p["title"].as_str().unwrap());
        start[at + 1].clone()
    };
    let dry = |n: &str, role: &str| name(&["spawn", "sendmeter", n, role, "--dry-run", "--json"]);
    assert_eq!(dry("12", "impl"), "sendmeter · impl #12 · fix the meter");
    assert_eq!(dry("12", "rev"), "sendmeter · rev #12 r1 · fix the meter");
    assert_eq!(dry("12", "design"), "sendmeter · design #12 · fix the meter");
    // No cached spec (a dry run never calls GitHub): the head alone.
    assert_eq!(dry("13", "impl"), "sendmeter · impl #13");
}

#[test]
fn lane_title_strips_prefixes_and_cuts_at_words() {
    use herdsman::spawn::lane_title;
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
    // Thai: kept whole when it fits; cut by chars (never bytes) when it does not.
    assert_eq!(lane_title(h, "แก้ไขปุ่มชำระเงิน"), "synergy · impl #2840 · แก้ไขปุ่มชำระเงิน");
    let thai = lane_title(h, &"ภาษาไทย".repeat(10));
    assert!(thai.ends_with('…') && thai.chars().count() <= 60, "{thai}");
    assert!(thai.starts_with("synergy · impl #2840 · ภาษาไทยภาษาไทย"));
}
