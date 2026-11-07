//! #537: `herdsman runners` over a fake recipe and a fake GitHub: the health line, revive (a token and a restart), add
//! within the cap and refused past it (an owner ask the only way on), remove draining first, retire-local, `check`'s
//! `runners-low` / `runners-ok` once each, and the registration token never in argv, the log or the output.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use serde_json::json;

const NOW: u64 = 1_000_000;
const KEY: &str = "sekrit-provider-key";
const TOK: &str = "REGTOK-sekrit-123";
const LIST: &str = "gh api repos/o/app/actions/runners?per_page=100";
const RECIPE: &str = r#"key_env = "FAKE_KEY"
runner_create = "fakeboat create {size} {ttl} {snapshots}"
runner_state = "fakeboat state {vm}"
runner_delete = "fakeboat delete {vm}"
runner_exec = "fakeboat exec {vm}"
runner_revive = "revive {rt} {service}"
runner_token = "token {rt}"
runner_drain = "drain"
runner_names = "names"
[rate_usd_hour]
large = 0.5
"#;

/// The `sh -c` line a recipe command runs as; `tok`: the registration token read from stdin too.
fn rc(tok: bool, line: &str) -> String {
    let rt = if tok { "IFS= read -r RT; export RT; " } else { "" };
    format!("sh -c IFS= read -r K; export FAKE_KEY=\"$K\"; unset K; {rt}{line}")
}

/// A VM-side step `c` run on `vm` through `runner_exec`.
fn on(vm: &str, tok: bool, c: &str) -> String {
    rc(tok, &format!("export C='{c}'; fakeboat exec {vm}"))
}

/// vm1 and vm2 (3 slots each, $12 a day each, cap 3 VMs / $40), this host's runners labelled `self`.
fn setup(name: &str) -> herdsman::Ctx {
    let cfg = format!(
        "{CONFIG}\n[ci_runners]\nprovider = \"fake\"\nrepo = \"o/app\"\nlabel = \"boat\"\nsize = \"large\"\nslots = 3\n\
         max_vms = 3\nusd_day = 40\nvms = [\"vm1\", \"vm2\"]\nlocal_label = \"self\"\nlocal_stop = \"stop-local\"\n"
    );
    let c = ctx(&tmpdir(name), &cfg);
    std::fs::create_dir_all(c.config.with_file_name("recipes")).unwrap();
    std::fs::write(c.config.with_file_name("recipes/fake.toml"), RECIPE).unwrap();
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    c
}

/// GitHub's runner list: (id, name, label, online, busy) each.
fn gh(rs: &[(u64, &str, &str, bool, bool)]) -> String {
    let r = rs.iter().map(|(id, n, l, on, busy)| {
        json!({"id": id, "name": n, "status": if *on { "online" } else { "offline" }, "busy": busy,
               "labels": [{"name": "self-hosted"}, {"name": l}]})
    });
    json!({"total_count": rs.len(), "runners": r.collect::<Vec<_>>()}).to_string()
}

/// vm1's three runners (`boat-aa-`), vm2's (`boat-bb-`, `bb_on` of them online) and one busy and one idle `self` runner.
fn fleet(bb_on: usize, aa_busy: bool) -> String {
    let mut v = vec![
        (11, "boat-aa-1", "boat", true, aa_busy),
        (12, "boat-aa-2", "boat", true, false),
        (13, "boat-aa-3", "boat", true, false),
        (31, "self-1", "self", true, true),
        (32, "self-2", "self", true, false),
    ];
    for (i, n) in ["boat-bb-1", "boat-bb-2", "boat-bb-3"].iter().enumerate() {
        v.push((21 + i as u64, n, "boat", i < bb_on, false));
    }
    gh(&v)
}

fn fake(list: &str) -> Fake {
    Fake::default()
        .stored("herdsman/all/FAKE_KEY", KEY)
        .reply(LIST, list)
        .reply(
            "gh api -X POST repos/o/app/actions/runners/registration-token",
            &format!("{TOK}\n"),
        )
        .reply("gh api -X DELETE", "")
        .reply(&rc(false, "fakeboat state vm1"), "running\n")
        .reply(&rc(false, "fakeboat state vm2"), "stopped\n")
        .reply(&rc(false, "fakeboat create large 0 false"), "booting\nvm3\n")
        .reply(&on("vm1", false, "names"), "boat-aa-\n")
        .reply(&rc(false, "fakeboat delete"), "")
        .reply(&rc(false, "export C="), "ok\n")
        .reply(&rc(true, "export C="), "ok\n")
        .reply("herdr agent list", NO_AGENTS)
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state).unwrap().into_iter().filter(|e| e.kind == kind).collect()
}

/// Nothing herdsman wrote or ran shows the token: not the log, the output or any argv; only the VM step's stdin.
fn no_token(c: &herdsman::Ctx, r: &Fake, out: &str) {
    let log = std::fs::read_to_string(c.state.with_file_name("events.db")).unwrap_or_default()
        + &serde_json::to_string(&read(&c.state).unwrap()).unwrap();
    assert!(!log.contains(TOK) && !log.contains(KEY), "{log}");
    assert!(!out.contains(TOK), "{out}");
    assert!(r.calls.borrow().iter().all(|l| !l.contains(TOK) && !l.contains(KEY)));
}

/// An owner ask `id` about `question`, answered `choice` by `by`.
fn asked(c: &herdsman::Ctx, id: &str, question: &str, choice: &str, by: &str) {
    let q = json!({"id": id, "question": question, "options": ["yes", "no"], "kind": "owner"});
    append(&c.state, &Event::new(NOW - 60, "synergy", None, "needs-decision", q)).unwrap();
    let d = json!({"id": id, "choice": choice, "by": by});
    append(&c.state, &Event::new(NOW - 30, "synergy", None, "decision", d)).unwrap();
}

#[test]
fn status_prints_the_health_line_each_vm_and_each_runner() {
    let c = setup("runners-status");
    let r = fake(&fleet(2, false));
    let (code, out) = run(&c, &r, &["runners", "status"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with("runners: 5/6 online (2 VMs × 3 slots, label boat)\n"),
        "{out}"
    );
    assert!(out.contains("vm vm1: running\nvm vm2: stopped\n"), "{out}");
    assert!(
        out.contains("runner boat-bb-3: offline\n") && out.contains("runner self-1: online busy\n"),
        "{out}"
    );
    assert_eq!(kinds(&c, "runners-status")[0].str("health"), Some("5/6 online"));
}

#[test]
fn revive_pushes_a_fresh_token_restarts_and_logs_it_never_showing_the_token() {
    let c = setup("runners-revive");
    let r = fake(&fleet(0, false));
    let (code, out) = run(&c, &r, &["runners", "revive", "vm2"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        r.called(&on("vm2", true, "revive {rt} ci-runner")),
        "{:?}",
        r.calls.borrow()
    );
    let stdin = r
        .inputs
        .borrow()
        .iter()
        .find(|i| i.0.contains("revive"))
        .unwrap()
        .1
        .clone();
    assert_eq!(stdin, format!("{KEY}\n{TOK}\n"), "the token goes on stdin");
    assert_eq!(kinds(&c, "runners-revive")[0].str("vm"), Some("vm2"));
    no_token(&c, &r, &out);
    assert_ne!(run(&c, &r, &["runners", "revive", "vm9"]).0, 0, "no such VM");
    assert!(!r.called(&on("vm9", true, "revive {rt} ci-runner")));
}

#[test]
fn add_creates_within_the_cap_refuses_past_it_and_only_an_owner_yes_lets_it_on() {
    let c = setup("runners-add");
    let r = fake(&fleet(3, false));
    let (code, out) = run(&c, &r, &["runners", "add"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("vm3: created (large), 3 runners starting"), "{out}");
    let add = &kinds(&c, "runners-add")[0];
    assert_eq!((add.str("vm"), add.data["usd_day"].as_f64()), (Some("vm3"), Some(12.0)));
    assert!(r.called(&on("vm3", true, "revive {rt} ci-runner")));
    no_token(&c, &r, &out);
    // A 4th VM is past max_vms 3: refused, nothing created.
    let made = r.count(&rc(false, "fakeboat create"));
    let (code, out) = run(&c, &r, &["runners", "add"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("REFUSED RUNNERS_CAP: 4 VMs at $48.00/day") && out.contains("--kind owner"),
        "{out}"
    );
    // A conductor's or a defaulted answer is no approval; nor an ask about another verb.
    asked(&c, "q1", "herdsman runners add 1: ok?", "yes", "conductor-t1");
    asked(&c, "q2", "herdsman runners add 1: ok?", "yes", "default");
    asked(&c, "q3", "herdsman runners remove vm1: ok?", "yes", "owner");
    asked(&c, "q4", "herdsman runners add 1: ok?", "no", "owner");
    for id in ["q1", "q2", "q3", "q4"] {
        assert_eq!(run(&c, &r, &["runners", "add", "--approved", id]).0, 3, "{id}");
    }
    assert_eq!(r.count(&rc(false, "fakeboat create")), made);
    asked(&c, "q5", "herdsman runners add 1: ok?", "yes", "owner");
    let (code, out) = run(&c, &r, &["runners", "add", "--approved", "q5"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(r.count(&rc(false, "fakeboat create")), made + 1);
    assert_eq!(kinds(&c, "runners-add")[1].str("approved"), Some("q5"));
    let (code, out) = run(&c, &r, &["runners", "add", "--approved", "q5"]);
    assert_eq!(code, 3, "one yes lets one run on: {out}");
}

#[test]
fn remove_drains_then_deregisters_then_deletes_and_the_last_vm_needs_the_owner() {
    let c = setup("runners-remove");
    // vm1 runs a job through the first poll, then is idle.
    let r = fake(&fleet(3, false))
        .once(LIST, 0, &fleet(3, true))
        .once(LIST, 0, &fleet(3, true));
    let (code, out) = run(&c, &r, &["runners", "remove", "vm1"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert!(out.contains("vm1: drained, 3 runner(s) deregistered, deleted"), "{out}");
    let at = |p: &str| {
        r.index(p)
            .unwrap_or_else(|| panic!("{p} not run: {:?}", r.calls.borrow()))
    };
    let (drain, wait) = (at(&on("vm1", false, "drain")), at("sleep 30"));
    let (dereg, del) = (
        at("gh api -X DELETE repos/o/app/actions/runners/11"),
        at(&rc(false, "fakeboat delete vm1")),
    );
    assert!(
        drain < wait && wait < dereg && dereg < del,
        "{drain} {wait} {dereg} {del}"
    );
    assert!(
        !r.called("gh api -X DELETE repos/o/app/actions/runners/21"),
        "vm2's runners stay"
    );
    assert_eq!(kinds(&c, "runners-remove")[0].str("vm"), Some("vm1"));
    let (_, out) = run(&c, &r, &["runners", "status"]);
    assert!(!out.contains("vm vm1") && out.contains("(1 VMs × 3 slots"), "{out}");
    // vm2 down to no online runner elsewhere: vm1 is the last VM with online runners.
    let c = setup("runners-remove-last");
    let r = fake(&fleet(0, false));
    let (code, out) = run(&c, &r, &["runners", "remove", "vm1"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED RUNNERS_LAST"), "{out}");
    assert!(!r.called(&on("vm1", false, "drain")) && !r.called("gh api -X DELETE"));
    // One idle runner by name is deregistered; a busy one is not.
    assert_eq!(run(&c, &r, &["runners", "remove", "self-2"]).0, 0);
    assert!(r.called("gh api -X DELETE repos/o/app/actions/runners/32"));
    assert_ne!(run(&c, &r, &["runners", "remove", "self-1"]).0, 0);
    assert!(!r.called("gh api -X DELETE repos/o/app/actions/runners/31"));
}

#[test]
fn retire_local_stops_and_deregisters_the_idle_local_runners_only_with_the_vms_up() {
    let c = setup("runners-retire");
    let r = fake(&fleet(2, false));
    let (code, out) = run(&c, &r, &["runners", "retire-local"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED RUNNERS_LOW: VM runners are 5/6 online"), "{out}");
    assert!(!r.called("sh -c stop-local"));
    let r = fake(&fleet(3, false)).reply("sh -c stop-local", "");
    let (code, out) = run(&c, &r, &["runners", "retire-local"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("1 deregistered, 1 still busy"), "{out}");
    let (stop, dereg) = (
        r.index("sh -c stop-local").unwrap(),
        r.index("gh api -X DELETE repos/o/app/actions/runners/32"),
    );
    assert!(dereg.is_some_and(|d| stop < d));
    assert!(
        !r.called("gh api -X DELETE repos/o/app/actions/runners/31"),
        "busy: left to finish"
    );
    assert_eq!(kinds(&c, "runners-retire-local")[0].data["deregistered"], 1);
}

#[test]
fn tokens_reach_every_vm_and_check_refreshes_them_and_alerts_low_and_ok_once_each() {
    let c = setup("runners-check");
    let r = fake(&fleet(0, false));
    let (code, out) = run(&c, &r, &["runners", "tokens"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("a fresh one on 2/2 VMs") && out.contains("before this: never"),
        "{out}"
    );
    assert!(r.called(&on("vm1", true, "token {rt}")) && r.called(&on("vm2", true, "token {rt}")));
    no_token(&c, &r, &out);
    // The 0/9-style outage: 3/6 online, two passes, one alert; the token was pushed just now, so not again.
    let mut outs = String::new();
    for _ in 0..2 {
        let (code, out) = run(&c, &r, &["check"]);
        assert_eq!(code, 0, "{out}");
        outs += &out;
    }
    assert_eq!(kinds(&c, "runners-low").len(), 1, "{outs}");
    assert_eq!(kinds(&c, "runners-low")[0].str("reason"), Some("CI runners 3/6 online"));
    assert_eq!(kinds(&c, "runners-tokens").len(), 1, "within 30 min of the last push");
    let r = fake(&fleet(3, false));
    for _ in 0..2 {
        assert_eq!(run(&c, &r, &["check"]).0, 0);
    }
    assert_eq!((kinds(&c, "runners-low").len(), kinds(&c, "runners-ok").len()), (1, 1));
    no_token(&c, &r, "");
}

#[test]
fn check_pushes_a_token_once_the_last_is_30_min_old() {
    let c = setup("runners-refresh");
    let d = json!({"vms": 2, "failed": []});
    append(&c.state, &Event::new(NOW - 1800, "herdsman", None, "runners-tokens", d)).unwrap();
    let r = fake(&fleet(3, false));
    assert_eq!(run(&c, &r, &["check"]).0, 0);
    assert_eq!(kinds(&c, "runners-tokens").len(), 2);
    assert_eq!(r.count(&on("vm1", true, "token {rt}")), 1);
}

#[test]
fn the_built_in_boat_recipe_has_every_runner_command() {
    let c = setup("runners-boat");
    let t = herdsman::cloud::load(&c, "boat").unwrap();
    for k in [
        "create",
        "runner_create",
        "runner_state",
        "runner_delete",
        "runner_exec",
        "runner_revive",
        "runner_token",
    ] {
        assert!(t.get(k).and_then(|v| v.as_str()).is_some(), "{k}");
    }
    assert!(t["runner_drain"].is_str() && t["runner_names"].is_str());
    assert_eq!(t["rate_usd_hour"]["large"].as_float(), Some(0.072));
}
