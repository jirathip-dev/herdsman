//! #157: multi-host lanes with a fake herdr of two machines (`mini`, this host; `bazzite`, over `--machine`/`ssh`)
//! and fake probes: tags, needs and NO_HOST, least-busy placement, per-host admission, `--machine` on every lane call,
//! a remote clean, an offline host that never pages, and the on/off switch (amendment A).
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, read, Event};
use herdsman::hosts::{cached, place};
use herdsman::serve::Server;
use serde_json::json;

const NOW: u64 = 1_000_000;
const SSH: &str = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/herdsman-ssh-%C -oControlPersist=10m bazzite -- ";
const WT: &str = "/home/me/.local/share/herdsman/work/sendmeter/issue-12"; // #630
const ADDR: &str = "127.0.0.1:7777";
const TOKEN: &str = concat!("sk", "-ant-oat01-AAAA-secret");

/// CONFIG on Claude with two hosts; `extra` lands in `[hosts.bazzite]`.
fn config(extra: &str) -> String {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    format!("{base}\n[hosts.mini]\ntags = [\"fast\"]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n{extra}")
}

/// A probed cache: mini (macOS, xcode) and bazzite (Linux, docker), bazzite last seen `seen` s ago.
fn setup(name: &str, extra: &str, bazzite_up: bool, seen: u64) -> herdsman::Ctx {
    let c = ctx(&tmpdir(name), &config(extra));
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let cache = json!({
        "mini": {"tags": ["os:macos", "arch:arm64", "xcode"], "home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
        "bazzite": {"tags": ["os:linux", "arch:x86_64", "docker"], "home": "/home/me", "probed": NOW,
                    "seen": NOW - seen, "up": bazzite_up, "load": 0.1, "mem": 12.0, "disk": 200.0},
    });
    std::fs::write(st.join("hosts.json"), cache.to_string()).unwrap();
    c
}

/// Both hosts reachable; mini at `mini` load/cpu, bazzite at `remote` (load ÷ 4 cpus, Linux `/proc`).
fn fake(mini: f64, remote: f64) -> Fake {
    Fake::default()
        .stored("herdsman/all/CLAUDE_CODE_OAUTH_TOKEN_A", TOKEN)
        .reply("herdr agent list", NO_AGENTS)
        .reply("herdr --machine bazzite agent list", NO_AGENTS)
        .reply("sysctl -n vm.loadavg", &format!("{{ {} 1 1 }}", mini * 4.0))
        .reply("sysctl -n hw.ncpu", "4")
        .reply(
            &format!("{SSH}'cat' '/proc/loadavg'"),
            &format!("{} 1 1 1/9 42", remote * 4.0),
        )
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' 'profiles' 'sync' '--profile'"),
            "",
        )
}

const HERDR_STATUS: &str = "Client:\n  version: 0.9.5\n\nserver:\n  version: 0.9.5\n";

/// #235: replies that make bazzite healthy: herdr same version in its login session, git over SSH, gh and Claude
/// logged in inside a pane, and this herdsman's version.
fn healthy(f: Fake) -> Fake {
    f.reply(&format!("{SSH}'sh' '-c' 'PATH="), HERDR_STATUS)
        .reply(&format!("{SSH}'sh' '-c' 'p=$(pgrep"), "")
        .reply(&format!("{SSH}'git' 'ls-remote'"), "abc\tHEAD\n")
        .reply(
            "herdr --machine bazzite workspace create",
            r#"{"result":{"root_pane":{"pane_id":"w5:p1"}}}"#,
        )
        .reply("herdr --machine bazzite pane read w5:p1", "gh:0\nclaude:0\nhs:done\n")
        .reply(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' '--version'"),
            &format!("herdsman {}\n", herdsman::VERSION),
        )
}

fn ev(c: &herdsman::Ctx, ts: u64, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn remote_spawn(c: &herdsman::Ctx, ts: u64) {
    let d = json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "pane": "w9:p1", "workspace": "w9",
                   "worktree": WT, "branch": "issue-12", "host": "bazzite", "machine": "bazzite"});
    ev(c, ts, 12, "spawn", d);
}

/// Where a lane of `role` on #`issue` goes: the host's name, or the refusal.
fn placed(c: &herdsman::Ctx, r: &Fake, issue: u64, role: &str, force: Option<&str>) -> Result<String, String> {
    let cfg = Config::load(&c.config).unwrap();
    let events = read(&c.state).unwrap();
    match place(c, &cfg, r, &events, ("sendmeter", issue, role), force) {
        Ok(p) => {
            let p = p.unwrap();
            p.refusal
                .map_or(Ok(p.name), |(code, why)| Err(format!("{code}: {why}")))
        }
        Err(e) => Err(format!("{e:#}")),
    }
}

#[test]
fn tags_are_detected_cached_and_refreshed_hourly() {
    let c = ctx(&tmpdir("hosts-tags"), &config(""));
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let r = healthy(fake(1.0, 1.0))
        .reply("sh -c uname -s", "Darwin\narm64\n/Users/me\nxcode\n")
        .reply(
            &format!("{SSH}'sh' '-c' 'uname -s"),
            "Linux\naarch64\n/home/me\ndocker\n",
        );
    let (code, out) = run(&c, &r, &["hosts", "refresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("mini · up · tags os:macos,arch:arm64,xcode,fast · load/cpu 1.00"),
        "{out}"
    );
    assert!(
        out.contains("bazzite · up · tags os:linux,arch:arm64,docker"),
        "hand tags only where set: {out}"
    );
    assert_eq!(cached(&c)["bazzite"].home, "/home/me");
    let probes = |r: &Fake| r.count("sh -c uname") + r.count(&format!("{SSH}'sh' '-c' 'uname"));
    assert_eq!(probes(&r), 2);
    let later = |s| herdsman::Ctx {
        now: NOW + s,
        ..c.clone()
    };
    assert_eq!(run(&later(60), &r, &["check"]).0, 0);
    assert_eq!(probes(&r), 2, "cached within the hour");
    assert_eq!(run(&later(3600), &r, &["check"]).0, 0);
    assert_eq!(probes(&r), 4, "probed again after an hour");
}

#[test]
fn needs_pick_the_host_whose_tags_cover_them_else_no_host() {
    let c = setup("hosts-needs", "", true, 0);
    let r = fake(0.1, 2.0);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("mini"),
        "least busy, no needs"
    );
    run(&c, &r, &["queue", "sendmeter", "add", "12", "--needs", "docker"]);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("bazzite"),
        "only bazzite has docker"
    );
    let (_, ls) = run(&c, &r, &["queue", "sendmeter", "ls"]);
    assert!(ls.contains("#12    queued  needs docker"), "{ls}");
    assert_eq!(run(&c, &r, &["queue", "sendmeter", "set", "12", "needs", "gpu"]).0, 0);
    let e = placed(&c, &r, 12, "impl", None).unwrap_err();
    assert_eq!(e, "REFUSED NO_HOST: no host with gpu");
    // A hand tag in fleet.toml covers it.
    let c2 = setup("hosts-needs-hand", "tags = [\"gpu\"]", true, 0);
    run(&c2, &r, &["queue", "sendmeter", "add", "12", "--needs", "gpu"]);
    assert_eq!(placed(&c2, &r, 12, "impl", None).as_deref(), Ok("bazzite"));
    // The fleet's needs add to the item's: nothing has both.
    let text = config("").replace(
        "integration = \"staging\"",
        "integration = \"staging\"\nneeds = [\"xcode\"]",
    );
    std::fs::write(&c.config, text).unwrap();
    run(&c, &r, &["queue", "sendmeter", "set", "12", "needs", "docker"]);
    let e = placed(&c, &r, 12, "impl", None).unwrap_err();
    assert_eq!(e, "REFUSED NO_HOST: no host with docker, xcode");
}

#[test]
fn the_least_busy_host_wins() {
    let c = setup("hosts-busy", "", true, 0);
    assert_eq!(placed(&c, &fake(2.0, 0.2), 12, "impl", None).as_deref(), Ok("bazzite"));
    assert_eq!(placed(&c, &fake(0.2, 2.0), 12, "impl", None).as_deref(), Ok("mini"));
    // `--host` forces one.
    assert_eq!(
        placed(&c, &fake(0.2, 2.0), 12, "impl", Some("bazzite")).as_deref(),
        Ok("bazzite")
    );
}

#[test]
fn admission_runs_per_host() {
    let c = setup("hosts-admit", "", true, 0);
    // mini is over load/cpu 2.5: HOST_BUSY there, so bazzite takes the lane.
    assert_eq!(placed(&c, &fake(3.0, 2.0), 12, "impl", None).as_deref(), Ok("bazzite"));
    // bazzite has 1 GB of disk: DISK_LOW there, so mini takes it although bazzite is idle.
    let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 9 8 1048576 90% /\n";
    let low = || fake(1.0, 0.1).reply(&format!("{SSH}'df' '-k' '/'"), df);
    assert_eq!(placed(&c, &low(), 12, "impl", None).as_deref(), Ok("mini"));
    let e = placed(&c, &low(), 12, "impl", Some("bazzite")).unwrap_err();
    let nothing = "nothing for `herdsman clean --yes` there; `herdsman reclaim` frees idle build folders";
    assert_eq!(e, format!("DISK_LOW: bazzite: 1.0 GB free < 10; {nothing}"));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let (code, out) = run(&c, &low(), &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 3, "{out}");
    // Every host busy: the least busy one's refusal.
    let e = placed(&c, &fake(3.0, 4.0), 12, "impl", None).unwrap_err();
    assert!(e.starts_with("HOST_BUSY: mini: load/cpu 3.00 > 2.5"), "{e}");
}

/// The impl role gets account A (its token, in `fake`'s store, returned); its profiles synced.
fn with_account(c: &herdsman::Ctx) -> &'static str {
    let toml = std::fs::read_to_string(&c.config).unwrap().replacen(
        "harness = \"claude\"",
        "harness = \"claude\"\naccount = \"A\"",
        1,
    ) + "\n[accounts]\nA = \"CLAUDE_CODE_OAUTH_TOKEN_A\"\n";
    std::fs::write(&c.config, toml).unwrap();
    assert_eq!(run(c, &Fake::default(), &["profiles", "sync"]).0, 0);
    TOKEN
}

/// #539: before the import, a config's retired `accounts_env` file still serves the token the store lacks.
#[test]
fn a_lane_gets_its_token_from_the_retired_accounts_env_before_the_import() {
    let c = setup("hosts-legacy-token", "", true, 0);
    let token = with_account(&c);
    let env = c.config.with_file_name("tokens.env");
    std::fs::write(&env, format!("CLAUDE_CODE_OAUTH_TOKEN_A={token}\n")).unwrap();
    let toml = std::fs::read_to_string(&c.config).unwrap().replace(
        "[accounts]\n",
        &format!("[accounts]\naccounts_env = \"{}\"\n", env.display()),
    );
    std::fs::write(&c.config, toml).unwrap();
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    let r = remote_fake();
    r.store.borrow_mut().clear();
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out}");
    let sent = r
        .inputs
        .borrow()
        .iter()
        .any(|i| i.1 == format!("export CLAUDE_CODE_OAUTH_TOKEN='{token}'\n"));
    assert!(sent, "{:?}", r.inputs.borrow());
}

/// A fresh lane on bazzite: its clone, worktree, pane and agent start all answer.
fn remote_fake() -> Fake {
    let ws = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
    fake(0.1, 0.1)
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply(
            &format!("{SSH}'git' 'clone' 'https://github.com/jirathip-dev/sendmeter.git'"),
            "",
        )
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply(&format!("{SSH}'sh' '-c' 'umask 077"), "")
        .reply(&format!("{SSH}'rm' '-f'"), "")
        .reply("herdr --machine bazzite worktree create", ws)
        .reply(
            "herdr --machine bazzite pane list --workspace w9",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr --machine bazzite pane run w9:p1 export", "{}")
        .reply("herdr --machine bazzite agent start sendmeter-impl-12", "{}")
        .reply("herdr --machine bazzite agent wait sendmeter-impl-12", "{}")
}

#[test]
fn a_remote_lane_runs_every_command_on_its_host() {
    let c = setup("hosts-spawn", "", true, 0);
    // #441: the impl role has an account: its token reaches bazzite only over the ssh channel's stdin.
    let token = with_account(&c);
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    let r = remote_fake();
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let calls = r.calls.borrow();
    let herdr: Vec<_> = calls.iter().filter(|c| c.starts_with("herdr ")).collect();
    assert!(
        herdr.len() >= 6 && herdr.iter().all(|c| c.starts_with("herdr --machine bazzite ")),
        "{herdr:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("git ")),
        "git runs on bazzite: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with(&format!(
            "{SSH}'git' 'clone' 'https://github.com/jirathip-dev/sendmeter.git' '/tmp/does-not-matter/sendmeter'"
        ))) && !calls.iter().any(|c| c.contains("'gh' 'repo' 'clone'")),
        "#307: first use clones with plain git (the host's insteadOf/SSH applies), never gh: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.contains(&format!("--path {WT} "))),
        "worktree under bazzite's home"
    );
    assert!(calls
        .iter()
        .any(|c| c.starts_with(&format!("{SSH}'sh' '-c' 'mkdir")) && c.ends_with(&format!("'{WT}/.brief.md'"))));
    assert!(
        !calls.iter().any(|c| c.contains("OAUTH_TOKEN") || c.contains(token)),
        "token in argv: {calls:?}"
    );
    let write = format!("{SSH}'sh' '-c' 'umask 077 && mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"' 'sh' '{WT}.env'");
    let sent: Vec<_> = r
        .inputs
        .borrow()
        .iter()
        .filter(|i| i.1.contains(token))
        .cloned()
        .collect();
    assert_eq!(sent, [(write, format!("export CLAUDE_CODE_OAUTH_TOKEN='{token}'\n"))]);
    let export = calls.iter().position(|c| c.contains("pane run w9:p1 export")).unwrap();
    assert!(
        calls[export].ends_with(&format!("&& . '{WT}.env'")),
        "{}",
        calls[export]
    );
    let rm = calls.iter().position(|c| c == &format!("{SSH}'rm' '-f' '{WT}.env'"));
    let start = calls.iter().position(|c| c.contains("agent start")).unwrap();
    assert!(
        rm.is_some_and(|rm| rm > start),
        "the secrets file is removed after the start: {calls:?}"
    );
    let start = calls.iter().find(|c| c.contains("agent start")).unwrap();
    assert!(start.contains(&format!("Read {WT}/.brief.md")), "{start}");
    drop(calls);
    let e = read(&c.state)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(
        (e.str("host"), e.str("machine"), e.str("worktree"), e.str("account")),
        (Some("bazzite"), Some("bazzite"), Some(WT), Some("A"))
    );
    // Its review round goes to the same host (the worktree lives there).
    assert_eq!(
        placed(&c, &fake(0.1, 3.0), 12, "rev", None).unwrap_err(),
        "HOST_BUSY: bazzite: load/cpu 3.00 > 2.5"
    );
    let (_, status) = run(&c, &r, &["status"]);
    assert!(status.contains("HOST") && status.contains(" bazzite "), "{status}");
}

#[test]
fn clean_removes_a_remote_worktree_over_its_host() {
    let c = setup("hosts-clean", "", true, 0);
    remote_spawn(&c, NOW - 900);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let r = fake(1.0, 1.0)
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
        .reply("herdr --machine bazzite worktree remove --workspace w9", "{}")
        .reply("herdr --machine bazzite pane close w9:p1", "{}");
    let (code, out) = run(&c, &r, &["clean", "sendmeter", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed bazzite:{WT} (sendmeter#12 merged)")),
        "{out}"
    );
    assert!(r.called("herdr --machine bazzite worktree remove --workspace w9"));
    assert!(r.called("herdr --machine bazzite pane close w9:p1"));
    assert!(!r.calls.borrow().iter().any(|c| c.starts_with("git ")));
}

/// #416: a merged lane on bazzite that only `spawn-started` logged (its spawn failed after the worktree was made) and an
/// orphan folder there: listed with the host, the lane removed by `--yes`, the orphan by `--orphans --yes`; a full
/// bazzite's `DISK_LOW` names what `clean --yes` frees there.
#[test]
fn clean_lists_and_removes_a_second_hosts_lane_and_orphan() {
    let c = setup("hosts-clean-416", "", true, 0);
    let wt = "/home/me/.herdr/worktrees/sendmeter/rev-12-r1";
    let d = json!({"agent": "sendmeter-rev-12-r1", "role": "rev", "host": "bazzite", "machine": "bazzite",
                   "worktree": wt, "pane": "w9:p2"});
    ev(&c, NOW - 900, 12, "spawn-started", d);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let old = "/home/me/.herdr/worktrees/old-repo";
    let dirs = format!("2048\t/home/me/.herdr/worktrees/sendmeter\n1048576\t{old}\n");
    let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 9 8 1048576 99% /\n";
    let r = fake(1.0, 1.0)
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'sh' '-c' 'cd && cd"), &dirs)
        .reply(&format!("{SSH}'git' '-C' '{wt}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{wt}' 'status'"), "")
        .reply(&format!("{SSH}'rm' '-rf' '--' '{old}'"), "")
        .reply(&format!("{SSH}'df' '-k' '/'"), df)
        .reply("herdr --machine bazzite worktree remove --workspace w9", "{}")
        .reply("herdr --machine bazzite pane close w9:p2", "{}");
    let e = placed(&c, &r, 13, "impl", Some("bazzite")).unwrap_err();
    let want = "DISK_LOW: bazzite: 1.0 GB free < 10; `herdsman clean --yes` frees 1 lane worktree(s) there: rev-12-r1";
    assert_eq!(e, want);
    let (code, dry) = run(&c, &r, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!("would remove bazzite:{wt} (pane w9:p2, sendmeter#12 merged)")),
        "{dry}"
    );
    assert!(dry.contains(&format!("orphan bazzite:{old} 1.0 GB")), "{dry}");
    assert!(
        !dry.contains("worktrees/sendmeter "),
        "a fleet's own folder is no orphan: {dry}"
    );
    assert!(
        r.called(&format!("{SSH}'sh' '-c' 'cd && cd \"$1\"")) && !r.called("herdr --machine bazzite worktree remove")
    );
    let (code, out) = run(&c, &r, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed bazzite:{wt} (sendmeter#12 merged)")),
        "{out}"
    );
    assert!(r.called("herdr --machine bazzite worktree remove --workspace w9"));
    assert!(
        out.contains(&format!("orphan bazzite:{old}")) && !r.called(&format!("{SSH}'rm'")),
        "{out}"
    );
    let (code, out) = run(&c, &r, &["clean", "--orphans", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("removed orphan bazzite:{old}")), "{out}");
    assert!(r.called(&format!("{SSH}'rm' '-rf' '--' '{old}'")));
}

#[test]
fn an_offline_host_never_pages_and_shows_grey() {
    let c = setup("hosts-offline", "", false, 1980);
    remote_spawn(&c, NOW - 4000);
    // A lane on this host in the same state does page.
    ev(
        &c,
        NOW - 4000,
        13,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-13", "branch": "issue-13"}),
    );
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
    let (code, out) = run(&c, &r, &["check"]);
    assert_eq!(code, 0, "{out}");
    let pages: Vec<_> = read(&c.state)
        .unwrap()
        .into_iter()
        .filter(|e| ["lane-dead", "stall"].contains(&e.kind.as_str()))
        .collect();
    assert!(!pages.is_empty() && pages.iter().all(|e| e.issue == Some(13)), "{out}");
    let (_, hosts) = run(&c, &r, &["hosts"]);
    assert!(
        hosts.contains("bazzite · offline · last seen 33m · tags os:linux,arch:x86_64,docker"),
        "{hosts}"
    );
    assert!(hosts.contains("1 lane(s) sendmeter-impl-12"), "{hosts}");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = host_tab(&s, ADDR, NOW);
    let g = &page[page
        .find("<article class=hcd data-h=\"bazzite\"")
        .expect("a bazzite card")..];
    let g = &g[..g.find("</article>").unwrap()];
    assert!(g.contains("<span class=\"hw m\">offline · last seen 33m</span>"), "{g}");
    assert!(g.split("data-t=").skip(1).all(|t| t.starts_with('m')), "all grey: {g}");
    let mini = page.find("data-h=\"mini\"").expect("a mini card");
    assert!(mini < page.find("data-h=\"bazzite\"").unwrap(), "main first");
}

#[test]
fn an_off_host_is_never_chosen_but_drains_its_lanes() {
    let c = setup("hosts-off", "enabled = false", true, 0);
    let r = fake(2.0, 0.1);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("mini"),
        "bazzite is idler but off"
    );
    run(&c, &r, &["queue", "sendmeter", "add", "14", "--needs", "docker"]);
    assert_eq!(
        placed(&c, &r, 14, "impl", None).unwrap_err(),
        "REFUSED NO_HOST: no host with docker"
    );
    // Its running lane still gets its review and fix rounds there.
    remote_spawn(&c, NOW - 60);
    assert_eq!(placed(&c, &r, 12, "rev", None).as_deref(), Ok("bazzite"));
    assert_eq!(placed(&c, &r, 12, "impl", None).as_deref(), Ok("bazzite"));
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = host_tab(&s, ADDR, NOW);
    assert!(
        page.contains("<span class=\"hw m\">off · draining 1 lane</span>"),
        "draining shown"
    );
    // Every host off: nothing new starts.
    let text = std::fs::read_to_string(&c.config)
        .unwrap()
        .replace("tags = [\"fast\"]", "tags = [\"fast\"]\nenabled = false");
    std::fs::write(&c.config, text).unwrap();
    assert_eq!(
        placed(&c, &r, 15, "impl", None).unwrap_err(),
        "REFUSED NO_HOST: no enabled host"
    );
}

#[test]
fn cli_and_board_toggles_both_log() {
    let c = setup("hosts-toggle", "", true, 0);
    let r = fake(1.0, 1.0);
    assert_eq!(
        run(&c, &r, &["hosts", "off", "bazzite"]),
        (0, "bazzite: off for new lanes\n".into())
    );
    assert!(std::fs::read_to_string(&c.config)
        .unwrap()
        .contains("machine = \"bazzite\"\nenabled = false\n"));
    let last = |c: &herdsman::Ctx| read(&c.state).unwrap().pop().unwrap();
    let e = last(&c);
    assert_eq!(
        (e.kind.as_str(), e.str("host"), e.str("by")),
        ("host-off", Some("bazzite"), Some("cli"))
    );
    assert_eq!(run(&c, &r, &["hosts", "off", "nope"]).0, 1);
    // The board's [on] button: paired devices only.
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let get = || host_tab(&s, ADDR, NOW);
    let page = get();
    assert!(
        page.contains("<span class=\"hw m\">off · not used for lanes</span>")
            && page.contains("name=to value=on><button data-confirm=\"turn bazzite on?\">on</button>")
    );
    // #286: the main host has no [off]; the CLI refuses it.
    let mini = &page[page.find("data-h=\"mini\"").unwrap()..page.find("data-h=\"bazzite\"").unwrap()];
    assert!(!mini.contains("name=to value="), "{mini}");
    let (code, out) = run(&c, &r, &["hosts", "off", "mini"]);
    assert!(code == 3 && out.contains("REFUSED MAIN_HOST"), "{out}");
    assert!(!std::fs::read_to_string(&c.config)
        .unwrap()
        .contains("[hosts.mini]\ntags = [\"fast\"]\nenabled"));
    let i = page.find("name=mtime value=").unwrap() + 17;
    let mt: String = page[i..].chars().take_while(char::is_ascii_digit).collect();
    let body = format!("op=host&fleet=&host=bazzite&to=on&mtime={mt}");
    let post = |cookie: &str| {
        let req = format!(
            "POST / HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned()
    };
    assert!(post("").starts_with("HTTP/1.1 401"), "unpaired");
    let cookie = pair(&s, &c, "", NOW);
    let res = post(&cookie);
    assert!(res.contains("bazzite%20on") || res.contains("bazzite on"), "{res}");
    let e = last(&c);
    assert_eq!(
        (e.kind.as_str(), e.str("host"), e.str("by")),
        ("host-on", Some("bazzite"), Some("web"))
    );
    assert!(std::fs::read_to_string(&c.config).unwrap().contains("enabled = true"));
    // #286: [off] asks first and says what happens.
    let off = "data-confirm=\"turn bazzite off? no new lanes; 0 running lanes finish\">off</button>";
    assert!(get().contains(off));
}

#[test]
fn lane_cards_and_the_herd_name_the_host() {
    let c = setup("hosts-cards", "", true, 0);
    remote_spawn(&c, NOW - 60);
    let row = r#"{"result":{"agents":[{"name":"sendmeter-impl-12","agent_status":"working","pane_id":"w9:p1"}]}}"#;
    let r = Fake::default()
        .reply("herdr --machine bazzite agent list", row)
        .reply("herdr agent list", NO_AGENTS);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page =
        String::from_utf8_lossy(&s.respond(&format!("GET /?all=1 HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), NOW)).into_owned();
    let page = with_queue(&page, &s, ADDR, NOW); // #269: Lanes as the board draws it
    let card = &page[page.find("data-agent=\"sendmeter-impl-12\"").expect("lane row")..];
    assert!(
        card[..card.find("</div></div>").unwrap()].contains("impl · bazzite · round 0 · claude"),
        "{card}"
    );
    let herd = String::from_utf8_lossy(&s.respond(&format!("GET /herd.json HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), NOW))
        .into_owned();
    assert!(herd.contains("\"host\":\"bazzite\""), "{herd}");
}

/// #235: bazzite's health rows with every check passing but the one `bad` prefix, which fails.
fn health_with(c: &herdsman::Ctx, bad: &str) -> Vec<String> {
    let cfg = Config::load(&c.config).unwrap();
    let r = healthy(fake(0.1, 0.1)).once(bad, 1, "Client:\n  version: 0.9.5\n\nserver:\n  version: 0.9.0\n");
    let on = herdsman::hosts::On::new(&r, Some("bazzite"));
    herdsman::hosts::health(c, &cfg, &on, &["docker".into()], "/home/me")
}

#[test]
fn each_health_check_says_what_failed_and_how_to_fix_it() {
    let c = setup("hosts-health", "", true, 0);
    assert!(health_with(&c, "nothing").is_empty(), "all ✓");
    let cases = [
        (format!("{SSH}'sh' '-c' 'PATH="), "✗ herdr — server none, client ? → herdr update on the host"),
        (format!("{SSH}'sh' '-c' 'p=$(pgrep"), "✗ herdr session — the server runs outside the login session (keychain locked) → restart the herdr server from its launchd job: `launchctl kickstart -k gui/<uid>/<label>`"),
        (format!("{SSH}'git' 'ls-remote'"), "✗ git sendmeter — can't reach jirathip-dev/sendmeter: no stderr → add the host's SSH key to GitHub and set `git config --global url.git@github.com:.insteadOf https://github.com/`"),
        ("herdr --machine bazzite workspace create".into(), "✗ gh — logged out in a herdr pane → ssh bazzite gh auth login"),
        (format!("{SSH}'/opt/herdsman/bin/herdsman'"), "! herdsman — missing there, herdsman 0.1.0"),
    ];
    for (bad, want) in cases {
        let rows = health_with(&c, &bad);
        assert!(rows[0].starts_with(want), "{bad}: {rows:?}");
    }
    // A server older than its client (a herdr update without a restart).
    let r = healthy(fake(0.1, 0.1)).once(
        &format!("{SSH}'sh' '-c' 'PATH="),
        0,
        "Client:\n  version: 0.9.5\n\nserver:\n  version: 0.9.0\n",
    );
    let cfg = Config::load(&c.config).unwrap();
    let rows = herdsman::hosts::health(&c, &cfg, &herdsman::hosts::On::new(&r, Some("bazzite")), &[], "/h");
    assert!(rows[0].starts_with("✗ herdr — server 0.9.0, client 0.9.5"), "{rows:?}");
    // Logged out of Claude only (the pane's screen).
    let r = healthy(fake(0.1, 0.1)).once("herdr --machine bazzite pane read", 0, "gh:0\nclaude:1\nhs:done\n");
    let rows = herdsman::hosts::health(&c, &cfg, &herdsman::hosts::On::new(&r, Some("bazzite")), &[], "/h");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].starts_with("✗ claude — logged out in a herdr pane → claude auth login"));
    assert!(
        r.called("herdr --machine bazzite workspace close w5"),
        "the throwaway workspace is closed"
    );
    // doctor lists each failure under the host, and exits 1 on a ✗.
    let r = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .once(&format!("{SSH}'git' 'ls-remote'"), 128, "");
    let (code, out) = run(&c, &r, &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("\n✗ host bazzite git sendmeter — can't reach jirathip-dev/sendmeter"),
        "{out}"
    );
    let (_, out) = run(&c, &healthy(r), &["doctor"]);
    assert!(
        out.contains("\n✓ host bazzite — herdr, session, git, gh, claude, herdsman 0.1.0 ("),
        "{out}"
    );
}

/// #592: a remote host's old fleet.toml is flagged, never removed; none there, no row.
#[test]
fn doctor_flags_a_stale_remote_fleet_toml_and_leaves_it() {
    let c = setup("hosts-stale-conf", "", true, 0);
    let r = healthy(fake(0.1, 0.1)).reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n");
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(!out.contains("stale fleet.toml"), "{out}");
    let conf = c.config.display().to_string();
    let r = r.reply(&format!("{SSH}'sh' '-c' 'test -e \"$1\"' 'sh' '{conf}'"), "");
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(out.contains("\n! stale fleet.toml on bazzite: no longer read"), "{out}");
    assert!(!r.calls.borrow().iter().any(|l| l.contains("'rm'") && l.contains(&conf)));
}

#[test]
fn an_unhealthy_host_takes_no_lanes_and_shows_red() {
    let c = setup("hosts-sick", "", true, 0);
    let mut all = cached(&c);
    all.get_mut("bazzite").unwrap().health =
        vec!["✗ gh — logged out in a herdr pane → ssh bazzite gh auth login".into()];
    std::fs::write(
        c.state.with_file_name("hosts.json"),
        serde_json::to_string(&all).unwrap(),
    )
    .unwrap();
    let r = fake(2.0, 0.1);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("mini"),
        "idle but unhealthy bazzite is skipped"
    );
    run(&c, &r, &["queue", "sendmeter", "add", "12", "--needs", "docker"]);
    let e = placed(&c, &r, 12, "impl", None).unwrap_err();
    assert_eq!(
        e,
        "NO_HOST: bazzite: unhealthy: ✗ gh — logged out in a herdr pane → ssh bazzite gh auth login"
    );
    let cfg = Config::load(&c.config).unwrap();
    let s = &cached(&c)["bazzite"];
    let (tone, words) = herdsman::hosts::shown(cfg.hosts["bazzite"].enabled, s, 0, NOW);
    assert_eq!(
        (tone, words.as_str()),
        (
            "r",
            "unhealthy · gh — logged out in a herdr pane → ssh bazzite gh auth login"
        )
    );
}

/// #307: a host whose `git ls-remote` of one fleet's repo fails is skipped for that fleet only, with the reason.
#[test]
fn a_host_that_cannot_reach_a_repo_is_skipped_for_that_fleet_only() {
    let c = setup("hosts-reach", "", true, 0);
    let cfg = Config::load(&c.config).unwrap();
    let r = healthy(fake(0.1, 0.1)).once_err(
        &format!("{SSH}'git' 'ls-remote'"),
        128,
        "remote: Repository not found.\nfatal: repository not found\n",
    );
    let rows = herdsman::hosts::health(&c, &cfg, &herdsman::hosts::On::new(&r, Some("bazzite")), &[], "/h");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].starts_with("✗ git sendmeter — can't reach jirathip-dev/sendmeter: remote: Repository not found. →"),
        "{rows:?}"
    );
    let write = |health: Vec<String>| {
        let mut all = cached(&c);
        all.get_mut("bazzite").unwrap().health = health;
        std::fs::write(
            c.state.with_file_name("hosts.json"),
            serde_json::to_string(&all).unwrap(),
        )
        .unwrap();
    };
    let r = fake(2.0, 0.1);
    write(vec![rows[0]
        .replace("sendmeter", "shed")
        .replace("jirathip-dev/shed", "jirathip-dev/other")]);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("bazzite"),
        "another fleet's repo: still placed"
    );
    write(rows.clone());
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("mini"),
        "the next host takes the lane"
    );
    run(&c, &r, &["queue", "sendmeter", "add", "12", "--needs", "docker"]);
    let e = placed(&c, &r, 12, "impl", None).unwrap_err();
    assert!(
        e.starts_with("NO_HOST: bazzite: unhealthy: ✗ git sendmeter — can't reach jirathip-dev/sendmeter: remote: Repository not found."),
        "{e}"
    );
}

#[test]
fn an_older_herdsman_there_warns_and_is_rebuilt_at_this_sha_once() {
    let extra =
        "\n[fleet.herdsman]\nrepo = \"jirathip-dev/herdsman\"\nlocal = \"~/src/herdsman\"\nintegration = \"staging\"\n";
    let c = setup("hosts-update", "", true, 0);
    std::fs::write(&c.config, config("") + extra).unwrap();
    let old = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .reply("sh -c uname -s", "Darwin\narm64\n/Users/me\n")
        .reply(&format!("{SSH}'sh' '-c' 'nohup"), "")
        .once(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' '--version'"),
            0,
            "herdsman 0.1.0 (0ld)\n",
        );
    let (code, out) = run(&c, &old, &["hosts", "refresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("bazzite · up · herdsman — herdsman 0.1.0 (0ld) there"),
        "amber, not red: {out}"
    );
    let sha = env!("HERDSMAN_SHA");
    let conf = c.config.display(); // #592: the full sync only where an old fleet.toml is left
    let script = format!(
        "{SSH}'sh' '-c' 'nohup sh -c \"$1\" </dev/null >\"$HOME/.herdsman-update.log\" 2>&1 &' 'sh' 'cd '\\''/home/me/src/herdsman'\\'' && git fetch -q origin && git checkout -q --detach {sha} && PATH=\"$HOME/.cargo/bin:$PATH\" cargo install --path crates/cli --locked --root '\\''/opt/herdsman'\\'' --force && {{ test ! -e '\\''{conf}'\\'' || '\\''/opt/herdsman/bin/herdsman'\\'' profiles sync; }}'"
    );
    assert!(old.called(&script), "{:#?}", old.calls.borrow());
    let ev = read(&c.state).unwrap();
    let up = ev.iter().find(|e| e.kind == "host-update").unwrap();
    assert_eq!((up.str("host"), up.str("sha")), (Some("bazzite"), Some(sha)));
    assert!(cached(&c)["bazzite"].health[0].starts_with("! herdsman"));
    // Still old on the next probe: no second build of the same sha.
    let again = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .reply("sh -c uname -s", "Darwin\narm64\n/Users/me\n")
        .once(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' '--version'"),
            0,
            "herdsman 0.1.0 (0ld)\n",
        );
    run(&c, &again, &["hosts", "refresh"]);
    assert!(!again.called(&format!("{SSH}'sh' '-c' 'nohup")));
    // The build landed: the version reads back the same and the warning clears.
    run(
        &c,
        &healthy(fake(0.1, 0.1)).reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n"),
        &["hosts", "refresh"],
    );
    assert!(cached(&c)["bazzite"].health.is_empty());
}

/// #236: a dry run places the lane first and prints the argvs exactly as a real spawn sends them: bazzite's home in
/// every path, `--machine`, and no account token.
#[test]
fn a_remote_dry_run_prints_the_argv_the_spawn_sends() {
    let c = setup("hosts-dry", "", true, 0);
    let token = with_account(&c);
    let text = std::fs::read_to_string(&c.config).unwrap().replace(
        "local = \"/tmp/does-not-matter/sendmeter\"",
        "local = \"~/src/sendmeter\"\nenv = { TOOLS = \"~/bin\" }",
    );
    std::fs::write(&c.config, text).unwrap();
    let r = remote_fake();
    let cfg = Config::load(&c.config).unwrap();
    herdsman::gh::spec(&c, &r, cfg.fleet("sendmeter").unwrap(), "sendmeter", 12, true).unwrap();
    let args = ["spawn", "sendmeter", "12", "impl", "--host", "bazzite", "--json"];
    let (code, dry) = run(&c, &r, &[&args[..], &["--dry-run"]].concat());
    assert_eq!(code, 0, "{dry}");
    let pj: serde_json::Value = serde_json::from_str(&dry).unwrap();
    let mine = herdsman::config::home().display().to_string();
    let cmds: Vec<String> = pj["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let w: Vec<&str> = c.as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
            w.join(" ").replace("<pane>", "w9:p1")
        })
        .collect();
    let all = cmds.join("\n");
    assert!(!all.contains("OAUTH_TOKEN") && !all.contains(token), "{all}");
    assert!(pj["account"] == "A" && pj["machine"] == "bazzite", "{pj}");
    assert!(!all.contains(&format!("{mine}/")), "main host's home: {all}");
    assert!(all.contains("--cwd /home/me/src/sendmeter --branch issue-12"), "{all}");
    assert!(
        all.contains("TOOLS='/home/me/bin'") && all.contains(&format!("Read {WT}/.brief.md")),
        "{all}"
    );
    assert!(
        cmds[0].starts_with(&format!("{SSH}'git' '-C' '/home/me/src/sendmeter' 'fetch'")),
        "{all}"
    );
    assert!(
        cmds[1..].iter().all(|c| c.starts_with("herdr --machine bazzite ")),
        "{all}"
    );
    // Golden: the real spawn sends exactly these argvs.
    let n = r.calls.borrow().len();
    let (code, out) = run(&c, &r, &args);
    assert_eq!(code, 0, "{out}");
    let sent = r.calls.borrow()[n..].to_vec();
    for c in &cmds {
        assert!(sent.contains(c), "planned but not sent: {c}\nsent: {sent:#?}");
    }
}

/// #236: a design lane runs on the main host whatever the issue's history or the load says; another host is refused.
/// A role pin (`host = "<name>"` or `"main"`) beats least-busy placement.
#[test]
fn design_runs_on_the_main_host_only_and_a_role_pin_holds() {
    let c = setup("hosts-design", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let r = fake(1.0, 0.1);
    assert_eq!(placed(&c, &r, 12, "impl", None).as_deref(), Ok("bazzite"), "history");
    assert_eq!(placed(&c, &r, 12, "design", None).as_deref(), Ok("mini"));
    assert_eq!(placed(&c, &r, 13, "design", Some("mini")).as_deref(), Ok("mini"));
    let main_only = "REFUSED DESIGN_MAIN_ONLY: host bazzite: design lanes run on the main host only";
    assert_eq!(placed(&c, &r, 13, "design", Some("bazzite")).unwrap_err(), main_only);
    let text = std::fs::read_to_string(&c.config).unwrap().replace(
        "profile = \"fleet-rev\"",
        "profile = \"fleet-rev\"\nhost = \"bazzite\"\n\n[fleet.sendmeter.role.design]\nharness = \"claude\"\nhost = \"bazzite\"",
    );
    std::fs::write(&c.config, &text).unwrap();
    assert_eq!(
        placed(&c, &fake(0.1, 2.0), 13, "rev", None).as_deref(),
        Ok("bazzite"),
        "pinned"
    );
    assert_eq!(
        placed(&c, &r, 13, "design", None).unwrap_err(),
        main_only,
        "a pin can't move design"
    );
    let (code, out) = run(
        &c,
        &r,
        &["spawn", "sendmeter", "13", "design", "--host", "bazzite", "--dry-run"],
    );
    assert_eq!((code, out.trim()), (3, main_only));
    let main = text.replace("host = \"bazzite\"", "host = \"main\"");
    std::fs::write(&c.config, main).unwrap();
    assert_eq!(placed(&c, &fake(2.0, 0.1), 13, "rev", None).as_deref(), Ok("mini"));
    assert_eq!(placed(&c, &r, 13, "design", None).as_deref(), Ok("mini"));
}

/// #236: an agent that fails to start on another host leaves no half-made workspace there.
#[test]
fn a_failed_remote_start_closes_its_workspace() {
    let c = setup("hosts-fail", "", true, 0);
    with_account(&c);
    let r = remote_fake()
        .once_err("herdr --machine bazzite agent start", 1, r#"{"error":{"code":"boom"}}"#)
        .reply("herdr --machine bazzite workspace close w9", "{}");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 1, "{out}");
    assert!(r.called("herdr --machine bazzite workspace close w9"));
    assert!(!r.called("herdr --machine bazzite pane close"));
}

/// #249: a review on the remote host: its verdict.md is read over ssh and recorded, its pane closed with `--machine`
/// (one a worktree remove already closed counts), and the board names the host.
#[test]
fn a_remote_review_is_read_over_ssh_and_recorded() {
    let c = setup("hosts-verdict", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let rev = "/home/me/.herdr/worktrees/sendmeter/rev-12-r1";
    let head = "a".repeat(40);
    let d = json!({"role": "rev", "agent": "sendmeter-rev-12-r1", "harness": "claude", "pane": "w8:p1",
                   "workspace": "w8", "worktree": rev, "branch": "rev-12-r1", "sha": head, "host": "bazzite",
                   "machine": "bazzite"});
    ev(&c, NOW - 600, 12, "spawn", d);
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": head,
                    "statusCheckRollup": [{"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS"}]});
    let r = fake(1.0, 1.0)
        .reply("gh pr view issue-12", &pr.to_string())
        .reply(
            "gh api repos/jirathip-dev/sendmeter/branches/issue-12",
            &format!("{head}\n"),
        )
        .reply(
            &format!("{SSH}'cat' '{rev}/verdict.md'"),
            "FAIL\n- blocking [code] a.rs:1\n  Proof: spec\n  > AC 1\n",
        )
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'log'"), &format!("{head} 20"))
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{rev}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{rev}' 'status'"), "")
        .reply("herdr --machine bazzite worktree remove --workspace w8", "{}")
        .once_err(
            "herdr --machine bazzite pane close w8:p1",
            1,
            r#"{"error":{"code":"pane_not_found"}}"#,
        );
    let cfg = Config::load(&c.config).unwrap();
    let w = herdsman::waiting::scan(&c, &cfg, &r, &read(&c.state).unwrap(), &[], true);
    let w = w.iter().find(|(s, _)| s.issue == 12).unwrap();
    assert_eq!(w.1.what, "review done on bazzite · recording within 1 min");
    let (code, out) = run(&c, &r, &["drive", "--fast"]);
    assert_eq!(code, 0, "{out}");
    let all = read(&c.state).unwrap();
    let v = all.iter().find(|e| e.kind == "verdict").expect(&out);
    assert_eq!((v.str("verdict"), v.str("sha")), (Some("FAIL"), Some(head.as_str())));
    assert!(r.called("herdr --machine bazzite pane close w8:p1"));
    assert!(all.iter().any(|e| e.kind == "lane-closed"), "{out}");
}

/// #389: a remote lane gets an outbox, never a route to the main host (`HERDSMAN_MAIN`), and an answer to it lands
/// with `--machine`: the prompt, then Enter, then the wait that confirms it.
#[test]
fn a_remote_lane_knows_the_main_host_and_answers_reach_it() {
    let c = setup("hosts-answer", "", true, 0);
    with_account(&c);
    let r = remote_fake();
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 0, "{out}");
    let export = "herdr --machine bazzite pane run w9:p1 export";
    let calls = r.calls.borrow().clone();
    let lane = calls.iter().find(|c| c.starts_with(export)).unwrap();
    assert!(
        lane.contains(" HERDSMAN_OUTBOX='1'") && !lane.contains("HERDSMAN_MAIN"),
        "{lane}"
    );
    let d = json!({"id": "q1", "question": "Which?", "options": ["a", "b"], "agent": "sendmeter-impl-12"});
    ev(&c, NOW, 12, "needs-decision", d);
    let r = Fake::default()
        .reply(
            "herdr --machine bazzite agent prompt sendmeter-impl-12 DECISION q1: a",
            "{}",
        )
        .reply("herdr --machine bazzite pane send-keys w9:p1 Enter", "{}")
        .reply("herdr --machine bazzite agent wait sendmeter-impl-12", "{}");
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "sendmeter q1: a -> sendmeter-impl-12\n"),
        "{:?}",
        r.calls.borrow()
    );
    // #304: plus the answer's kick of #12.
    assert_eq!(r.calls.borrow().len(), 4, "{:?}", r.calls.borrow());
    assert!(r.called("sh -c nohup '/opt/herdsman/bin/herdsman' kick sendmeter 12 --why answer --now"));
}

/// #345: with `[service] url` a remote lane reports over HTTP: it gets the url and its own token, never an
/// outbox; the token is in no plan output or event, and it is scoped to its issue.
#[test]
fn a_remote_lane_gets_the_service_url_and_its_own_token() {
    let c = setup(
        "hosts-service",
        "\n[service]\nurl = \"http://mini.tail:7778\"\n",
        true,
        0,
    );
    with_account(&c);
    let r = remote_fake();
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
    assert_eq!(code, 0, "{out}");
    let calls = r.calls.borrow().clone();
    let lane = calls
        .iter()
        .find(|c| c.starts_with("herdr --machine bazzite pane run w9:p1 export"))
        .unwrap();
    assert!(
        lane.contains(" HERDSMAN_SERVICE='http://mini.tail:7778'") && !lane.contains("HERDSMAN_OUTBOX"),
        "{lane}"
    );
    let token = lane
        .split(" HERDSMAN_TOKEN='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert_eq!(token.len(), 64, "{lane}");
    let log = herdsman::events::jsonl(&c.state).unwrap();
    assert!(!log.contains(token) && !out.contains(token), "{log}\n{out}");
    let cfg = Config::load(&c.config).unwrap();
    let ask = |n: &str| herdsman::runner::argv(&["ask", "sendmeter", n, "Which?"]);
    let ok = herdsman::service::authorize(&c, &cfg, token, &ask("12"));
    assert_eq!(ok.unwrap(), "sendmeter:12:impl");
    let other = herdsman::service::authorize(&c, &cfg, token, &ask("13"));
    assert!(other.unwrap_err().to_string().starts_with("REFUSED TOKEN"));
}

/// #275: `agent_name_not_found` on another host: one more start (#359: on a fresh pane, the unconfirmed one closed)
/// when no harness came up, none when one did (it is named instead); every call on that host, and no workspace closed.
#[test]
fn a_lost_agent_name_retries_once_without_a_double_start() {
    let lost =
        r#"{"error":{"code":"agent_name_not_found","message":"named agent no longer owns the target terminal"}}"#;
    let start = "herdr --machine bazzite agent start";
    for up in [false, true] {
        let c = setup("hosts-lost", "", true, 0);
        with_account(&c);
        let pane =
            json!({"result": {"pane": {"pane_id": "w9:p1", "agent": "claude", "terminal_title": "✳ Claude Code"}}});
        let mut r = remote_fake()
            .once_err(start, 1, lost)
            .reply(
                "herdr --machine bazzite pane split w9:p1",
                r#"{"result":{"pane":{"pane_id":"w9:p2"}}}"#,
            )
            .reply("herdr --machine bazzite pane run w9:p2 export", "{}")
            .reply("herdr --machine bazzite agent rename w9:p1 sendmeter-impl-12", "{}");
        if up {
            r = r.reply("herdr --machine bazzite pane get w9:p1", &pane.to_string());
        }
        let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"]);
        assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
        assert_eq!(r.count(start), if up { 1 } else { 2 });
        assert_eq!(r.called("herdr --machine bazzite agent rename"), up);
        assert_eq!(r.called("herdr --machine bazzite pane close w9:p1"), !up);
        assert!(!r.called("herdr --machine bazzite workspace close"));
        let calls = r.calls.borrow();
        assert!(calls
            .iter()
            .filter(|c| c.starts_with("herdr "))
            .all(|c| c.starts_with("herdr --machine bazzite ")));
    }
}

/// #275: asks a lane logged on its own host before forwarding are imported into the main log once, under a new id.
/// #283: only the ones unanswered there and under a day old; `asks-imported` marks the host once.
#[test]
fn asks_stranded_on_a_remote_host_are_imported_once() {
    let c = setup("hosts-import", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let d = json!({"id": "q1", "question": "Old?", "options": [], "agent": "x"});
    ev(&c, NOW - 800, 9, "needs-decision", d);
    let ask = |ts: u64, id: &str| {
        json!({"ts": ts, "fleet": "sendmeter", "issue": 12, "type": "needs-decision",
               "id": id, "question": "Which way?", "options": ["a"], "agent": null})
    };
    let answered = json!({"ts": NOW - 650, "fleet": "sendmeter", "issue": 12, "type": "decision", "id": "q58"});
    let remote = [
        ask(NOW - 30 * 3600, "q57"),
        ask(NOW - 700, "q58"),
        answered,
        ask(NOW - 600, "q59"),
    ];
    let remote: String = remote.iter().map(|e| format!("{e}\n")).collect();
    let log = "/home/me/.local/state/herdsman/events.jsonl";
    let r = fake(1.0, 1.0).reply(&format!("{SSH}'cat' '{log}'"), &remote);
    for _ in 0..2 {
        assert_eq!(run(&c, &r, &["check"]).0, 0);
    }
    let all = read(&c.state).unwrap();
    let asks: Vec<_> = all
        .iter()
        .filter(|e| e.kind == "needs-decision" && e.issue == Some(12))
        .collect();
    assert_eq!(asks.len(), 1, "only the unanswered recent ask, imported once");
    let a = asks[0];
    assert_eq!(
        (a.str("id"), a.str("from"), a.str("agent"), a.ts),
        (Some("q2"), Some("bazzite:q59"), Some("sendmeter-impl-12"), NOW - 600)
    );
    let marks: Vec<_> = all.iter().filter(|e| e.kind == "asks-imported").collect();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].data["upto_ts"], json!(NOW - 600));
    assert!(
        !all.iter().any(|e| e.kind == "ask-waiting"),
        "an imported ask never pushes"
    );
    let (_, out) = run(&c, &r, &["asks", "sendmeter"]);
    assert!(out.contains("q2 #12") && out.contains("Which way?"), "{out}");
}

/// #275 (conductor): `agent_name_not_found` also hits the main host under load: the same retry or naming; #359: only
/// a pane that never confirmed is closed.
#[test]
fn a_lost_agent_name_on_the_main_host_retries_or_keeps_the_harness() {
    let lost = r#"{"error":{"code":"agent_name_not_found"}}"#;
    let ws = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
    for up in [false, true] {
        let c = setup("hosts-lost-main", "", true, 0);
        with_account(&c);
        let pane = json!({"result": {"pane": {"pane_id": "w9:p1", "agent": "claude", "agent_status": "idle"}}});
        let mut r = fake(0.1, 0.1)
            .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
            .reply("git -C", "")
            .reply("herdr worktree create", ws)
            .reply(
                "herdr pane list --workspace w9",
                r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
            )
            .reply("herdr pane run w9:p1 export", "{}")
            .reply("herdr pane run w9:p2 export", "{}")
            .reply("herdr pane split w9:p1", r#"{"result":{"pane":{"pane_id":"w9:p2"}}}"#)
            .reply("herdr pane close w9:p1", "{}")
            .once_err("herdr agent start", 1, lost)
            .reply("herdr agent start sendmeter-impl-12", "{}")
            .reply("herdr agent wait sendmeter-impl-12", "{}")
            .reply("herdr agent rename w9:p1 sendmeter-impl-12", "{}");
        if up {
            r = r.reply("herdr pane get w9:p1", &pane.to_string());
        }
        let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--host", "mini"]);
        assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
        assert_eq!(r.count("herdr agent start"), if up { 1 } else { 2 });
        assert_eq!(r.called("herdr agent rename"), up);
        assert_eq!(r.called("herdr pane close w9:p1"), !up);
        assert!(!r.called("herdr --machine"));
        let spawned = read(&c.state)
            .unwrap()
            .into_iter()
            .rfind(|e| e.kind == "spawn")
            .unwrap();
        assert_eq!(
            (spawned.str("host"), spawned.str("pane")),
            (Some("mini"), Some(if up { "w9:p1" } else { "w9:p2" }))
        );
    }
}

/// Both hosts idle, #12's impl lane live on bazzite in pane w9:p1.
fn live_fake() -> Fake {
    Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(
            "herdr --machine bazzite agent list",
            &agents(&[("sendmeter-impl-12", "idle", WT, "w9:p1")]),
        )
        .reply("sysctl -n vm.loadavg", "{ 0.4 1 1 }")
        .reply("sysctl -n hw.ncpu", "4")
        .reply(&format!("{SSH}'cat' '/proc/loadavg'"), "0.4 1 1 1/9 42")
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/herdsman/bin/herdsman' 'profiles' 'sync' '--profile'"),
            "",
        )
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
}

/// #327: superseding a lane on bazzite kills, reaps and reopens it over `--machine` (never the main host's herdr);
/// its reopened workspace gone under it (`workspace_not_found`) is opened again and the spawn goes on.
#[test]
fn a_remote_supersede_stays_on_its_host_and_outlives_a_gone_workspace() {
    let c = setup("hosts-supersede", "", true, 0);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    remote_spawn(&c, NOW - 900);
    let ws = |w: &str| format!(r#"{{"result":{{"workspace":{{"workspace_id":"{w}"}}}}}}"#);
    let gone = r#"{"error":{"code":"workspace_not_found","message":"workspace w9 not found"}}"#;
    let r = live_fake()
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply("herdr --machine bazzite pane close w9:p1", "{}")
        .once("herdr --machine bazzite worktree open", 0, &ws("w9"))
        .reply("herdr --machine bazzite worktree open", &ws("wA"))
        .once_err("herdr --machine bazzite pane list --workspace w9", 1, gone)
        .reply(
            "herdr --machine bazzite pane list --workspace wA",
            r#"{"result":{"panes":[{"pane_id":"wA:p1","agent":null}]}}"#,
        )
        .reply("herdr --machine bazzite pane run wA:p1 export", "{}")
        .reply("herdr --machine bazzite agent start sendmeter-impl-12", "{}")
        .reply("herdr --machine bazzite agent wait sendmeter-impl-12", "{}");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl", "--supersede"]);
    let calls = r.calls.borrow().clone();
    assert_eq!(code, 0, "{out} {calls:?}");
    let herdr: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("herdr ") && !c.starts_with("herdr agent list"))
        .collect();
    assert!(
        herdr.iter().all(|c| c.starts_with("herdr --machine bazzite ")),
        "{herdr:?}"
    );
    assert!(r.called("herdr --machine bazzite pane close w9:p1"), "{calls:?}");
    assert_eq!(r.count("herdr --machine bazzite worktree open"), 2, "{calls:?}");
    let e = read(&c.state)
        .unwrap()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!((e.str("workspace"), e.str("pane")), (Some("wA"), Some("wA:p1")));
}

/// #327: a supersede that moves the lane (`--host mini`) still closes the old lane's pane on bazzite.
#[test]
fn a_supersede_onto_another_host_closes_the_old_lane_on_its_own() {
    let c = setup("hosts-supersede-move", "", true, 0);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    remote_spawn(&c, NOW - 900);
    let r = live_fake()
        .reply("git ", "")
        .reply(
            "herdr worktree create",
            r#"{"result":{"workspace":{"workspace_id":"w1"}}}"#,
        )
        .reply(
            "herdr pane list --workspace w1",
            r#"{"result":{"panes":[{"pane_id":"w1:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run w1:p1 export", "{}")
        .reply("herdr agent start sendmeter-impl-12", "{}")
        .reply("herdr agent wait sendmeter-impl-12", "{}")
        .reply("herdr --machine bazzite pane close w9:p1", "{}");
    let args = ["spawn", "sendmeter", "12", "impl", "--supersede", "--host", "mini"];
    let (code, out) = run(&c, &r, &args);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert!(
        r.called("herdr --machine bazzite pane close w9:p1"),
        "{:?}",
        r.calls.borrow()
    );
    assert!(!r.called("herdr pane close w9:p1"), "never on the main host's herdr");
}

/// #438: short of disk on a remote host, its shared build folder is trimmed over ssh first; then its disk is read again.
#[test]
fn disk_low_on_a_remote_host_trims_its_shared_build_folder_first() {
    let c = setup("hosts-trim", "", true, 0);
    let df = |kb: u64| format!("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 9 8 {kb} 90% /\n");
    let trim = format!("{SSH}'sh' '-c' 'r=$1;");
    let low = |freed: &str| {
        fake(1.0, 0.1)
            .once(&format!("{SSH}'df' '-k' '/'"), 0, &df(1_048_576))
            .reply(&format!("{SSH}'df' '-k' '/'"), &df(30 * 1_048_576))
            .reply(
                &trim,
                &format!("builds: trimmed /home/me/.cache/herdsman/target/synergy/debug/ (9 KB)\n{freed}\n"),
            )
    };
    let r = low("freed 20971520");
    assert_eq!(placed(&c, &r, 12, "impl", Some("bazzite")).as_deref(), Ok("bazzite"));
    let call = r.calls.borrow().iter().find(|l| l.starts_with(&trim)).cloned().unwrap();
    assert!(
        call.ends_with("'sh' '~/.local/share/herdsman/work/.build' '0'"),
        "every idle profile goes: {call}"
    );
    // Nothing freed: the first reading stands.
    let e = placed(&c, &low("freed 0"), 12, "impl", Some("bazzite")).unwrap_err();
    assert!(e.starts_with("DISK_LOW: bazzite: 1.0 GB free < 10;"), "{e}");
}

/// #438/#485: remote shared build folders are trimmed to `build_budget_gb` by `trim_hosts`, one quick ssh per host;
/// drive's budget pass no longer ssh's. The build root is absent here, so the local trim never touches this host.
const QUICK: &str = "ssh -oBatchMode=yes -oConnectTimeout=5 -oControlMaster=auto -oControlPath=/tmp/herdsman-ssh-%C -oControlPersist=10m bazzite -- 'sh' '-c' 'r=$1;";

fn trim_setup(name: &str, extra: &str) -> herdsman::Ctx {
    let c = setup(name, &format!("root = \"/srv/herdsman-485\"\n{extra}"), true, 0);
    let toml = std::fs::read_to_string(&c.config).unwrap();
    let toml = toml.replace("build_root = \"\"", "build_budget_gb = 2");
    std::fs::write(&c.config, toml).unwrap();
    c
}

const TRIMMED: &str = "builds: trimmed /home/me/.cache/herdsman/target/synergy/debug/ (9 KB)";

#[test]
fn remote_shared_build_folders_are_trimmed_with_a_quick_ssh_not_in_the_budget_pass() {
    let c = trim_setup("hosts-budget-trim", "");
    let cfg = Config::load(&c.config).unwrap();
    let r = fake(1.0, 0.1).reply(QUICK, &format!("{TRIMMED}\nfreed 9\n"));
    let mut out = Vec::new();
    herdsman::check::budget(&c, &cfg, &r, &mut out).unwrap();
    assert!(
        !r.calls.borrow().iter().any(|l| l.contains("'r=$1;")),
        "{:?}",
        r.calls.borrow()
    );
    herdsman::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(TRIMMED), "{out}");
    let calls = r.calls.borrow();
    let trims: Vec<_> = calls.iter().filter(|l| l.contains("'r=$1;")).collect();
    assert_eq!(trims.len(), 1, "{trims:?}");
    assert!(
        trims[0].starts_with(QUICK) && trims[0].ends_with("'sh' '/srv/herdsman-485/.build' '2097152'"),
        "{trims:?}"
    );
}

/// #485: a host turned off gets no trim call at all.
#[test]
fn a_disabled_host_gets_no_trim_call() {
    let c = trim_setup("hosts-trim-off", "enabled = false");
    let cfg = Config::load(&c.config).unwrap();
    let r = fake(1.0, 0.1);
    let mut out = Vec::new();
    herdsman::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
    assert!(
        !r.calls.borrow().iter().any(|l| l.contains("bazzite")),
        "{:?}",
        r.calls.borrow()
    );
}

/// #485: an unreachable enabled host costs one 5 s attempt per pass, its failure printed, nothing retried.
#[test]
fn an_unreachable_host_costs_one_short_attempt_per_pass() {
    let c = trim_setup("hosts-trim-down", "");
    let cfg = Config::load(&c.config).unwrap();
    let down = "ssh: connect to host bazzite port 22: Operation timed out";
    let r = fake(1.0, 0.1).once_err(QUICK, 255, down);
    let mut out = Vec::new();
    herdsman::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out, format!("builds: bazzite: trim failed (255): {down}\n"));
    let calls = r.calls.borrow();
    let ssh: Vec<_> = calls
        .iter()
        .filter(|l| l.contains("bazzite -- 'sh' '-c' 'r=$1;"))
        .collect();
    assert_eq!(ssh, [&calls[0]], "one attempt, the quick one: {calls:?}");
    assert!(ssh[0].starts_with(QUICK));
}

/// #485: the tick drives first; the remote trim runs after drive's pass.
#[test]
fn the_tick_drives_before_it_trims_remote_hosts() {
    let c = trim_setup("hosts-tick-trim", "");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let r = fake(1.0, 0.1)
        .reply(
            "herdr status",
            "client:\n  version: 0.9.5\n\nserver:\n  status: running\n  version: 0.9.5\n",
        )
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "4\n")
        .reply("git -C", "")
        .reply(QUICK, &format!("{TRIMMED}\nfreed 9\n"));
    let (code, out) = run(&c, &r, &["tick"]);
    assert_eq!(code, 0, "{out}");
    let (driven, trimmed) = (out.find("sendmeter #12 start:"), out.find(TRIMMED));
    assert!(driven.is_some() && trimmed.is_some() && driven < trimmed, "{out}");
}
