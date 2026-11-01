//! AC5: spawn launches the right harness with the synced profile.
mod common;
use common::*;
use herdsman::events::{fold, read};
use serde_json::Value;

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
        ["pane", "run", "<pane>", "export", "HERDSMAN_LANE='sendmeter:12:impl'"]
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
            format!("CARGO_TARGET_DIR='{home}/.cache/herdsman/target/sendmeter'"),
            "npm_config_cache='/c/npm'".into(),
        ]
    );
}

/// #103: `clone_node_modules` needs some lockfile, and every lockfile identical in both checkouts.
#[test]
fn lockfiles_match_needs_an_identical_lockfile() {
    let d = tmpdir("lockfiles");
    let (local, wt) = (d.join("local"), d.join("wt"));
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&wt).unwrap();
    let m = || herdsman::spawn::lockfiles_match(&local, &wt);
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
fn hermes_role_dry_run_kicks_prompt_after_boot() {
    let d = tmpdir("spawn-hermes");
    let c = ctx(&d, CONFIG);
    let p = plan(&c, &Fake::default(), "rev");
    assert_eq!(
        words(&p["commands"][2])[4..],
        ["export", "HERDSMAN_LANE='sendmeter:12:rev'"]
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
    // The combined doc (184 lines at #43 + #46) plus 5 lines of slack, plus #95's `close` verb (3 lines).
    assert!(out.lines().count() <= 192, "{} lines", out.lines().count());
    for w in [
        "herdsman spawn",
        "herdsman merge",
        "herdsman verdict",
        "herdsman set",
        "herdsman status",
        "herdsman profiles sync",
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

/// #19: Claude lanes get `--name "<fleet> · <role> #<issue> · <short title>"`; orch is `<fleet> · orch`.
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
    assert_eq!(dry("-", "orch"), "sendmeter · orch");
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
