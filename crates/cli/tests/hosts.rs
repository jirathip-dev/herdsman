mod common;
use common::*;
use drover::config::Config;
use drover::events::{append, read, Event};
use drover::hosts::{cached, place};
use drover::serve::Server;
use serde_json::json;

const NOW: u64 = 1_000_000;
const SSH: &str = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C -oControlPersist=10m bazzite -- ";
const WT: &str = "/home/me/.local/share/drover/work/sendmeter/issue-12";
const ADDR: &str = "127.0.0.1:7777";
const TOKEN: &str = concat!("sk", "-ant-oat01-AAAA-secret");

fn config(extra: &str) -> String {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    format!("{base}\n[hosts.mini]\ntags = [\"fast\"]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n{extra}")
}

fn setup(name: &str, extra: &str, bazzite_up: bool, seen: u64) -> drover::Ctx {
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

fn fake(mini: f64, remote: f64) -> Fake {
    Fake::default()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("sysctl -n vm.loadavg", &format!("{{ {} 1 1 }}", mini * 4.0))
        .reply("sysctl -n hw.ncpu", "4")
        .reply(
            &format!("{SSH}'cat' '/proc/loadavg'"),
            &format!("{} 1 1 1/9 42", remote * 4.0),
        )
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/drover/bin/drover' 'profiles' 'sync' '--profile'"),
            "",
        )
}

fn two<'a>(f: &'a Fake, bazzite: &'a FakeShed) -> OverSsh<'a> {
    OverSsh {
        f,
        hosts: vec![("bazzite", bazzite)],
    }
}

fn shed_call_at(r: &Fake, method: &str) -> Option<usize> {
    let calls = r.calls.borrow();
    let at = calls.iter().enumerate().filter(|(_, c)| c.contains("'shed' 'connect'"));
    let inputs = r.inputs.borrow();
    let sent = inputs.iter().filter(|(c, _)| c.contains("'shed' 'connect'"));
    let want = format!("\"method\":\"{method}\"");
    at.zip(sent).find(|(_, (_, i))| i.contains(&want)).map(|((n, _), _)| n)
}

const IMPL: &str = "sendmeter-impl-12";

fn healthy(f: Fake) -> Fake {
    f.reply(&format!("{SSH}'git' 'ls-remote'"), "abc\tHEAD\n")
        .reply(&format!("{SSH}'sh' '-lc'"), "")
        .reply(
            &format!("{SSH}'/opt/drover/bin/drover' '--version'"),
            &format!("drover {}\n", drover::VERSION),
        )
}

fn ev(c: &drover::Ctx, ts: u64, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn remote_spawn(c: &drover::Ctx, ts: u64) {
    let d = json!({"role": "impl", "agent": IMPL, "harness": "claude", "pane": IMPL, "workspace": "",
                   "worktree": WT, "branch": "issue-12", "host": "bazzite", "machine": "bazzite"});
    ev(c, ts, 12, "spawn", d);
}

fn placed(c: &drover::Ctx, r: &Fake, issue: u64, role: &str, force: Option<&str>) -> Result<String, String> {
    let cfg = Config::load(&c.config).unwrap();
    let events = read(&c.state).unwrap();
    let (mini, bazzite) = (FakeShed::new(), FakeShed::new());
    match place(
        c,
        &cfg,
        &mini.on(&two(r, &bazzite)),
        &events,
        ("sendmeter", issue, role),
        force,
    ) {
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
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let f = healthy(fake(1.0, 1.0))
        .reply("sh -c uname -s", "Darwin\narm64\n/Users/me\nxcode\n")
        .reply(
            &format!("{SSH}'sh' '-c' 'uname -s"),
            "Linux\naarch64\n/home/me\ndocker\n",
        );
    let r = two(&f, &bazzite);
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
    assert_eq!(probes(&f), 2);
    let later = |s| drover::Ctx {
        now: NOW + s,
        ..c.clone()
    };
    assert_eq!(run(&later(60), &r, &["check"]).0, 0);
    assert_eq!(probes(&f), 2, "cached within the hour");
    assert_eq!(run(&later(3600), &r, &["check"]).0, 0);
    assert_eq!(probes(&f), 4, "probed again after an hour");
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
    let c2 = setup("hosts-needs-hand", "tags = [\"gpu\"]", true, 0);
    run(&c2, &r, &["queue", "sendmeter", "add", "12", "--needs", "gpu"]);
    assert_eq!(placed(&c2, &r, 12, "impl", None).as_deref(), Ok("bazzite"));
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
    assert_eq!(
        placed(&c, &fake(0.2, 2.0), 12, "impl", Some("bazzite")).as_deref(),
        Ok("bazzite")
    );
}

#[test]
fn admission_runs_per_host() {
    let c = setup("hosts-admit", "", true, 0);
    assert_eq!(placed(&c, &fake(3.0, 2.0), 12, "impl", None).as_deref(), Ok("bazzite"));
    let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 9 8 1048576 90% /\n";
    let low = || fake(1.0, 0.1).reply(&format!("{SSH}'df' '-k' '/'"), df);
    assert_eq!(placed(&c, &low(), 12, "impl", None).as_deref(), Ok("mini"));
    let e = placed(&c, &low(), 12, "impl", Some("bazzite")).unwrap_err();
    let nothing = "nothing for `drover clean --yes` there; `drover reclaim` frees idle build folders";
    assert_eq!(e, format!("DISK_LOW: bazzite: 1.0 GB free < 10; {nothing}"));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let (_sh, bazzite, f) = (FakeShed::attached(&c), FakeShed::new(), low());
    let (code, out) = run(
        &c,
        &two(&f, &bazzite),
        &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"],
    );
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("DISK_LOW"), "{out}");
    let e = placed(&c, &fake(3.0, 4.0), 12, "impl", None).unwrap_err();
    assert!(e.starts_with("HOST_BUSY: mini: load/cpu 3.00 > 2.5"), "{e}");
}

fn with_account(c: &drover::Ctx) -> &'static str {
    let toml = std::fs::read_to_string(&c.config).unwrap().replacen(
        "harness = \"claude\"",
        "harness = \"claude\"\naccount = \"A\"",
        1,
    ) + "\n[accounts]\nA = \"CLAUDE_CODE_OAUTH_TOKEN_A\"\n";
    std::fs::write(&c.config, toml).unwrap();
    assert_eq!(run(c, &Fake::default(), &["profiles", "sync"]).0, 0);
    TOKEN
}

fn remote_fake() -> Fake {
    fake(0.1, 0.1)
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply(&format!("{SSH}'sh' '-c' 'git clone --single-branch"), "")
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply(&format!("{SSH}'sh' '-c' 'umask 077"), "")
        .reply(&format!("{SSH}'rm' '-f'"), "")
}

#[test]
fn a_remote_lane_runs_every_command_on_its_host() {
    let c = setup("hosts-spawn", "", true, 0);
    let token = with_account(&c);
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    let (sh, bazzite, r) = (FakeShed::attached(&c), FakeShed::new(), remote_fake());
    let (code, out) = run(&c, &two(&r, &bazzite), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let calls = r.calls.borrow();
    for m in ["run", "send", "wait"] {
        assert!(bazzite.called(m, IMPL), "{m}: {:?}", bazzite.methods());
        assert!(!sh.called(m, IMPL), "{m} here: {:?}", sh.methods());
    }
    assert!(
        !calls.iter().any(|c| c.starts_with("git ")),
        "git runs on bazzite: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with(&format!(
            "{SSH}'sh' '-c' 'git clone --single-branch --branch '\\''staging'\\'' \
             https://github.com/jirathip-dev/sendmeter.git '\\''/tmp/does-not-matter/sendmeter'\\''"
        ))) && !calls.iter().any(|c| c.contains("'gh' 'repo' 'clone'")),
        "#307: first use clones with plain git (the host's insteadOf/SSH applies), never gh: {calls:?}"
    );
    assert_eq!(bazzite.last("run")["cwd"], json!(WT), "worktree under bazzite's home");
    assert!(calls
        .iter()
        .any(|c| c.starts_with(&format!("{SSH}'sh' '-c' 'mkdir")) && c.ends_with(&format!("'{WT}/.brief.md'"))));
    assert!(
        !calls.iter().any(|c| c.contains("OAUTH_TOKEN") || c.contains(token)),
        "token in argv: {calls:?}"
    );
    assert!(
        !r.inputs
            .borrow()
            .iter()
            .any(|i| i.1.contains(token) || i.1.contains("OAUTH_TOKEN")),
        "{:?}",
        r.inputs.borrow()
    );
    let sent = bazzite.sent(IMPL);
    assert!(
        !sent.iter().any(|t| t.contains("OAUTH_TOKEN") || t.contains(token)),
        "token typed: {sent:?}"
    );
    let export = sent.iter().find(|t| t.starts_with("export ")).unwrap();
    assert!(
        export.contains("DROVER_ACCOUNT='A'") && !export.contains("CLAUDE_CONFIG_DIR") && !export.contains("&& . '"),
        "{export}"
    );
    let start = sent.iter().find(|t| t.starts_with("exec ")).unwrap();
    assert!(start.contains(&format!("Read {WT}/.brief.md")), "{start}");
    drop(calls);
    let e = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(
        (e.str("host"), e.str("machine"), e.str("worktree"), e.str("account")),
        (Some("bazzite"), Some("bazzite"), Some(WT), Some("A"))
    );
    assert_eq!(
        placed(&c, &fake(0.1, 3.0), 12, "rev", None).unwrap_err(),
        "HOST_BUSY: bazzite: load/cpu 3.00 > 2.5"
    );
    let (_, status) = run(&c, &two(&r, &bazzite), &["status"]);
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
    let (sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let r = fake(1.0, 1.0)
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove'"), "")
        .reply(&format!("{SSH}'rm'"), "");
    let (code, out) = run(&c, &two(&r, &bazzite), &["clean", "sendmeter", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed bazzite:{WT} (sendmeter#12 merged)")),
        "{out}"
    );
    assert!(r.called(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove' '{WT}'")));
    assert!(
        bazzite.called("kill", IMPL) && !sh.called("kill", IMPL),
        "its pane closes on bazzite"
    );
    assert!(!r.calls.borrow().iter().any(|c| c.starts_with("git ") && c.contains(WT)));
}

#[test]
fn clean_lists_and_removes_a_second_hosts_lane_and_orphan() {
    let c = setup("hosts-clean-416", "", true, 0);
    let wt = "/home/me/.herdr/worktrees/sendmeter/rev-12-r1";
    let rev = "sendmeter-rev-12-r1";
    let d = json!({"agent": rev, "role": "rev", "host": "bazzite", "machine": "bazzite", "worktree": wt, "pane": rev});
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
        .reply(&format!("{SSH}'git' '-C' '{wt}' 'worktree' 'remove'"), "")
        .reply(&format!("{SSH}'rm' '-f'"), "")
        .reply(&format!("{SSH}'rm' '-rf' '{wt}/"), "");
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let e = placed(&c, &r, 13, "impl", Some("bazzite")).unwrap_err();
    let want = "DISK_LOW: bazzite: 1.0 GB free < 10; `drover clean --yes` frees 1 lane worktree(s) there: rev-12-r1";
    assert_eq!(e, want);
    let o = two(&r, &bazzite);
    let (code, dry) = run(&c, &o, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!("would remove bazzite:{wt} (pane {rev}, sendmeter#12 merged)")),
        "{dry}"
    );
    assert!(dry.contains(&format!("orphan bazzite:{old} 1.0 GB")), "{dry}");
    assert!(
        !dry.contains("worktrees/sendmeter "),
        "a fleet's own folder is no orphan: {dry}"
    );
    let remove = format!("{SSH}'git' '-C' '{wt}' 'worktree' 'remove'");
    assert!(r.called(&format!("{SSH}'sh' '-c' 'cd && cd \"$1\"")) && !r.called(&remove));
    let (code, out) = run(&c, &o, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed bazzite:{wt} (sendmeter#12 merged)")),
        "{out}"
    );
    assert!(r.called(&remove));
    assert!(
        out.contains(&format!("orphan bazzite:{old}")) && !r.called(&format!("{SSH}'rm' '-rf' '--'")),
        "{out}"
    );
    let (code, out) = run(&c, &o, &["clean", "--orphans", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("removed orphan bazzite:{old}")), "{out}");
    assert!(r.called(&format!("{SSH}'rm' '-rf' '--' '{old}'")));
}

#[test]
fn an_offline_host_never_pages_and_shows_grey() {
    let c = setup("hosts-offline", "", false, 1980);
    remote_spawn(&c, NOW - 4000);
    ev(
        &c,
        NOW - 4000,
        13,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-13", "branch": "issue-13"}),
    );
    let sh = FakeShed::attached(&c);
    let r = Fake::default();
    let (code, out) = run(&c, &r, &["check"]);
    assert_eq!(code, 0, "{out}");
    let pages: Vec<_> = read(&c.state)
        .unwrap()
        .to_vec()
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
    let w = sh.on(&r);
    let s = Server::new(&c, &w, Some(ADDR.into()));
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
    remote_spawn(&c, NOW - 60);
    let sh = FakeShed::attached(&c);
    let w = sh.on(&r);
    let s = Server::new(&c, &w, Some(ADDR.into()));
    let page = host_tab(&s, ADDR, NOW);
    assert!(
        page.contains("<span class=\"hw m\">off · draining 1 lane</span>"),
        "draining shown"
    );
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
fn after_hosts_off_the_issue_leaves_its_last_host() {
    let c = setup("hosts-off-sticky", "", true, 0);
    let r = fake(2.0, 0.1);
    remote_spawn(&c, NOW - 60);
    assert_eq!(
        placed(&c, &r, 12, "impl", None).as_deref(),
        Ok("bazzite"),
        "sticky while on"
    );
    assert_eq!(run(&c, &r, &["hosts", "off", "bazzite"]).0, 0);
    assert_eq!(placed(&c, &r, 12, "impl", None).as_deref(), Ok("mini"));
    assert_eq!(placed(&c, &r, 12, "rev", None).as_deref(), Ok("mini"));
}

#[test]
fn a_sticky_host_refusing_disk_low_yields_unless_its_worktree_has_unpushed_work() {
    let c = setup("hosts-sticky-disk", "", true, 0);
    remote_spawn(&c, NOW - 60);
    let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/sda 9 8 1048576 90% /\n";
    let low = || fake(2.0, 0.1).reply(&format!("{SSH}'df' '-k' '/'"), df);
    assert_eq!(placed(&c, &low(), 12, "impl", None).as_deref(), Ok("mini"));
    let kept = low()
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "2\n");
    let e = placed(&c, &kept, 12, "impl", None).unwrap_err();
    assert!(e.starts_with("DISK_LOW: bazzite: 1.0 GB free < 10;"), "{e}");
    assert!(
        e.ends_with("; stays on bazzite: its worktree there has unpushed work"),
        "{e}"
    );
    let e = placed(&c, &low(), 12, "impl", Some("bazzite")).unwrap_err();
    assert!(e.starts_with("DISK_LOW: bazzite"), "{e}");
}

#[test]
fn cli_and_board_toggles_both_log() {
    let c = setup("hosts-toggle", "", true, 0);
    let (sh, bazzite, f) = (FakeShed::attached(&c), FakeShed::new(), fake(1.0, 1.0));
    let r = two(&f, &bazzite);
    let w = sh.on(&r);
    assert_eq!(
        run(&c, &r, &["hosts", "off", "bazzite"]),
        (0, "bazzite: off for new lanes\n".into())
    );
    assert!(std::fs::read_to_string(&c.config)
        .unwrap()
        .contains("machine = \"bazzite\"\nenabled = false\n"));
    let last = |c: &drover::Ctx| read(&c.state).unwrap().to_vec().pop().unwrap();
    let e = last(&c);
    assert_eq!(
        (e.kind.as_str(), e.str("host"), e.str("by")),
        ("host-off", Some("bazzite"), Some("cli"))
    );
    assert_eq!(run(&c, &r, &["hosts", "off", "nope"]).0, 1);
    let s = Server::new(&c, &w, Some(ADDR.into()));
    let get = || host_tab(&s, ADDR, NOW);
    let page = get();
    assert!(
        page.contains("<span class=\"hw m\">off · not used for lanes</span>")
            && page.contains("name=to value=on><button data-confirm=\"turn bazzite on?\">on</button>")
    );
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
    let off = "data-confirm=\"turn bazzite off? no new lanes; 0 running lanes finish\">off</button>";
    assert!(get().contains(off));
}

#[test]
fn lane_cards_and_the_herd_name_the_host() {
    let c = setup("hosts-cards", "", true, 0);
    remote_spawn(&c, NOW - 60);
    let (sh, bazzite, f) = (FakeShed::attached(&c), FakeShed::new(), Fake::default());
    bazzite.lane(IMPL, "working", WT);
    let r = two(&f, &bazzite);
    let w = sh.on(&r);
    let s = Server::new(&c, &w, Some(ADDR.into()));
    let page =
        String::from_utf8_lossy(&s.respond(&format!("GET /?all=1 HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), NOW)).into_owned();
    let page = with_queue(&page, &s, ADDR, NOW);
    let card = &page[page.find("data-agent=\"sendmeter-impl-12\"").expect("lane row")..];
    assert!(
        card[..card.find("</div></div>").unwrap()].contains("impl · bazzite · round 0 · claude"),
        "{card}"
    );
    let herd = String::from_utf8_lossy(&s.respond(&format!("GET /herd.json HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), NOW))
        .into_owned();
    assert!(herd.contains("\"host\":\"bazzite\""), "{herd}");
}

fn health_of(c: &drover::Ctx, f: &Fake, tags: &[String]) -> Vec<String> {
    let cfg = Config::load(&c.config).unwrap();
    let (here, there) = (FakeShed::new(), FakeShed::new());
    let over = two(f, &there);
    let r = here.on(&over);
    drover::hosts::health(c, &cfg, &drover::hosts::On::new(&r, Some("bazzite")), tags, "/home/me")
}

fn health_with(c: &drover::Ctx, bad: &str) -> Vec<String> {
    health_of(c, &healthy(fake(0.1, 0.1)).once(bad, 1, ""), &["docker".into()])
}

#[test]
fn each_health_check_says_what_failed_and_how_to_fix_it() {
    let c = setup("hosts-health", "", true, 0);
    assert!(health_with(&c, "nothing").is_empty(), "all ✓");
    let cases = [
        (format!("{SSH}'git' 'ls-remote'"), "✗ git sendmeter — can't reach jirathip-dev/sendmeter: no stderr → add the host's SSH key to GitHub and set `git config --global url.git@github.com:.insteadOf https://github.com/`"),
        (format!("{SSH}'sh' '-lc' 'gh auth status'"), "✗ gh — logged out → ssh bazzite gh auth login"),
        (format!("{SSH}'/opt/drover/bin/drover'"), "! drover — missing there, drover 0.1.0"),
    ];
    for (bad, want) in cases {
        let rows = health_with(&c, &bad);
        assert!(rows[0].starts_with(want), "{bad}: {rows:?}");
    }
    let r = healthy(fake(0.1, 0.1)).once(&format!("{SSH}'sh' '-lc' 'claude auth status'"), 1, "");
    let rows = health_of(&c, &r, &[]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].starts_with("✗ claude — logged out → claude auth login"));
    let cfg = Config::load(&c.config).unwrap();
    let (here, f) = (FakeShed::new(), healthy(fake(0.1, 0.1)));
    let r = here.on(&f);
    let rows = drover::hosts::health(&c, &cfg, &drover::hosts::On::new(&r, Some("bazzite")), &[], "/h");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].starts_with("! shed: "),
        "a shed that does not answer only warns: {rows:?}"
    );
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let r = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .once(&format!("{SSH}'git' 'ls-remote'"), 128, "");
    let (code, out) = run(&c, &two(&r, &bazzite), &["doctor"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("\n✗ host bazzite git sendmeter — can't reach jirathip-dev/sendmeter"),
        "{out}"
    );
    let (_, out) = run(&c, &two(&healthy(r), &bazzite), &["doctor"]);
    assert!(
        out.contains("\n✓ host bazzite — git, gh, claude, drover 0.1.0 ("),
        "{out}"
    );
}

#[test]
fn doctor_flags_a_stale_remote_fleet_toml_and_leaves_it() {
    let c = setup("hosts-stale-conf", "", true, 0);
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let r = healthy(fake(0.1, 0.1)).reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n");
    let (_, out) = run(&c, &two(&r, &bazzite), &["doctor"]);
    assert!(!out.contains("stale fleet.toml"), "{out}");
    let conf = c.config.display().to_string();
    let r = r.reply(&format!("{SSH}'sh' '-c' 'test -e \"$1\"' 'sh' '{conf}'"), "");
    let (_, out) = run(&c, &two(&r, &bazzite), &["doctor"]);
    assert!(out.contains("\n! stale fleet.toml on bazzite: no longer read"), "{out}");
    assert!(!r.calls.borrow().iter().any(|l| l.contains("'rm'") && l.contains(&conf)));
}

#[test]
fn doctor_names_the_failed_host_check_and_says_unreachable_only_for_ssh() {
    let c = setup("hosts-doctor-why", "", true, 0);
    let _sh = FakeShed::attached(&c);
    let r = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .once_err(&format!("{SSH}'shed' 'connect'"), 1, "shed: no server on bazzite");
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(
        out.contains("\n! host bazzite shed: ") && out.contains("no server on bazzite"),
        "{out}"
    );
    assert!(!out.contains("unreachable"), "{out}");
    let mut r = fake(0.1, 0.1);
    r.replies.retain(|p| !p.0.starts_with(SSH));
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(out.contains("\n✗ host bazzite — unreachable (ssh: "), "{out}");
}

#[test]
fn an_unhealthy_host_takes_no_lanes_and_shows_red() {
    let c = setup("hosts-sick", "", true, 0);
    let mut all = cached(&c);
    all.get_mut("bazzite").unwrap().health = vec!["✗ gh — logged out → ssh bazzite gh auth login".into()];
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
        "NO_HOST: bazzite: unhealthy: ✗ gh — logged out → ssh bazzite gh auth login"
    );
    let cfg = Config::load(&c.config).unwrap();
    let s = &cached(&c)["bazzite"];
    let (tone, words) = drover::hosts::shown(cfg.hosts["bazzite"].enabled, s, 0, NOW);
    assert_eq!(
        (tone, words.as_str()),
        ("r", "unhealthy · gh — logged out → ssh bazzite gh auth login")
    );
}

#[test]
fn a_host_that_cannot_reach_a_repo_is_skipped_for_that_fleet_only() {
    let c = setup("hosts-reach", "", true, 0);
    let r = healthy(fake(0.1, 0.1)).once_err(
        &format!("{SSH}'git' 'ls-remote'"),
        128,
        "remote: Repository not found.\nfatal: repository not found\n",
    );
    let rows = health_of(&c, &r, &[]);
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

const STATUS: &str = "'sh' '-c' '\"$1\" --version";
const START: &str = "'sh' '-c' 'V=";

fn synced(status: &str) -> Fake {
    healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}'sh' '-c' 'uname"), "Linux\nx86_64\n/home/me\ndocker\n")
        .reply("sh -c uname -s", "Darwin\narm64\n/Users/me\n")
        .reply(&format!("{SSH}{STATUS}"), status)
        .reply(&format!("{SSH}{START}"), "")
}

fn syncs(c: &drover::Ctx) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "host-sync")
        .cloned()
        .collect()
}

#[test]
fn a_mismatched_host_gets_one_install_at_this_sha_then_profiles_sync_and_a_matching_one_none() {
    let c = setup("hosts-sync", "", true, 0);
    let _sh = FakeShed::attached(&c);
    let bazzite = FakeShed::new();
    let (sha, old) = (env!("DROVER_SHA"), "drover 0.1.0 (0ld)\n");
    let r = synced(old);
    let (code, out) = run(&c, &two(&r, &bazzite), &["hosts", "refresh"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("· drover 0ld ≠ {sha} ·")), "{out}");
    let repo = "https://github.com/jirathip-dev/herdsman";
    let install = format!("cargo install --git {repo} --rev {sha} --locked --root '\\''/opt/drover'\\'' --force");
    let sync = "|| '\\''/opt/drover/bin/drover'\\'' profiles sync; }; }; echo \"$? ";
    let starts: Vec<String> = r.calls.borrow().iter().filter(|l| l.contains(START)).cloned().collect();
    assert_eq!(starts.len(), 1, "{starts:#?}");
    assert!(
        starts[0].contains(&install) && starts[0].contains(sync),
        "{}",
        starts[0]
    );
    assert!(
        starts[0].ends_with("'drover 0.1.0 (0ld)'"),
        "the from version: {}",
        starts[0]
    );
    let busy = synced(&format!("{old}busy\n"));
    run(&c, &two(&busy, &bazzite), &["hosts", "refresh"]);
    assert!(busy.called(&format!("{SSH}{STATUS}")) && !busy.called(&format!("{SSH}{START}")));
    let landed = synced(&format!(
        "drover {}\ndone 0 {sha} drover 0.1.0 (0ld)\n",
        drover::VERSION
    ));
    let (_, out) = run(&c, &two(&landed, &bazzite), &["hosts", "refresh"]);
    assert!(out.contains(&format!("· drover {sha} ✓ ·")), "{out}");
    assert!(!landed.called(&format!("{SSH}{START}")));
    let ev = syncs(&c);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!(
        json!(ev[0].data),
        json!({"host": "bazzite", "from": "0ld", "to": sha, "ok": true})
    );
}

#[test]
fn a_failed_install_is_logged_alerted_once_and_retried_each_tick() {
    let c = setup("hosts-sync-fail", "", true, 0);
    let _sh = FakeShed::attached(&c);
    let bazzite = FakeShed::new();
    let sha = env!("DROVER_SHA");
    let failed = format!("drover 0.1.0 (0ld)\ndone 101 {sha} drover 0.1.0 (0ld)\n");
    for _ in 0..2 {
        let r = synced(&failed);
        run(&c, &two(&r, &bazzite), &["check"]);
        assert_eq!(r.count(&format!("{SSH}{START}")), 1, "retried");
    }
    let ev = syncs(&c);
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert!(ev.iter().all(|e| e.val("ok") == false && e.str("from") == Some("0ld")));
    let alerts = read(&c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "host-sync-failed")
        .count();
    assert_eq!(alerts, 1, "once per state");
}

#[test]
fn an_unreachable_or_switched_off_host_costs_no_sync_ssh() {
    let c = setup("hosts-sync-off", "", true, 0);
    let r = fake(0.1, 0.1).reply("sh -c uname -s", "Darwin\narm64\n/Users/me\n");
    run(&c, &r, &["hosts", "refresh"]);
    assert!(!cached(&c)["bazzite"].up);
    assert!(!r
        .calls
        .borrow()
        .iter()
        .any(|l| l.contains(".drover-sync") || l.contains(STATUS)));
    let c = setup("hosts-sync-disabled", "enabled = false\n", true, 0);
    let _sh = FakeShed::attached(&c);
    let (bazzite, r) = (FakeShed::new(), synced("drover 0.1.0 (0ld)\n"));
    run(&c, &two(&r, &bazzite), &["hosts", "refresh"]);
    assert!(!r.called(&format!("{SSH}{STATUS}")) && syncs(&c).is_empty());
}

#[test]
fn a_remote_dry_run_prints_the_argv_the_spawn_sends() {
    let c = setup("hosts-dry", "", true, 0);
    let token = with_account(&c);
    let text = std::fs::read_to_string(&c.config).unwrap().replace(
        "local = \"/tmp/does-not-matter/sendmeter\"",
        "local = \"~/src/sendmeter\"\nenv = { TOOLS = \"~/bin\" }",
    );
    std::fs::write(&c.config, text).unwrap();
    let (_sh, bazzite, f) = (FakeShed::attached(&c), FakeShed::new(), remote_fake());
    let r = two(&f, &bazzite);
    let cfg = Config::load(&c.config).unwrap();
    drover::gh::spec(&c, &f, cfg.fleet("sendmeter").unwrap(), "sendmeter", 12, true).unwrap();
    let args = ["spawn", "sendmeter", "12", "impl", "--host", "bazzite", "--json"];
    let (code, dry) = run(&c, &r, &[&args[..], &["--dry-run"]].concat());
    assert_eq!(code, 0, "{dry}");
    let pj: serde_json::Value = serde_json::from_str(&dry).unwrap();
    let mine = drover::config::home().display().to_string();
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
    assert!(
        all.contains("'git' '-C' '/home/me/src/sendmeter' 'worktree' 'add' '-b' 'issue-12'"),
        "{all}"
    );
    let typed = all.replace("'\\''", "'");
    assert!(
        typed.contains("TOOLS='/home/me/bin'") && typed.contains(&format!("Read {WT}/.brief.md")),
        "{all}"
    );
    assert!(
        cmds[0].starts_with(&format!("{SSH}'git' '-C' '/home/me/src/sendmeter' 'fetch'")),
        "{all}"
    );
    assert!(cmds[1..].iter().all(|c| c.starts_with(SSH)), "{all}");
    assert!(
        cmds.iter().any(|c| c.starts_with(&format!("{SSH}'shed' 'run' "))),
        "{all}"
    );
    let verify = format!("{SSH}'git' '-C' '/home/me/src/sendmeter' 'rev-parse' '--verify'");
    let new = drover::runner::Out {
        code: 1,
        ..Default::default()
    };
    f.onces.borrow_mut().push((verify, new));
    let n = f.calls.borrow().len();
    let (code, out) = run(&c, &r, &args);
    assert_eq!(code, 0, "{out}");
    let sent = f.calls.borrow()[n..].to_vec();
    for c in &cmds {
        let Some(req) = c.strip_prefix(&format!("{SSH}'shed' '")) else {
            assert!(sent.contains(c), "planned but not sent: {c}\nsent: {sent:#?}");
            continue;
        };
        let (method, params) = req.split_once("' '").unwrap();
        let params: serde_json::Value =
            serde_json::from_str(&params.trim_end_matches('\'').replace("'\\''", "'")).unwrap();
        let got = bazzite.all(method);
        assert!(got.contains(&params), "planned but not sent: {c}\nsent: {got:#?}");
    }
}

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
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let (code, out) = run(
        &c,
        &two(&r, &bazzite),
        &["spawn", "sendmeter", "13", "design", "--host", "bazzite", "--dry-run"],
    );
    assert_eq!((code, out.trim()), (3, main_only));
    let main = std::fs::read_to_string(&c.config)
        .unwrap()
        .replace("host = \"bazzite\"", "host = \"main\"");
    std::fs::write(&c.config, main).unwrap();
    assert_eq!(placed(&c, &fake(2.0, 0.1), 13, "rev", None).as_deref(), Ok("mini"));
    assert_eq!(placed(&c, &r, 13, "design", None).as_deref(), Ok("mini"));
}

#[test]
fn a_remote_review_is_read_over_ssh_and_recorded() {
    let c = setup("hosts-verdict", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let rev = "/home/me/.herdr/worktrees/sendmeter/rev-12-r1";
    let lane = "sendmeter-rev-12-r1";
    let head = "a".repeat(40);
    let d = json!({"role": "rev", "agent": lane, "harness": "claude", "pane": lane, "workspace": "",
                   "worktree": rev, "branch": "rev-12-r1", "sha": head, "host": "bazzite", "machine": "bazzite"});
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
        .reply(&format!("{SSH}'git' '-C' '{rev}' 'worktree' 'remove'"), "")
        .reply(&format!("{SSH}'rm'"), "");
    let (sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let r = two(&r, &bazzite);
    let cfg = Config::load(&c.config).unwrap();
    let w = drover::waiting::scan(&c, &cfg, &sh.on(&r), &read(&c.state).unwrap(), &[], true);
    let w = w.iter().find(|(s, _)| s.issue == 12).unwrap();
    assert_eq!(w.1.what, "review done on bazzite · recording within 1 min");
    let (code, out) = run(&c, &r, &["drive", "--fast"]);
    assert_eq!(code, 0, "{out}");
    let all = read(&c.state).unwrap();
    let v = all.iter().find(|e| e.kind == "verdict").expect(&out);
    assert_eq!((v.str("verdict"), v.str("sha")), (Some("FAIL"), Some(head.as_str())));
    assert!(
        bazzite.called("kill", lane) && !sh.called("kill", lane),
        "closed on bazzite"
    );
    assert!(all.iter().any(|e| e.kind == "lane-closed"), "{out}");
}

#[test]
fn a_remote_lane_knows_the_main_host_and_answers_reach_it() {
    let c = setup("hosts-answer", "", true, 0);
    with_account(&c);
    let (sh, bazzite, r) = (FakeShed::attached(&c), FakeShed::new(), remote_fake());
    let (code, out) = run(
        &c,
        &two(&r, &bazzite),
        &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"],
    );
    assert_eq!(code, 0, "{out}");
    let sent = bazzite.sent(IMPL);
    let lane = sent.iter().find(|t| t.starts_with("export ")).unwrap();
    assert!(
        lane.contains(" DROVER_OUTBOX='1'") && !lane.contains("DROVER_MAIN"),
        "{lane}"
    );
    let d = json!({"id": "q1", "question": "Which?", "options": ["a", "b"], "agent": "sendmeter-impl-12"});
    ev(&c, NOW, 12, "needs-decision", d);
    let (seen, f) = (bazzite.lock().seen.len(), Fake::default());
    let (code, out) = run(&c, &two(&f, &bazzite), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "sendmeter q1: a -> sendmeter-impl-12\n"),
        "{:?}",
        f.calls.borrow()
    );
    let after: Vec<_> = bazzite.lock().seen[seen..]
        .iter()
        .filter(|(m, _)| ["send", "keys", "wait"].contains(&m.as_str()))
        .cloned()
        .collect();
    assert_eq!(
        after,
        [
            ("send".to_string(), json!({"name": IMPL, "text": "DECISION q1: a"})),
            ("keys".to_string(), json!({"name": IMPL, "keys": ["Enter"]})),
            (
                "wait".to_string(),
                json!({"name": IMPL, "until": ["working", "blocked"], "timeout": 20.0})
            ),
        ],
        "{after:?}"
    );
    assert!(!sh.called("send", IMPL) && !sh.called("keys", IMPL));
    let calls = f.calls.borrow();
    let other: Vec<_> = calls.iter().filter(|c| !c.contains("'shed' 'connect'")).collect();
    assert_eq!(other.len(), 1, "{calls:?}");
    assert!(f.called("sh -c nohup '/opt/drover/bin/drover' kick sendmeter 12 --why answer --now"));
}

#[test]
fn a_remote_lane_gets_the_service_url_and_its_own_token() {
    let c = setup(
        "hosts-service",
        "\n[service]\nurl = \"http://mini.tail:7778\"\n",
        true,
        0,
    );
    with_account(&c);
    let (_sh, bazzite, r) = (FakeShed::attached(&c), FakeShed::new(), remote_fake());
    let (code, out) = run(
        &c,
        &two(&r, &bazzite),
        &["spawn", "sendmeter", "12", "impl", "--host", "bazzite"],
    );
    assert_eq!(code, 0, "{out}");
    let sent = bazzite.sent(IMPL);
    let lane = sent.iter().find(|t| t.starts_with("export ")).unwrap();
    assert!(
        lane.contains(" DROVER_SERVICE='http://mini.tail:7778'") && !lane.contains("DROVER_OUTBOX"),
        "{lane}"
    );
    let token = lane
        .split(" DROVER_TOKEN='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    assert_eq!(token.len(), 64, "{lane}");
    let log = drover::events::jsonl(&c.state).unwrap();
    assert!(!log.contains(token) && !out.contains(token), "{log}\n{out}");
    let cfg = Config::load(&c.config).unwrap();
    let ask = |n: &str| drover::runner::argv(&["ask", "sendmeter", n, "Which?"]);
    let ok = drover::service::authorize(&c, &cfg, token, &ask("12"));
    assert_eq!(ok.unwrap(), "sendmeter:12:impl");
    let other = drover::service::authorize(&c, &cfg, token, &ask("13"));
    assert!(other.unwrap_err().to_string().starts_with("REFUSED TOKEN"));
}

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
    let log = "/home/me/.local/state/drover/events.jsonl";
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let f = fake(1.0, 1.0).reply(&format!("{SSH}'cat' '{log}'"), &remote);
    let r = two(&f, &bazzite);
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
    assert_eq!(*marks[0].val("upto_ts"), json!(NOW - 600));
    assert!(
        !all.iter().any(|e| e.kind == "ask-waiting"),
        "an imported ask never pushes"
    );
    let (_, out) = run(&c, &r, &["asks", "sendmeter"]);
    assert!(out.contains("q2 #12") && out.contains("Which way?"), "{out}");
}

fn live_fake() -> Fake {
    Fake::default()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("sysctl -n vm.loadavg", "{ 0.4 1 1 }")
        .reply("sysctl -n hw.ncpu", "4")
        .reply(&format!("{SSH}'cat' '/proc/loadavg'"), "0.4 1 1 1/9 42")
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/drover/bin/drover' 'profiles' 'sync' '--profile'"),
            "",
        )
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
}

fn live_on() -> FakeShed {
    let bazzite = FakeShed::new();
    bazzite.lane(IMPL, "idle", WT);
    bazzite
}

#[test]
fn a_remote_supersede_stays_on_its_host() {
    let c = setup("hosts-supersede", "", true, 0);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    remote_spawn(&c, NOW - 900);
    let (sh, bazzite) = (FakeShed::attached(&c), live_on());
    let f = live_fake()
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "");
    let (code, out) = run(
        &c,
        &two(&f, &bazzite),
        &["spawn", "sendmeter", "12", "impl", "--supersede"],
    );
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert!(bazzite.called("kill", IMPL), "{:?}", bazzite.methods());
    assert!(
        bazzite.called("run", IMPL),
        "the new lane runs there: {:?}",
        bazzite.methods()
    );
    assert!(
        sh.methods().iter().all(|m| m == "ls"),
        "nothing but listing here: {:?}",
        sh.methods()
    );
    let e = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!((e.str("host"), e.str("pane")), (Some("bazzite"), Some(IMPL)));
}

#[test]
fn a_supersede_onto_another_host_closes_the_old_lane_on_its_own() {
    let c = setup("hosts-supersede-move", "", true, 0);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    remote_spawn(&c, NOW - 900);
    let (sh, bazzite, f) = (FakeShed::attached(&c), live_on(), live_fake().reply("git ", ""));
    let args = ["spawn", "sendmeter", "12", "impl", "--supersede", "--host", "mini"];
    let (code, out) = run(&c, &two(&f, &bazzite), &args);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    assert!(sh.called("run", IMPL), "the new lane runs here: {:?}", sh.methods());
    assert!(bazzite.called("kill", IMPL), "{:?}", bazzite.methods());
    assert!(!bazzite.has(IMPL), "the old lane is gone from bazzite");
    assert!(!sh.called("kill", IMPL), "never on the main host's shed");
}

#[test]
fn a_lane_moved_off_its_last_host_starts_from_the_pushed_branch_and_its_wip() {
    let c = setup("hosts-move-wip", "enabled = false", true, 0);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    remote_spawn(&c, NOW - 900);
    ev(
        &c,
        NOW - 600,
        12,
        "lane-killed",
        json!({"role": "impl", "agent": "sendmeter-impl-12"}),
    );
    let r = fake(2.0, 0.1)
        .once("git -C /tmp/does-not-matter/sendmeter rev-parse --verify", 1, "")
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "1\n")
        .reply(&format!("{SSH}'git'"), "")
        .reply("git ", "");
    let (sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let (code, out) = run(&c, &two(&r, &bazzite), &["spawn", "sendmeter", "12", "impl"]);
    let calls = r.calls.borrow().clone();
    assert_eq!(code, 0, "{out} {calls:?}");
    let create = calls
        .iter()
        .find(|l| l.starts_with("git -C") && l.contains(" worktree add "))
        .unwrap();
    assert!(create.ends_with(" origin/issue-12"), "{create}");
    assert!(sh.called("run", IMPL), "{:?}", sh.methods());
    assert!(
        calls
            .iter()
            .any(|l| l.starts_with(&format!("{SSH}'git' '-C' '{WT}' 'push'"))),
        "{calls:?}"
    );
    let e = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(e.str("host"), Some("mini"));
    let brief = std::fs::read_to_string(std::path::Path::new(e.str("worktree").unwrap()).join(".brief.md")).unwrap();
    assert!(brief.contains("work is on `origin/wip/sendmeter-impl-12-"), "{brief}");
}

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
                &format!("builds: trimmed /home/me/.cache/drover/target/synergy/debug/ (9 KB)\n{freed}\n"),
            )
    };
    let r = low("freed 20971520");
    assert_eq!(placed(&c, &r, 12, "impl", Some("bazzite")).as_deref(), Ok("bazzite"));
    let call = r.calls.borrow().iter().find(|l| l.starts_with(&trim)).cloned().unwrap();
    assert!(
        call.ends_with("'sh' '~/.local/share/drover/work/.build' '0'"),
        "every idle profile goes: {call}"
    );
    let e = placed(&c, &low("freed 0"), 12, "impl", Some("bazzite")).unwrap_err();
    assert!(e.starts_with("DISK_LOW: bazzite: 1.0 GB free < 10;"), "{e}");
}

const QUICK: &str = "ssh -oBatchMode=yes -oConnectTimeout=5 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C -oControlPersist=10m bazzite -- 'sh' '-c' 'r=$1;";

fn trim_setup(name: &str, extra: &str) -> drover::Ctx {
    let c = setup(name, &format!("root = \"/srv/drover-485\"\n{extra}"), true, 0);
    let toml = std::fs::read_to_string(&c.config).unwrap();
    let toml = toml.replace("build_root = \"\"", "build_budget_gb = 2");
    std::fs::write(&c.config, toml).unwrap();
    c
}

const TRIMMED: &str = "builds: trimmed /home/me/.cache/drover/target/synergy/debug/ (9 KB)";

#[test]
fn remote_shared_build_folders_are_trimmed_with_a_quick_ssh_not_in_the_budget_pass() {
    let c = trim_setup("hosts-budget-trim", "");
    let cfg = Config::load(&c.config).unwrap();
    let r = fake(1.0, 0.1).reply(QUICK, &format!("{TRIMMED}\nfreed 9\n"));
    let mut out = Vec::new();
    drover::check::budget(&c, &cfg, &FakeShed::new().on(&r), &mut out).unwrap();
    assert!(
        !r.calls.borrow().iter().any(|l| l.contains("'r=$1;")),
        "{:?}",
        r.calls.borrow()
    );
    drover::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(TRIMMED), "{out}");
    let calls = r.calls.borrow();
    let trims: Vec<_> = calls.iter().filter(|l| l.contains("'r=$1;")).collect();
    assert_eq!(trims.len(), 1, "{trims:?}");
    assert!(
        trims[0].starts_with(QUICK) && trims[0].ends_with("'sh' '/srv/drover-485/.build' '2097152'"),
        "{trims:?}"
    );
}

#[test]
fn a_disabled_host_gets_no_trim_call() {
    let c = trim_setup("hosts-trim-off", "enabled = false");
    let cfg = Config::load(&c.config).unwrap();
    let r = fake(1.0, 0.1);
    let mut out = Vec::new();
    drover::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
    assert!(
        !r.calls.borrow().iter().any(|l| l.contains("bazzite")),
        "{:?}",
        r.calls.borrow()
    );
}

#[test]
fn an_unreachable_host_costs_one_short_attempt_per_pass() {
    let c = trim_setup("hosts-trim-down", "");
    let cfg = Config::load(&c.config).unwrap();
    let down = "ssh: connect to host bazzite port 22: Operation timed out";
    let r = fake(1.0, 0.1).once_err(QUICK, 255, down);
    let mut out = Vec::new();
    drover::check::trim_hosts(&c, &cfg, &r, &mut out).unwrap();
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

#[test]
fn the_tick_drives_before_it_trims_remote_hosts() {
    let c = trim_setup("hosts-tick-trim", "");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let _sh = FakeShed::attached(&c);
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

#[test]
fn a_symlinked_drover_updates_and_reads_back_its_real_install_root() {
    let mut c = setup("hosts-symlink", "", true, 0);
    let dir = c.state.parent().unwrap().parent().unwrap().canonicalize().unwrap();
    let root = dir.join("share/drover");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("local/bin")).unwrap();
    std::fs::write(root.join("bin/drover"), "").unwrap();
    std::os::unix::fs::symlink(root.join("bin/drover"), dir.join("local/bin/drover")).unwrap();
    c.exe = dir.join("local/bin/drover").display().to_string();
    let (real, link) = (root.join("bin/drover").display().to_string(), c.exe.clone());
    let cfg = Config::load(&c.config).unwrap();
    let r = healthy(fake(0.1, 0.1))
        .reply(&format!("{SSH}{STATUS}"), "drover 0.1.0 (0ld)\n")
        .reply(&format!("{SSH}{START}"), "")
        .reply(
            &format!("{SSH}'{link}' '--version'"),
            &format!("drover {}\n", drover::VERSION),
        )
        .reply(&format!("{SSH}'{real}' '--version'"), "drover 0.1.0 (0ld)\n");
    let (here, there) = (FakeShed::new(), FakeShed::new());
    let over = two(&r, &there);
    let base = here.on(&over);
    let (on, home) = (drover::hosts::On::new(&base, Some("bazzite")), drover::config::home());
    let home = home.to_str().unwrap();
    let rows = drover::hosts::health(&c, &cfg, &on, &[], home);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].starts_with("! drover — drover 0.1.0 (0ld) there"), "{rows:?}");
    assert_eq!(drover::hosts::sync(&c, &on, "bazzite", home), "drover 0.1.0 (0ld)");
    let root = format!("--root '\\''{}'\\'' --force", root.display());
    let sync = format!("|| '\\''{real}'\\'' profiles sync");
    let calls = r.calls.borrow();
    assert!(
        calls.iter().any(|c| c.contains(&root) && c.contains(&sync)),
        "{calls:#?}"
    );
    let asked = format!("'sh' '{real}'");
    assert!(
        calls.iter().any(|c| c.contains(STATUS) && c.ends_with(&asked)),
        "{calls:#?}"
    );
}

fn dirty_remote(f: Fake, push: Option<i32>) -> (Fake, String) {
    let g = format!("{SSH}'git' '-C' '{WT}' ");
    let f = match push {
        Some(code) => f.once_err(
            &format!("{g}'push'"),
            code,
            "ssh: connect to host bazzite: Connection refused",
        ),
        None => f,
    };
    let f = f
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{g}'status' '--porcelain'"), " M src/a.rs\n")
        .reply(&format!("{g}'rev-list' '--count'"), "0\n")
        .reply(&format!("{g}'diff' '--cached' '--name-only'"), "")
        .reply(&format!("{g}'diff' '--cached'"), "+fn a() {}\n")
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply("date -r", "20261107-120000\n");
    (f, g)
}

#[test]
fn kill_saves_a_remote_lanes_work_on_its_own_host() {
    let c = setup("hosts-kill-wip", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let (r, g) = dirty_remote(Fake::default(), None);
    let (sh, bazzite) = (FakeShed::attached(&c), live_on());
    let (code, out) = run(&c, &two(&r, &bazzite), &["kill", "sendmeter-impl-12", "--yes"]);
    let calls = r.calls.borrow().clone();
    assert_eq!(code, 0, "{out} {calls:?}");
    let push = format!("{g}'push' 'origin' 'HEAD:refs/heads/wip/sendmeter-impl-12-20261107-120000-");
    for step in ["'status' '--porcelain'", "'add' '-A'", "'commit' '-q' '--no-verify'"] {
        assert!(r.called(&format!("{g}{step}")), "{step}: {calls:?}");
    }
    assert!(r.index(&push).unwrap() < shed_call_at(&r, "kill").unwrap(), "{calls:?}");
    assert!(!bazzite.has(IMPL) && !sh.called("kill", IMPL), "closed on bazzite");
    assert!(!r.called("git "), "never local git: {calls:?}");
}

#[test]
fn a_supersede_saves_a_remote_lanes_dirty_work_first() {
    for fail in [false, true] {
        let c = setup(&format!("hosts-supersede-wip-{fail}"), "", true, 0);
        assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
        remote_spawn(&c, NOW - 900);
        let live = live_fake()
            .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
            .reply(&format!("{SSH}'sh' '-c' 'umask 077"), "")
            .reply(&format!("{SSH}'rm' '-f'"), "");
        let (r, g) = dirty_remote(live, fail.then_some(1));
        let (sh, bazzite) = (FakeShed::attached(&c), live_on());
        let (code, out) = run(
            &c,
            &two(&r, &bazzite),
            &["spawn", "sendmeter", "12", "impl", "--supersede"],
        );
        let calls = r.calls.borrow().clone();
        let push = format!("{g}'push' 'origin' 'HEAD:refs/heads/wip/sendmeter-impl-12-20261107-120000-");
        assert!(r.called(&push), "{calls:?}");
        if fail {
            assert!(code == 3 && out.contains("REFUSED SNAPSHOT_FAILED"), "{out}");
            assert!(
                !bazzite.called("kill", IMPL) && bazzite.has(IMPL),
                "{:?}",
                bazzite.methods()
            );
            assert!(!read(&c.state).unwrap().iter().any(|e| e.kind == "supersede"));
            continue;
        }
        assert_eq!(code, 0, "{out} {calls:?}");
        assert!(r.index(&push).unwrap() < shed_call_at(&r, "kill").unwrap(), "{calls:?}");
        assert!(
            bazzite.called("run", IMPL) && !sh.called("run", IMPL),
            "restarted on bazzite"
        );
        let brief = r
            .inputs
            .borrow()
            .iter()
            .any(|(_, t)| t.contains("wip/sendmeter-impl-12-20261107-120000-"));
        assert!(brief, "the new lane's brief names the wip branch");
    }
}

#[test]
fn kill_closes_a_remote_lanes_live_pane_and_clean_then_removes_it() {
    let c = setup("hosts-kill-664", "", true, 0);
    remote_spawn(&c, NOW - 900);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let f = fake(1.0, 1.0)
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove'"), "")
        .reply(&format!("{SSH}'rm'"), "");
    let (sh, bazzite) = (FakeShed::attached(&c), live_on());
    let r = two(&f, &bazzite);
    let (code, out) = run(&c, &r, &["kill", "sendmeter-impl-12", "--yes"]);
    let calls = f.calls.borrow().clone();
    assert_eq!(code, 0, "{out} {calls:?}");
    assert!(out.contains("killed sendmeter-impl-12"), "{out}");
    assert!(
        bazzite.called("kill", IMPL) && !sh.called("kill", IMPL),
        "the live lane closes on its host"
    );
    let kill = bazzite.methods().iter().position(|m| m == "kill").unwrap();
    assert!(
        bazzite.methods()[kill..].iter().any(|m| m == "ls"),
        "kill confirms it is gone: {:?}",
        bazzite.methods()
    );
    assert!(!bazzite.has(IMPL));
    let (code, out) = run(&c, &r, &["clean", "sendmeter", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed bazzite:{WT} (sendmeter#12 merged)")),
        "{out}"
    );
    assert!(f.called(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove'")));
}

#[test]
fn a_wip_push_whose_ref_exists_retries_under_a_new_name() {
    let c = setup("hosts-wip-664", "", true, 0);
    remote_spawn(&c, NOW - 900);
    let rejected = " ! [rejected]        HEAD -> wip/sendmeter-impl-12 (fetch first)\nerror: failed to push some refs";
    let (r, g) = dirty_remote(Fake::default(), None);
    let r = r.once_err(&format!("{g}'push'"), 1, rejected);
    let (_sh, bazzite) = (FakeShed::attached(&c), live_on());
    let (code, out) = run(&c, &two(&r, &bazzite), &["kill", "sendmeter-impl-12", "--yes"]);
    let calls = r.calls.borrow().clone();
    assert_eq!(code, 0, "{out} {calls:?}");
    let pushes: Vec<_> = calls.iter().filter(|c| c.starts_with(&format!("{g}'push'"))).collect();
    assert_eq!(pushes.len(), 2, "{calls:?}");
    assert_ne!(pushes[0], pushes[1], "the retry has a new name");
    for p in &pushes {
        assert!(p.contains("'HEAD:refs/heads/wip/sendmeter-impl-12-"), "{p}");
        assert!(
            !p.contains("force") && !p.contains("'-f'") && !p.contains("'+"),
            "never forced: {p}"
        );
    }
    let w = pushes[1].rsplit("refs/heads/").next().unwrap().trim_end_matches('\'');
    assert!(
        out.contains(&format!("killed sendmeter-impl-12: work pushed to {w}")),
        "{out}"
    );
    assert!(bazzite.called("kill", IMPL), "{:?}", bazzite.methods());
}

#[test]
fn a_worktree_whose_gitfile_points_at_another_host_is_reported_not_removed() {
    let c = setup("hosts-wronghost-664", "", true, 0);
    remote_spawn(&c, NOW - 900);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let gitdir = "/Users/jkmacmini/src/sendmeter/.git/worktrees/issue-12";
    let r = fake(1.0, 1.0)
        .reply(&format!("{SSH}'cat' '{WT}/.git'"), &format!("gitdir: {gitdir}\n"))
        .once(&format!("{SSH}'sh' '-c' 'test -e \"$1\"' 'sh' '{gitdir}'"), 1, "")
        .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "0\n")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
        .reply(&format!("{SSH}'git' '-C' '{WT}' 'worktree'"), "")
        .reply(&format!("{SSH}'rm'"), "");
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    let (code, out) = run(&c, &two(&r, &bazzite), &["clean", "sendmeter", "--yes"]);
    let calls = r.calls.borrow().clone();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("orphan (wrong host) bazzite:{WT}")), "{out}");
    assert!(out.contains(gitdir) && out.contains("fix:"), "{out}");
    assert!(!r.called(&format!("{SSH}'git' '-C' '{WT}' 'worktree'")), "{calls:?}");
    assert!(!r.called(&format!("{SSH}'rm'")), "{calls:?}");
    assert!(!out.contains("removed"), "{out}");
}

#[test]
fn clean_closes_a_killed_lanes_leftover_shell_pane_then_removes_the_worktree() {
    let c = setup("hosts-leftover-664", "", true, 0);
    remote_spawn(&c, NOW - 900);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    ev(
        &c,
        NOW - 50,
        12,
        "lane-killed",
        json!({"agent": "sendmeter-impl-12", "pane": "w9:p1", "host": "bazzite"}),
    );
    let (_sh, bazzite) = (FakeShed::attached(&c), live_on());
    let r = || {
        Fake::default()
            .reply(&format!("{SSH}'sh' '-c' 'test -e"), "")
            .reply(&format!("{SSH}'git' '-C' '{WT}' 'rev-list'"), "0\n")
            .reply(&format!("{SSH}'git' '-C' '{WT}' 'status'"), "")
            .reply(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove'"), "")
            .reply(&format!("{SSH}'rm'"), "")
    };
    let r0 = r();
    let (code, out) = run(&c, &two(&r0, &bazzite), &["clean", "sendmeter", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("live agent (idle; its pane closes in 15 min)"), "{out}");
    assert!(
        !bazzite.called("kill", IMPL) && bazzite.has(IMPL),
        "{:?}",
        bazzite.methods()
    );
    let later = drover::Ctx {
        now: c.now + 900,
        ..c.clone()
    };
    let r1 = r();
    let (code, out) = run(&later, &two(&r1, &bazzite), &["clean", "sendmeter", "--yes"]);
    assert_eq!(code, 0, "{out} {:?}", r1.calls.borrow());
    let close = shed_call_at(&r1, "kill").expect("the lane's terminal closes");
    let remove = r1.index(&format!("{SSH}'git' '-C' '{WT}' 'worktree' 'remove'"));
    assert!(close < remove.expect("removed"), "{:?}", r1.calls.borrow());
    assert!(!bazzite.has(IMPL));
    assert!(
        out.contains(&format!("removed bazzite:{WT} (sendmeter#12 merged)")),
        "{out}"
    );
}

#[test]
fn a_lane_of_the_same_name_on_another_host_keeps_no_worktree_live() {
    let c = setup("hosts-samename-664", "", true, 0);
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let w = wt.display().to_string();
    let d = json!({"role": "impl", "agent": IMPL, "pane": IMPL, "workspace": "", "worktree": w,
                   "branch": "issue-12", "host": "mini"});
    ev(&c, NOW - 900, 12, "spawn", d);
    ev(
        &c,
        NOW - 100,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let (_sh, bazzite) = (FakeShed::attached(&c), FakeShed::new());
    bazzite.lane(IMPL, "working", WT);
    let r = Fake::default()
        .reply(&format!("git -C {w} rev-list"), "0\n")
        .reply(&format!("git -C {w} status"), "");
    let (code, out) = run(&c, &two(&r, &bazzite), &["clean", "sendmeter"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("would remove {w} (pane {IMPL}, sendmeter#12 merged)")),
        "{out}"
    );
    assert!(!out.contains("live agent"), "{out}");
}
