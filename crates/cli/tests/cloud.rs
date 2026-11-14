mod common;
use common::*;
use drover::config::Config;
use drover::events::{append, read, Event};
use drover::hosts::place;
use drover::runner::{Out, Runner};
use serde_json::json;
use std::collections::BTreeMap;

const NOW: u64 = 1_000_000;
const SSH: &str = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C -oControlPersist=10m cloud1 -- ";
const WT: &str = "/home/me/.herdr/worktrees/sendmeter/issue-12";
const KEY: &str = "sekrit-key-123";
const RECIPE: &str = r#"key_env = "FAKE_CLOUD_KEY"
start = "fakecloud start {name} {size}"
stop = "fakecloud stop {name}"
status = "fakecloud status {name}"
address = "fakecloud address {name}"
destroy = "fakecloud destroy {name}"
[rate_usd_hour]
small = 1.0
"#;

fn rc(verb: &str) -> String {
    format!("sh -c IFS= read -r K; export FAKE_CLOUD_KEY=\"$K\"; unset K; fakecloud {verb} cloud1")
}

fn setup(name: &str) -> drover::Ctx {
    let dir = tmpdir(name);
    std::fs::create_dir_all(&dir).unwrap();
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    let hosts = "[hosts.mini]\n\n[hosts.cloud1]\nmachine = \"cloud1\"\ncloud = \"fake\"\nsize = \"small\"\n\
budget_usd_month = 10\nmax_hours_day = 20\ntags = [\"docker\"]\n";
    let cfg = format!("{base}\n{hosts}");
    let c = ctx(&dir, &cfg);
    std::fs::create_dir_all(c.config.with_file_name("recipes")).unwrap();
    std::fs::write(c.config.with_file_name("recipes/fake.toml"), RECIPE).unwrap();
    secret_file(&c, "FAKE_CLOUD_KEY", KEY, 0o600);
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let cache = json!({
        "mini": {"tags": ["os:macos"], "home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
        "cloud1": {"tags": ["os:linux"], "home": "/home/me", "probed": NOW, "seen": NOW - 9000, "up": false,
                   "busy": NOW - 2000},
    });
    std::fs::write(st.join("hosts.json"), cache.to_string()).unwrap();
    c
}

fn ev(c: &drover::Ctx, ts: u64, kind: &str) {
    append(
        &c.state,
        &Event::new(ts, "drover", None, kind, json!({"host": "cloud1"})),
    )
    .unwrap();
}

fn healthy(f: Fake) -> Fake {
    let gh = "HTTP/2.0 200 OK\nGitHub-Authentication-Token-Expiration: 2099-01-01 00:00:00 UTC\n\n[]";
    f.reply(&format!("{SSH}'gh' 'api' '-i'"), gh)
        .reply(&format!("{SSH}'git' 'ls-remote'"), "abc\tHEAD\n")
        .reply(&format!("{SSH}'sh' '-lc'"), "")
        .reply(
            &format!("{SSH}'/opt/drover/bin/drover' '--version'"),
            &format!("drover {}\n", drover::VERSION),
        )
}

fn fake(mini: f64) -> Fake {
    healthy(Fake::default())
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("sysctl -n vm.loadavg", &format!("{{ {} 1 1 }}", mini * 4.0))
        .reply("sysctl -n hw.ncpu", "4")
        .reply(&format!("{SSH}'cat' '/proc/loadavg'"), "0.4 1 1 1/9 42")
        .reply(&format!("{SSH}'nproc'"), "4")
        .reply(
            &format!("{SSH}'/opt/drover/bin/drover' 'profiles' 'sync' '--profile'"),
            "",
        )
        .reply(&rc("start"), "")
        .reply(&rc("status"), "running\n")
        .reply(&rc("address"), "ubuntu@10.0.0.7 2222\nssh-ed25519 AAAAC3fakekey\n")
        .reply(&rc("stop"), "")
}

fn placed(c: &drover::Ctx, r: &Fake, force: Option<&str>) -> Result<String, String> {
    let cfg = Config::load(&c.config).unwrap();
    let sh = FakeShed::new();
    match place(
        c,
        &cfg,
        &sh.on(r),
        &read(&c.state).unwrap(),
        ("sendmeter", 12, "impl"),
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

fn spawn_fake() -> Fake {
    fake(0.1)
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply(
            &format!("{SSH}'sh' '-c' 'git clone --single-branch --branch '\\''staging'\\'' https://github.com/jirathip-dev/sendmeter.git"),
            "",
        )
        .reply(&format!("{SSH}'git' '-C'"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply(&format!("{SSH}'sh' '-c' 'umask 077"), "")
        .reply(&format!("{SSH}'rm' '-f'"), "")
}

fn on_cloud<'a>(f: &'a Fake, sh: &'a FakeShed) -> OverSsh<'a> {
    OverSsh {
        f,
        hosts: vec![("cloud1", sh)],
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

#[test]
fn a_lane_only_the_cloud_fits_starts_it_refreshes_its_address_then_spawns() {
    let c = setup("cloud-start");
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let (sh, cloud) = (FakeShed::new(), FakeShed::new());
    sh.attach(&c);
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    let r = spawn_fake();
    let (code, out) = run(
        &c,
        &on_cloud(&r, &cloud),
        &["spawn", "sendmeter", "12", "impl", "--dry-run"],
    );
    assert!(!r.called(&rc("start")), "{code} {out}");
    assert!(cloud.methods().is_empty(), "{:?}", cloud.methods());
    let r = spawn_fake();
    let (code, out) = run(&c, &on_cloud(&r, &cloud), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let i = |p: &str| r.index(p).unwrap_or_else(|| panic!("no {p}: {:?}", r.calls.borrow()));
    assert!(i(&rc("status")) < i(&rc("start")) && i(&rc("start")) < i(&rc("address")));
    let started = shed_call_at(&r, "run").unwrap_or_else(|| panic!("no run: {:?}", r.calls.borrow()));
    assert!(i(&rc("address")) < started);
    assert!(cloud.called("run", "sendmeter-impl-12") && sh.all("run").is_empty());
    let inc = std::fs::read_to_string(c.home.join(".ssh/config.d/herdsman")).unwrap();
    assert!(
        inc.contains("Host cloud1\n  HostName 10.0.0.7\n  User ubuntu\n  Port 2222\n"),
        "{inc}"
    );
    let known = std::fs::read_to_string(c.home.join(".ssh/herdsman/cloud1.known")).unwrap();
    assert_eq!(known, "[10.0.0.7]:2222 ssh-ed25519 AAAAC3fakekey\n");
    let events = read(&c.state).unwrap();
    let kinds: Vec<_> = events
        .iter()
        .map(|e| e.kind.as_str())
        .filter(|k| k.starts_with("cloud-"))
        .collect();
    assert_eq!(kinds, ["cloud-start", "cloud-up"]);
    let s = events.iter().rfind(|e| e.kind == "spawn").unwrap();
    assert_eq!(s.str("host"), Some("cloud1"));
    assert_eq!(s.str("why"), Some("needs docker: no local host has it"));
    let sent = cloud.sent("sendmeter-impl-12");
    assert!(sent.iter().any(|c| c.contains("DROVER_HOST='cloud1'")), "{sent:?}");
    let clone: Vec<_> = (r.calls.borrow().iter())
        .filter(|c| c.contains("git clone"))
        .cloned()
        .collect();
    assert!(
        clone.len() == 1 && clone[0].contains("--single-branch") && clone[0].contains("^refs/heads/wip/*"),
        "{clone:?}"
    );
}

#[test]
fn a_stopped_cloud_host_takes_overflow_only_after_the_locals_were_held_long_enough() {
    let c = setup("cloud-overflow");
    let r = fake(0.1);
    assert_eq!(placed(&c, &r, None).as_deref(), Ok("mini"));
    assert!(!r.called(SSH), "{:?}", r.calls.borrow());
    let e = placed(&c, &fake(3.0), None).unwrap_err();
    assert!(e.starts_with("HOST_BUSY: mini"), "{e}");
    let later = drover::Ctx {
        now: NOW + 601,
        ..c.clone()
    };
    assert_eq!(
        placed(&later, &fake(3.0), None).unwrap_err(),
        "CLOUD_STOPPED: cloud1: stopped"
    );
    assert_eq!(placed(&later, &fake(0.1), None).as_deref(), Ok("mini"));
    assert!(!c.state.with_file_name("held-since").exists());
    let e = placed(
        &drover::Ctx {
            now: NOW + 700,
            ..c.clone()
        },
        &fake(3.0),
        None,
    )
    .unwrap_err();
    assert!(e.starts_with("HOST_BUSY: mini"), "{e}");
    let e = placed(
        &drover::Ctx {
            now: NOW + 5 * 3600,
            ..c
        },
        &fake(3.0),
        None,
    )
    .unwrap_err();
    assert!(e.starts_with("HOST_BUSY: mini"), "{e}");
}

#[test]
fn a_running_cloud_host_waits_behind_the_macs_and_says_why_it_was_picked() {
    let c = setup("cloud-running-overflow");
    ev(&c, NOW - 100, "cloud-start");
    ev(&c, NOW - 90, "cloud-up");
    let (sh, cloud) = (FakeShed::new(), FakeShed::new());
    let pick = |c: &drover::Ctx, r: &Fake| {
        let cfg = Config::load(&c.config).unwrap();
        let p = place(
            c,
            &cfg,
            &sh.on(&on_cloud(r, &cloud)),
            &read(&c.state).unwrap(),
            ("sendmeter", 12, "impl"),
            None,
        );
        let p = p.unwrap().unwrap();
        (p.name, p.refusal.map(|r| r.0), p.why)
    };
    assert_eq!(pick(&c, &fake(2.0)), ("mini".into(), None, None));
    assert_eq!(pick(&c, &fake(3.0)).1, Some("HOST_BUSY"));
    let later = drover::Ctx {
        now: NOW + 601,
        ..c.clone()
    };
    let why = Some("overflow: every local host busy 10 min".to_string());
    assert_eq!(pick(&later, &fake(3.0)), ("cloud1".into(), None, why));
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    let why = Some("needs docker: no local host has it".to_string());
    assert_eq!(pick(&c, &fake(0.1)), ("cloud1".into(), None, why));
}

fn running(name: &str) -> (drover::Ctx, Config) {
    let c = setup(name);
    ev(&c, NOW - 3000, "cloud-start");
    ev(&c, NOW - 2990, "cloud-up");
    let d = json!({"role": "impl", "agent": "sendmeter-impl-12", "pane": "w9:p1", "worktree": WT,
                   "branch": "issue-12", "host": "cloud1", "machine": "cloud1"});
    append(&c.state, &Event::new(NOW - 2900, "sendmeter", Some(12), "spawn", d)).unwrap();
    let cfg = Config::load(&c.config).unwrap();
    (c, cfg)
}

fn lane_fake() -> Fake {
    let git = format!("{SSH}'git' '-C' '{WT}'");
    fake(0.1)
        .reply(&format!("{SSH}'test' '-e'"), "")
        .reply(&format!("{git} 'rev-list'"), "1\n")
        .reply(&git, "")
}

fn tick(
    c: &drover::Ctx,
    cfg: &Config,
    r: &dyn Runner,
    live: &BTreeMap<String, Option<Vec<serde_json::Value>>>,
) -> Vec<String> {
    let mut out = vec![];
    drover::cloud::tick(c, cfg, r, live, &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(String::from).collect()
}

fn idle() -> BTreeMap<String, Option<Vec<serde_json::Value>>> {
    BTreeMap::from([("cloud1".to_string(), Some(vec![]))])
}

#[test]
fn an_idle_cloud_host_saves_each_lane_to_wip_then_stops() {
    let (c, cfg) = running("cloud-idle");
    let r = lane_fake();
    let out = tick(&c, &cfg, &r, &idle());
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].contains("stopped (idle 20 min)"), "{out:?}");
    let push = format!("{SSH}'git' '-C' '{WT}' 'push' 'origin' 'HEAD:refs/heads/wip/cloud1-sendmeter-impl-12'");
    assert!(
        r.index(&push).unwrap() < r.index(&rc("stop")).unwrap(),
        "{:?}",
        r.calls.borrow()
    );
    let events = read(&c.state).unwrap();
    let stop = events.iter().rfind(|e| e.kind == "cloud-stop").unwrap();
    assert_eq!(*stop.val("wip"), json!(["wip/cloud1-sendmeter-impl-12"]));
    assert_eq!(drover::cloud::state(&events, "cloud1"), "stopped");
    let (c, cfg) = running("cloud-busy");
    let live = BTreeMap::from([("cloud1".to_string(), Some(vec![json!({"name": "x"})]))]);
    assert!(tick(&c, &cfg, &lane_fake(), &live).is_empty());
    let soon = drover::Ctx { now: NOW - 1000, ..c };
    assert!(tick(&soon, &cfg, &lane_fake(), &idle()).is_empty());
}

#[test]
fn a_failed_wip_push_aborts_the_stop() {
    let (c, cfg) = running("cloud-abort");
    let push = format!("{SSH}'git' '-C' '{WT}' 'push'");
    let r = lane_fake().once_err(&push, 1, "rejected");
    let out = tick(&c, &cfg, &r, &idle());
    assert!(
        out[0].contains("stop aborted, sendmeter-impl-12's work not saved"),
        "{out:?}"
    );
    assert!(!r.called(&rc("stop")), "{:?}", r.calls.borrow());
    let events = read(&c.state).unwrap();
    assert!(events.iter().any(|e| e.kind == "cloud-abort"));
    assert_eq!(drover::cloud::state(&events, "cloud1"), "running");
}

#[test]
fn an_unreachable_host_aborts_the_stop_and_destroy_saves_first() {
    let (c, cfg) = running("cloud-unreachable");
    let r = lane_fake().once_err(&format!("{SSH}'test' '-e'"), 255, "Connection timed out");
    let (code, out) = run(&c, &r, &["hosts", "destroy", "cloud1"]);
    assert_ne!(code, 0, "{out}");
    assert!(!r.called(&rc("destroy")), "{:?}", r.calls.borrow());
    let abort = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "cloud-abort")
        .unwrap();
    assert!(abort.str("why").unwrap().starts_with("can't reach cloud1"), "{abort:?}");
    let tick_r = lane_fake().once_err(&format!("{SSH}'test' '-e'"), 255, "Connection timed out");
    assert!(tick(&c, &cfg, &tick_r, &idle())[0].contains("stop aborted"));
    assert!(!tick_r.called(&rc("stop")), "{:?}", tick_r.calls.borrow());
    let events = read(&c.state).unwrap();
    assert_eq!(events.iter().filter(|e| e.kind == "cloud-abort").count(), 2);
    assert_eq!(drover::cloud::state(&events, "cloud1"), "running");
    let r = lane_fake().reply(&rc("destroy"), "");
    let (code, out) = run(&c, &r, &["hosts", "destroy", "cloud1"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("cloud1: destroyed; wip wip/cloud1-sendmeter-impl-12"),
        "{out}"
    );
    let push = format!("{SSH}'git' '-C' '{WT}' 'push' 'origin' 'HEAD:refs/heads/wip/cloud1-sendmeter-impl-12'");
    assert!(r.index(&push).unwrap() < r.index(&rc("destroy")).unwrap());
}

#[test]
fn the_budget_warns_at_80_percent_and_refuses_when_used_up() {
    let card = |c: &drover::Ctx| {
        let cfg = Config::load(&c.config).unwrap();
        drover::cloud::card(c, &cfg, &read(&c.state).unwrap(), ("cloud1", 0)).unwrap()
    };
    let c = setup("cloud-budget");
    ev(&c, NOW - 7200, "cloud-start");
    ev(&c, NOW, "cloud-stop");
    let [head, tone, text] = card(&c);
    assert_eq!(head, "stopped · 0 lane(s) paused: cloud host stopped");
    assert_eq!(tone, "g");
    assert!(
        text.starts_with("fake small · spent $2 of $10 · 2.0 h today of 20 · address never"),
        "{text}"
    );
    let c = setup("cloud-budget-80");
    ev(&c, NOW - 30_600, "cloud-start");
    ev(&c, NOW - 30_000, "cloud-up");
    let [head, tone, text] = card(&c);
    assert_eq!((head.as_str(), tone.as_str()), ("", "a"), "{text}");
    assert!(text.contains("spent $8 of $10 · 8.5 h today"), "{text}");
    let c = setup("cloud-budget-over");
    ev(&c, NOW - 39_600, "cloud-start");
    ev(&c, NOW - 39_000, "cloud-up");
    assert_eq!(card(&c)[1], "r");
    assert_eq!(
        placed(&c, &fake(0.1), Some("cloud1")).unwrap_err(),
        "CLOUD_BUDGET: cloud1: spent $11.00 of $10 this month"
    );
    run(
        &c,
        &Fake::default(),
        &["queue", "sendmeter", "add", "12", "--needs", "docker"],
    );
    assert_eq!(
        placed(&c, &fake(0.1), None).unwrap_err(),
        "REFUSED NO_HOST: no host with docker (cloud host stopped or over budget)"
    );
    let cfg = Config::load(&c.config).unwrap();
    let out = tick(&c, &cfg, &fake(0.1), &BTreeMap::from([("cloud1".into(), Some(vec![]))]));
    assert!(out[0].contains("stopped (spent $11.00 of $10 this month)"), "{out:?}");
    let c = drover::Ctx {
        now: 40 * 86_400,
        ..setup("cloud-budget-month")
    };
    ev(&c, 2_600_000, "cloud-start");
    ev(&c, 31 * 86_400 + 3600, "cloud-stop");
    assert!(card(&c)[2].contains("spent $1 of $10 · 0.0 h today"), "{}", card(&c)[2]);
}

#[test]
fn the_ssh_include_rewrite_is_idempotent_and_keeps_other_blocks() {
    let c = setup("cloud-include");
    let cfg = Config::load(&c.config).unwrap();
    let dir = c.home.join(".ssh/config.d");
    std::fs::create_dir_all(&dir).unwrap();
    let other = "# herdsman other\nHost other\n  HostName 1.2.3.4\n# end other\n";
    std::fs::write(dir.join("herdsman"), other).unwrap();
    drover::cloud::start(&c, &cfg, &fake(0.1), "cloud1").unwrap();
    let once = std::fs::read_to_string(dir.join("herdsman")).unwrap();
    drover::cloud::start(&c, &cfg, &fake(0.1), "cloud1").unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("herdsman")).unwrap(), once);
    assert!(
        once.starts_with(other) && once.matches("Host cloud1").count() == 1,
        "{once}"
    );
    let mut r = fake(0.1);
    let stdout = "ubuntu@10.0.0.9 22\nssh-ed25519 AAAAC3fakekey\n".into();
    r.replies.insert(
        0,
        (
            rc("address"),
            Out {
                stdout,
                ..Out::default()
            },
        ),
    );
    drover::cloud::start(&c, &cfg, &r, "cloud1").unwrap();
    let now = std::fs::read_to_string(dir.join("herdsman")).unwrap();
    assert!(now.contains("HostName 10.0.0.9") && !now.contains("10.0.0.7"), "{now}");
    let known = std::fs::read_to_string(c.home.join(".ssh/herdsman/cloud1.known")).unwrap();
    assert_eq!(known, "10.0.0.9 ssh-ed25519 AAAAC3fakekey\n");
}

#[test]
fn a_lost_end_marker_keeps_later_blocks_and_a_bad_address_is_refused() {
    let c = setup("cloud-include-bad");
    let cfg = Config::load(&c.config).unwrap();
    let dir = c.home.join(".ssh/config.d");
    std::fs::create_dir_all(&dir).unwrap();
    let other = "# herdsman other\nHost other\n# end other\n";
    std::fs::write(dir.join("herdsman"), format!("# herdsman cloud1\nHost cloud1\n{other}")).unwrap();
    drover::cloud::start(&c, &cfg, &fake(0.1), "cloud1").unwrap();
    let now = std::fs::read_to_string(dir.join("herdsman")).unwrap();
    assert!(
        now.starts_with(other) && now.matches("Host cloud1").count() == 1,
        "{now}"
    );
    let r = fake(0.1).once(
        &rc("address"),
        0,
        "ubuntu@10.0.0.9 22 ProxyCommand sh\nssh-ed25519 BBBB\n",
    );
    let e = drover::cloud::start(&c, &cfg, &r, "cloud1").unwrap_err();
    assert!(format!("{e:#}").contains("not `user@host port`"), "{e:#}");
    assert_eq!(std::fs::read_to_string(dir.join("herdsman")).unwrap(), now);
}

struct Stdin(Fake, Shared<Vec<String>>);

impl Runner for Stdin {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        self.1.borrow_mut().push(input.into());
        self.0.run(prog, args)
    }
}

#[test]
fn the_provider_key_never_reaches_argv_or_the_log() {
    let (c, cfg) = running("cloud-key");
    let r = Stdin(
        lane_fake().once_err(&rc("stop"), 1, &format!("bad key {KEY}")),
        Shared::default(),
    );
    let e = drover::cloud::stop(&c, &cfg, &r, "cloud1", "stop", "test").unwrap_err();
    assert!(format!("{e:#}").contains("bad key •••"), "{e:#}");
    drover::cloud::stop(&c, &cfg, &r, "cloud1", "stop", "test").unwrap();
    drover::cloud::start(&c, &cfg, &r, "cloud1").unwrap();
    assert!(
        r.1.borrow().iter().any(|i| i == &format!("{KEY}\n")),
        "the key goes on stdin"
    );
    assert!(
        !r.0.calls.borrow().iter().any(|c| c.contains(KEY)),
        "{:?}",
        r.0.calls.borrow()
    );
    let log = drover::events::jsonl(&c.state).unwrap();
    assert!(!log.contains(KEY) && log.contains("cloud-up"), "{log}");
}

#[test]
fn a_recipe_without_key_env_runs_its_command_with_no_key() {
    let (c, cfg) = running("cloud-nokey");
    let recipe = c.config.with_file_name("recipes/fake.toml");
    std::fs::write(&recipe, RECIPE.replace("key_env = \"FAKE_CLOUD_KEY\"\n", "")).unwrap();
    std::fs::remove_file(c.config.with_file_name("secrets/FAKE_CLOUD_KEY")).unwrap();
    let r = Stdin(lane_fake().reply("sh -c fakecloud stop cloud1", ""), Shared::default());
    drover::cloud::stop(&c, &cfg, &r, "cloud1", "stop", "test").unwrap();
    assert!(r.0.called("sh -c fakecloud stop cloud1"), "{:?}", r.0.calls.borrow());
    assert!(
        r.1.borrow().iter().all(String::is_empty),
        "no key on stdin: {:?}",
        r.1.borrow()
    );
    assert!(read(&c.state).unwrap().iter().any(|e| e.kind == "cloud-stop"));
}

#[test]
fn a_cloud_key_file_others_can_read_is_refused() {
    let (c, cfg) = running("cloud-0644");
    secret_file(&c, "FAKE_CLOUD_KEY", KEY, 0o644);
    let r = Stdin(lane_fake(), Shared::default());
    let e = drover::cloud::stop(&c, &cfg, &r, "cloud1", "stop", "test").unwrap_err();
    assert!(format!("{e:#}").contains("REFUSED SECRET_FILE_OPEN"), "{e:#}");
    assert!(!r.0.called("sh -c"), "nothing ran: {:?}", r.0.calls.borrow());
}

fn step(cmd: &str) -> String {
    let s = format!("{}{cmd}", drover::cloud::SH).replace('\'', "'\\''");
    format!("{SSH}'sh' '-c' '{s}")
}

fn boot_fake(gh_in: bool) -> Fake {
    let mine = format!(
        "[ \"$('/opt/drover/bin/drover' --version)\" = 'drover {}' ]",
        drover::VERSION
    );
    let f = Fake::default()
        .reply(&format!("{SSH}'sh' '-c' 'uname -s"), "Linux\nx86_64\n/home/me\n")
        .reply(&step("[ \"$(shed --version)\""), "")
        .reply(&step("shed doctor"), "")
        .once(&step("command -v gh"), 1, "")
        .once(&step(&mine), 1, "")
        .reply(&step("command -v"), "")
        .reply(&step("apt_ gh"), "")
        .reply(
            &step("cargo install --git https://github.com/jirathip-dev/herdsman --rev"),
            "",
        )
        .reply(&step(&mine), "")
        .reply(&step("claude auth status"), "")
        .reply(&step("git config --global --get credential"), "")
        .reply(&format!("{SSH}'sh' '-c' 'mkdir"), "")
        .reply(&step("git clone"), "")
        .reply(&step("test ! -e "), "");
    if gh_in {
        f.reply(&step("gh auth status"), "")
    } else {
        f
    }
}

#[test]
fn bootstrap_installs_what_is_missing_step_by_step_and_opens_the_logins_in_a_terminal() {
    let (c, cfg) = running("cloud-boot");
    let (r, here, cloud) = (boot_fake(false), FakeShed::new(), FakeShed::new());
    let mut out = vec![];
    let there = on_cloud(&r, &cloud);
    let e = drover::cloud::bootstrap(&c, &cfg, &here.on(&there), "cloud1", &mut out).unwrap_err();
    assert!(
        format!("{e:#}").contains("log in in terminal drover-setup-gh-token"),
        "{e:#}"
    );
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("✓ git — there") && out.contains("✓ gh — installed"),
        "{out}"
    );
    assert!(out.contains("✓ drover — installed"), "{out}");
    let order = [
        "command -v git",
        "apt_ gh",
        "command -v claude",
        "command -v cargo",
        "[ \"$(shed --version)\"",
        "shed doctor",
    ];
    let at: Vec<_> = order.iter().map(|s| r.index(&step(s)).unwrap()).collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
    assert!(!r.called(&step("apt_ git")));
    let sha = env!("DROVER_SHA");
    let build = format!(
        "cargo install --git https://github.com/jirathip-dev/herdsman --rev {sha} --locked --root '/opt/drover'"
    );
    assert!(r.called(&step(&build)), "{:?}", r.calls.borrow());
    assert!(here.methods().is_empty(), "nothing on this host: {:?}", here.methods());
    let runs: Vec<String> = cloud
        .all("send")
        .iter()
        .map(|p| p["text"].as_str().unwrap().into())
        .collect();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(
        runs[0].contains("read -rs T; printf %s \"$T\" | gh auth login --with-token"),
        "{runs:?}"
    );
    assert!(!r.called(&step("gh auth setup-git")) && !r.called(&step("git clone")));
}

#[test]
fn bootstrap_again_after_the_logins_sets_up_https_git_clones_and_syncs_without_secrets() {
    let (c, _) = running("cloud-boot2");
    let text = std::fs::read_to_string(&c.config).unwrap();
    let extra = "\n[[fleet.sendmeter.notify]]\ncmd = \"curl https://api.telegram.org/botSEKRIT/send\"\n";
    std::fs::write(&c.config, text + extra).unwrap();
    let cfg = Config::load(&c.config).unwrap();
    let r = Stdin(
        boot_fake(true)
            .once(&step("git config --global --get credential"), 1, "")
            .once(&step("test -d"), 1, "")
            .reply(&step("test -d"), ""),
        Shared::default(),
    );
    let r = Stdin(r.0.reply(&step("gh auth setup-git"), ""), r.1);
    let mut out = vec![];
    drover::cloud::bootstrap(&c, &cfg, &r, "cloud1", &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("✓ git over https — installed") && out.contains("bootstrapped"),
        "{out}"
    );
    let clone = "git clone --single-branch --branch 'staging' https://github.com/jirathip-dev/sendmeter.git \
                 '/tmp/does-not-matter/sendmeter' && git -C '/tmp/does-not-matter/sendmeter' config remote.origin.fetch \
                 '+refs/heads/*:refs/remotes/origin/*' && git -C '/tmp/does-not-matter/sendmeter' config --add \
                 remote.origin.fetch '^refs/heads/wip/*'";
    assert!(r.0.called(&step(clone)), "{:?}", r.0.calls.borrow());
    assert!(!r.0.called(&step("git clone https://")), "never a full clone");
    assert!(
        !r.0.calls.borrow().iter().any(|c| c.contains("--with-token")),
        "no login pane"
    );
    let written = r.1.borrow().join("\n");
    assert!(!written.contains("[fleet") && !written.contains("SEKRIT"), "{written}");
    let conf = c.config.display().to_string();
    assert!(r.0.called(&step(&format!(
        "test ! -e '{conf}' || '/opt/drover/bin/drover' profiles sync"
    ))));
    assert!(out.contains("✓ profiles sync — there"), "{out}");
    let log = drover::events::jsonl(&c.state).unwrap();
    assert!(
        log.contains("host-bootstrap") && !log.contains("SEKRIT") && !log.contains(KEY),
        "{log}"
    );
    assert!(!r
        .0
        .calls
        .borrow()
        .iter()
        .any(|c| c.contains(KEY) || c.contains("SEKRIT")));
}

struct Remote<'a>(&'a Fake, String, std::path::PathBuf);

impl Runner for Remote<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let Some(line) = args.last().filter(|l| prog == "ssh" && l.contains(&self.1)) else {
            return self.0.run(prog, args);
        };
        let there = self.2.join("fleet.toml").display().to_string();
        let line = line
            .replace(&self.1, &there)
            .replace("/opt/drover/bin/drover", env!("CARGO_BIN_EXE_drover"));
        let o = std::process::Command::new("sh")
            .args(["-c", &line])
            .env("HOME", &self.2)
            .env("DROVER_HOME", &self.2)
            .env("FLEET_CONFIG", &there)
            .env("FLEET_STATE", self.2.join("state/events.jsonl"))
            .env("FLEET_HERMES_PROFILES", self.2.join("hermes"))
            .output()?;
        Ok(Out {
            code: o.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&o.stdout).into(),
            stderr: String::from_utf8_lossy(&o.stderr).into(),
        })
    }
}

const OLD_CONF: &str = r#"[fleet.sendmeter]
integration = "staging"
local = "/tmp/does-not-matter/sendmeter"
repo = "jirathip-dev/sendmeter"

[fleet.sendmeter.role.impl]
account = "C"
harness = "claude"
profile = "fleet-impl"

[hosts.cloud1]
allow_commands = ["docker"]
"#;

fn boot_again() -> Fake {
    boot_fake(true)
        .reply(&step("git config --global --get credential"), "")
        .reply(&step("test -d"), "")
}

#[test]
fn bootstrap_repairs_an_old_bootstrap_fleet_toml_so_profiles_sync_passes_there() {
    let (c, cfg) = running("cloud-boot-old-conf");
    let there = c.config.parent().unwrap().parent().unwrap().join("cloud1");
    std::fs::create_dir_all(&there).unwrap();
    std::fs::write(there.join("fleet.toml"), OLD_CONF).unwrap();
    let f = boot_again();
    let conf = c.config.display().to_string();
    let r = Remote(&f, conf.clone(), there.clone());
    let sync = format!("test ! -e '{conf}' || '{}' profiles sync", c.exe);
    let o = r.run("ssh", &["cloud1".into(), sync.clone()]).unwrap();
    assert!(o.stderr.contains("account 'C' is not in [accounts]"), "{o:?}");
    let mut out = vec![];
    let res = drover::cloud::bootstrap(&c, &cfg, &r, "cloud1", &mut out);
    let out = String::from_utf8(out).unwrap();
    assert!(res.is_ok(), "{res:?} {out}");
    assert!(out.contains("✓ profiles sync"), "{out}");
    let left = there.join("fleet.toml");
    assert!(
        !left.exists() || Config::load(&left).is_ok(),
        "{:?}",
        std::fs::read_to_string(&left)
    );
    let o = r.run("ssh", &["cloud1".into(), sync]).unwrap();
    assert_eq!(o.code, 0, "profiles sync passes there: {o:?}");
}

#[test]
fn a_failed_bootstrap_step_shows_its_command_exit_code_and_output_and_logs_them() {
    let (c, cfg) = running("cloud-boot-fail");
    let err = "invalid config /home/me/.config/drover/fleet.toml: fleet.berth.role.orchestrator: account 'C' is not in [accounts]";
    let f = boot_again()
        .once_err(&step("test ! -e "), 1, &format!("warming up\n{err}\n"))
        .once_err(&step("test ! -e "), 1, &format!("warming up\n{err}\n"));
    let mut out = vec![];
    let res = drover::cloud::bootstrap(&c, &cfg, &f, "cloud1", &mut out);
    let out = String::from_utf8(out).unwrap();
    let shown = format!("{out}\n{:#}", res.unwrap_err());
    let cmd = format!(
        "test ! -e '{}' || '/opt/drover/bin/drover' profiles sync",
        c.config.display()
    );
    assert!(shown.contains(&cmd) && shown.contains("exit 1"), "{shown}");
    assert!(shown.contains(err) && shown.contains("warming up"), "{shown}");
    let log = read(&c.state).unwrap();
    let e = log
        .iter()
        .rev()
        .find(|e| e.kind == "host-bootstrap")
        .expect("the bootstrap event");
    let d = serde_json::Value::Object(e.data.clone());
    assert_eq!(
        (d["step"].as_str(), d["code"].as_i64()),
        (Some("profiles sync"), Some(1)),
        "{d}"
    );
    assert!(d["cmd"] == cmd && d["tail"].as_str().unwrap().contains(err), "{d}");
}

#[test]
fn the_token_expiry_parses_and_warns_from_14_days() {
    let day = 86_400;
    let now = 20_000 * day;
    let head = |d: &str| {
        format!("HTTP/2.0 200 OK\nGitHub-Authentication-Token-Expiration: {d}\n\n[{{\"full_name\":\"a/b\"}},{{\"full_name\":\"a/c\"}}]")
    };
    let (card, warn) = drover::cloud::token(&head("2024-12-23 10:00:00 UTC"), now);
    assert_eq!(card, "github: token · 2 repos · expires in 80d");
    assert_eq!(warn, None);
    let (card, warn) = drover::cloud::token(&head("2024-10-18 00:00:00 +0000"), now);
    assert!(card.ends_with("expires in 14d"), "{card}");
    assert!(warn.unwrap().starts_with("! github — token expires in 14d"));
    let (_, warn) = drover::cloud::token(&head("2024-03-01 00:00:00 UTC"), now);
    assert!(warn.unwrap().starts_with("! github — token expires in -217d"));
    assert_eq!(
        drover::cloud::token("HTTP/2.0 200 OK\n\n[]", now).0,
        "github: token · 0 repos · no expiry"
    );
    let (card, warn) = drover::cloud::token("", now);
    assert_eq!(card, "github: unreadable");
    assert!(warn.unwrap().starts_with("✗ github — gh there has no working token"));
}

#[test]
fn a_snapshot_needs_a_green_health_check_first() {
    let (c, cfg) = running("cloud-snap");
    let recipe = RECIPE.replace("[rate", "snapshot = \"fakecloud snapshot {name}\"\n[rate");
    std::fs::write(c.config.with_file_name("recipes/fake.toml"), recipe).unwrap();
    let r = Fake::default().reply(&rc("snapshot"), "");
    let e = drover::cloud::snapshot(&c, &cfg, &r, "cloud1").unwrap_err();
    assert!(format!("{e:#}").contains("cloud1 is not healthy"), "{e:#}");
    assert!(!r.called(&rc("snapshot")));
    let r = healthy(Fake::default()).reply(&rc("snapshot"), "");
    let (sh, cloud) = (FakeShed::new(), FakeShed::new());
    drover::cloud::snapshot(&c, &cfg, &sh.on(&on_cloud(&r, &cloud)), "cloud1").unwrap();
    assert!(cloud.methods().contains(&"ls".to_string()), "its lanes were looked at");
    assert!(r.called(&rc("snapshot")) && drover::events::jsonl(&c.state).unwrap().contains("cloud-snapshot"));
    assert!(
        drover::hosts::cached(&c)["cloud1"].github.starts_with("github:"),
        "the look reads the token too"
    );
}
