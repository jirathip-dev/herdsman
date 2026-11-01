//! #46: idle with a reason: `waiting_on` per case, the orch nudge / `orch-stuck`, and the owner's 9 idle lanes replayed.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, read, Event};
use herdsman::serve::Server;
use herdsman::waiting::{orch, scan, Waiting};
use serde_json::json;

const NOW: u64 = 1_000_000;
const ORCH: &str = "sendmeter-orch";

/// A 40-hex sha starting with `p`.
fn sha(p: &str) -> String {
    format!("{p}{}", "0".repeat(40 - p.len()))
}

fn ev(c: &herdsman::Ctx, ts: u64, issue: Option<u64>, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", issue, kind, d)).unwrap();
}

fn spawn(c: &herdsman::Ctx, ts: u64, n: u64, role: &str, agent: &str) {
    ev(c, ts, Some(n), "spawn", json!({"role": role, "agent": agent, "pane": format!("w1:{agent}"), "worktree": format!("/wt/{agent}")}));
}

/// A rev lane in a real worktree; `verdict` = verdict.md's text (None: not written), `old` = mtime before the spawn.
fn rev(c: &herdsman::Ctx, ts: u64, n: u64, verdict: Option<&str>, old: bool) -> String {
    let (agent, wt) = (format!("sendmeter-rev-{n}-r1"), c.worktrees.join(format!("rev-{n}")));
    std::fs::create_dir_all(&wt).unwrap();
    ev(c, ts, Some(n), "spawn", json!({"role": "rev", "agent": agent, "pane": format!("w1:{agent}"), "worktree": wt}));
    if let Some(v) = verdict {
        let f = std::fs::File::create(wt.join("verdict.md")).unwrap();
        std::io::Write::write_all(&mut &f, v.as_bytes()).unwrap();
        if old {
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts - 5)).unwrap();
        }
    }
    wt.join("verdict.md").display().to_string()
}

fn verdict(c: &herdsman::Ctx, ts: u64, n: u64, v: &str, sha: &str) {
    ev(c, ts, Some(n), "verdict", json!({"verdict": v, "sha": sha}));
}

fn pr(head: &str, ci: &str) -> String {
    let check = if ci == "green" { json!({"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}) } else { json!({"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS"}) };
    json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head, "statusCheckRollup": [check]}).to_string()
}

/// Every case of the brief in one log, with fake gh (PR views) and git (worktree heads).
fn fixture(name: &str) -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir(name), CONFIG);
    let t = NOW - 1500;
    spawn(&c, t, 1, "impl", "sendmeter-impl-1");
    ev(&c, NOW - 600, Some(1), "needs-decision", json!({"id": "q3", "agent": "sendmeter-impl-1", "question": "split it?", "options": [], "recommend": null}));
    spawn(&c, t, 42, "impl", "sendmeter-impl-42");
    spawn(&c, t + 10, 42, "rev", "sendmeter-rev-42-r1");
    verdict(&c, t + 20, 42, "FAIL", &sha("1"));
    spawn(&c, t + 30, 42, "impl", "sendmeter-impl-42");
    spawn(&c, t, 31, "impl", "sendmeter-impl-31");
    for (n, v) in [(3, "FAIL"), (4, "PASS"), (5, "PASS"), (6, ""), (9, ""), (12, ""), (14, "")] {
        spawn(&c, t, n, "impl", &format!("sendmeter-impl-{n}"));
        let file = [(6, Some("PASS\nall good\n"), false), (12, Some("PASS\n"), true), (14, Some("LGTM\n"), false)].into_iter().find(|x| x.0 == n);
        rev(&c, t + 10, n, file.and_then(|x| x.1), file.is_some_and(|x| x.2));
        if !v.is_empty() {
            verdict(&c, t + 20, n, v, &sha(&n.to_string()));
        }
    }
    spawn(&c, t, 13, "impl", "sendmeter-impl-13");
    spawn(&c, t, 7, "impl", "sendmeter-impl-7");
    spawn(&c, t, 8, "impl", "sendmeter-impl-8");
    ev(&c, t + 20, Some(8), "merged", json!({"pr": "9"}));
    spawn(&c, t, 10, "impl", "sendmeter-impl-10");
    spawn(&c, t, 11, "impl", "sendmeter-impl-11");
    let r = Fake::default()
        .reply("gh pr view issue-42 ", &pr(&sha("605297c"), "pending"))
        .reply("git -C /wt/sendmeter-impl-42 ", &format!("{} {}", sha("605297c"), NOW - 480))
        .reply("gh pr view issue-31 ", &pr(&sha("70fe5a1"), "pending"))
        .reply("git -C /wt/sendmeter-impl-31 ", &format!("{} {}", sha("70fe5a1"), NOW - 180))
        .reply("gh pr view issue-3 ", &pr(&sha("3"), "green"))
        .reply("gh pr view issue-4 ", &pr(&sha("4"), "green"))
        .reply("gh pr view issue-5 ", &pr(&sha("5"), "pending"))
        .reply("gh pr view issue-6 ", &pr(&sha("6"), "pending"))
        .reply(&format!("git -C {} ", c.worktrees.join("rev-6").display()), &format!("{} {}", sha("6"), NOW - 900))
        .reply("gh pr view issue-14 ", &pr(&sha("e"), "pending"))
        .reply("gh pr view issue-13 ", &pr(&sha("d"), "pending").replace("OPEN", "CLOSED"))
        .reply("gh pr view issue-10 ", &pr(&sha("b"), "pending"))
        .reply("git -C /wt/sendmeter-impl-10 ", &format!("{} {}", sha("c"), NOW - 60));
    (c, r)
}

/// The issues' reasons: each one's latest lane only.
fn waits(c: &herdsman::Ctx, r: &Fake, agents: &[serde_json::Value]) -> Vec<Waiting> {
    lanes(c, r, agents).into_iter().filter(|(s, w)| s.lane.as_ref() == Some(&w.lane)).map(|x| x.1).collect()
}

fn lanes(c: &herdsman::Ctx, r: &Fake, agents: &[serde_json::Value]) -> Vec<(herdsman::events::IssueState, Waiting)> {
    scan(c, &Config::load(&c.config).unwrap(), r, &read(&c.state).unwrap(), agents, true)
}

#[test]
fn every_case_has_its_who_what_and_next_command() {
    let (c, r) = fixture("waiting-table");
    let all = waits(&c, &r, &[json!({"name": "sendmeter-impl-11", "agent_status": "working"})]);
    let t = NOW - 1500;
    let s = |n: &str| Some(format!("herdsman {n}"));
    let want: [(u64, &str, &str, u64, Option<String>); 14] = [
        (1, "owner", "waiting on your answer (q3)", NOW - 600, None),
        (3, "orch", "review FAIL, waiting for orch to start fix round", t + 20, s("spawn sendmeter 3 impl --supersede")),
        (4, "orch", "PASS + green, waiting for merge", t + 20, s("merge sendmeter 4")),
        (5, "ci", "waiting for CI", t + 20, None),
        (6, "orch", "verdict written, waiting for orch to record it", t + 10, s(&format!("verdict sendmeter 6 PASS --sha {} --file {}", sha("6"), c.worktrees.join("rev-6/verdict.md").display()))),
        (7, "none", "idle without pushing (check the pane)", t, None),
        (8, "none", "nothing to do (merged): clean me", t + 20, None),
        (9, "none", "rev idle without a verdict (check the pane)", t + 10, None),
        (10, "none", "idle without pushing (check the pane)", t, None),
        (12, "none", "rev idle without a verdict (check the pane)", t + 10, None),
        (13, "none", "nothing to do (closed): clean me", t, None),
        (14, "orch", "verdict written, waiting for orch to record it", t + 10, None),
        (31, "orch", "pushed 70fe5a1, waiting for orch to spawn review", NOW - 180, s("spawn sendmeter 31 rev")),
        (42, "orch", "pushed fix 605297c, waiting for orch to spawn review", NOW - 480, s("spawn sendmeter 42 rev")),
    ];
    let got: Vec<_> = all.iter().map(|w| (w.issue, w.who, w.what.as_str(), w.since, w.next_cmd.clone())).collect();
    assert_eq!(got, want.to_vec(), "a working lane (#11) waits on nothing");
    assert_eq!(orch(&all, "sendmeter").unwrap().what, "orch idle with 6 items waiting");
    assert_eq!((all[0].tone(NOW), all[7].tone(NOW)), ("r", "a"), "owner red, rev idle without a verdict amber");
    assert_eq!((all[12].tone(NOW), all[13].tone(NOW)), ("m", "a"), "orch amber only past 5 min");
    // Every next command is one pasteable herdsman invocation: no empty args, no `PASS|FAIL`, no `?`.
    for cmd in all.iter().filter_map(|w| w.next_cmd.as_deref()) {
        let args: Vec<&str> = cmd.split(' ').collect();
        assert!(args[0] == "herdsman" && args.iter().all(|a| !a.is_empty() && !a.contains(['|', '?'])), "{cmd}");
        assert!(!cmd.contains("--sha") || args[args.iter().position(|a| *a == "--sha").unwrap() + 1].len() == 40, "{cmd}");
    }
}

#[test]
fn every_idle_lane_of_an_issue_gets_a_reason() {
    let (c, r) = fixture("waiting-lanes");
    let all = lanes(&c, &r, &[json!({"name": "sendmeter-rev-9-r1", "agent_status": "working"})]);
    let of = |lane: &str| all.iter().find(|x| x.1.lane == lane).map(|x| (x.1.who, x.1.what.as_str()));
    assert_eq!(of("sendmeter-rev-42-r1"), Some(("orch", "pushed fix 605297c, waiting for orch to spawn review")), "an older lane shares the issue's reason");
    assert_eq!(of("sendmeter-impl-3"), Some(("orch", "review FAIL, waiting for orch to start fix round")));
    assert_eq!(of("sendmeter-impl-9"), Some(("reviewer", "waiting on sendmeter-rev-9-r1 (working)")));
    assert_eq!(of("sendmeter-rev-9-r1"), None, "a working lane waits on nothing");
}

/// The orch lane plus one pushed impl lane, both idle unless `orch` says otherwise.
fn nudge_fixture(name: &str, orch: &str) -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir(name), CONFIG);
    ev(&c, NOW - 1200, None, "spawn", json!({"role": "orch", "agent": ORCH, "pane": "w1:p1"}));
    spawn(&c, NOW - 1200, 42, "impl", "sendmeter-impl-42");
    (c, pushed(orch, "605297c", NOW - 480))
}

fn pushed(orch: &str, head: &str, at: u64) -> Fake {
    Fake::default()
        .reply("herdr agent list", &agents(&[(ORCH, orch, "/x", "w1:p9"), ("sendmeter-impl-42", "idle", "/wt", "w1:p2")]))
        .reply("gh pr view issue-42 ", &pr(&sha(head), "pending"))
        .reply("git -C /wt/sendmeter-impl-42 ", &format!("{} {at}", sha(head)))
        .reply("herdr agent prompt", "")
        .reply("herdr pane send-keys", "")
}

fn check(c: &herdsman::Ctx, r: &Fake) -> String {
    let (code, out) = run(c, r, &["check"]);
    assert_eq!(code, 0, "{out}");
    out
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> usize {
    read(&c.state).unwrap().iter().filter(|e| e.kind == kind).count()
}

#[test]
fn nudge_once_per_state_then_orch_stuck_once_then_renudge_on_a_new_head() {
    let (mut c, r) = nudge_fixture("nudge", "idle");
    let line = "#42 pushed 605297c, waiting for orch to spawn review 8m ago: run herdsman spawn sendmeter 42 rev";
    let out = check(&c, &r);
    assert!(out.contains(&format!("sendmeter #42 orch-nudged: {line}")), "{out}");
    assert_eq!(r.count(&format!("herdr agent prompt {ORCH} {line}")), 1);
    assert_eq!(r.count("herdr pane send-keys w1:p9 Enter"), 1, "the pane as herdr shows it, not the spawn's w1:p1");
    assert_eq!(check(&c, &r), "", "no repeat on the next check");
    assert_eq!(r.count("herdr agent prompt"), 1);

    c.now = NOW + 9 * 60;
    assert!(!check(&c, &r).contains("orch-stuck"), "not before 10 min");
    c.now = NOW + 11 * 60;
    let out = check(&c, &r);
    assert!(out.contains("sendmeter #42 orch-stuck: still waiting: #42 pushed 605297c"), "{out}");
    assert!(!check(&c, &r).contains("orch-stuck"));
    assert_eq!((kinds(&c, "orch-stuck"), kinds(&c, "orch-nudged"), r.count("herdr agent prompt")), (1, 1, 1));

    // A new head is a new state: one more nudge (once the 60 s PR cache has turned over).
    c.now += 61;
    let r2 = pushed("idle", "abc1234", c.now - 400);
    assert!(check(&c, &r2).contains("orch-nudged: #42 pushed abc1234"));
    assert_eq!((r2.count("herdr agent prompt"), kinds(&c, "orch-nudged")), (1, 2));
}

#[test]
fn a_failed_enter_is_still_logged_so_the_line_is_never_typed_twice() {
    let (c, r) = nudge_fixture("nudge-enter", "idle");
    let r = r.once("herdr pane send-keys", 1, "");
    assert!(check(&c, &r).contains("orch-nudged"));
    check(&c, &r);
    assert_eq!((r.count("herdr agent prompt"), kinds(&c, "orch-nudged")), (1, 1));
}

#[test]
fn no_nudge_while_the_orch_is_working() {
    let (c, r) = nudge_fixture("nudge-working", "working");
    check(&c, &r);
    assert!(!r.called("herdr agent prompt"));
    assert_eq!(kinds(&c, "orch-nudged"), 0);
    let r = pushed("idle", "605297c", NOW - 480);
    check(&c, &r);
    assert_eq!((r.count("herdr agent prompt"), kinds(&c, "orch-nudged")), (1, 1), "nudged once it went idle");
}

#[test]
fn waiting_lists_orch_and_owner_items_with_their_next_command() {
    let (c, r) = fixture("waiting-cmd");
    let (code, out) = run(&c, &r, &["waiting", "sendmeter"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("sendmeter #42 orch: pushed fix 605297c, waiting for orch to spawn review (8m) → herdsman spawn sendmeter 42 rev"), "{out}");
    assert!(out.contains("sendmeter #1 owner: waiting on your answer (q3) (10m)\n"), "{out}");
    assert!(!out.contains("#5 ") && !out.contains("#7 "), "ci and none items are not listed: {out}");
    let (_, j) = run(&c, &r, &["waiting", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&j).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 7);
    assert!(out.contains("sendmeter #14 orch: verdict written, waiting for orch to record it (24m) → ?\n"), "unclear verdict word: {out}");
    let (_, st) = run(&c, &r, &["status", "sendmeter"]);
    assert!(st.contains("WAITING") && st.contains("pushed fix 605297c, waiting for orch to spawn review"), "{st}");
}

/// The owner's screenshot (2026-10-31 09:48): 9 lanes herdr calls `idle`, each now with its true reason.
#[test]
fn nine_idle_lanes_render_their_reason() {
    let (c, r) = fixture("nine-idle");
    run(&c, &r, &["waiting"]); // fills the PR cache the page reads
    let names = ["sendmeter-orch", "sendmeter-impl-1", "sendmeter-impl-42", "sendmeter-impl-31", "sendmeter-rev-3-r1", "sendmeter-rev-4-r1", "sendmeter-rev-6-r1", "sendmeter-impl-7", "sendmeter-impl-8"];
    let list: Vec<_> = names.iter().map(|n| json!({"name": n, "agent_status": "idle", "pane_id": format!("w1:{n}"), "cwd": "/x"})).collect();
    ev(&c, NOW - 3000, None, "spawn", json!({"role": "orch", "agent": ORCH, "pane": "w1:sendmeter-orch"}));
    let r = r.reply("herdr agent list", &json!({"result": {"agents": list}}).to_string()).reply("herdr workspace list", "{}").reply("herdr pane read", "");
    let s = Server::new(&c, &r, None);
    let page = String::from_utf8_lossy(&s.respond("GET /?tab=lanes HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    for (tone, label) in [
        ("a", "idle · orch idle with 6 items waiting (24m)"),
        ("r", "idle · waiting on your answer (q3) (10m)"),
        ("a", "idle · pushed fix 605297c, waiting for orch to spawn review (8m)"),
        ("m", "idle · pushed 70fe5a1, waiting for orch to spawn review (3m)"),
        ("a", "idle · review FAIL, waiting for orch to start fix round (24m)"),
        ("a", "idle · PASS + green, waiting for merge (24m)"),
        ("a", "idle · verdict written, waiting for orch to record it (24m)"),
        ("a", "idle · idle without pushing (check the pane) (25m)"),
        ("m", "idle · nothing to do (merged): clean me (24m)"),
    ] {
        let (what, a) = label.trim_start_matches("idle · ").rsplit_once(" (").unwrap();
        assert!(page.contains(&format!("<span class=\"nw {tone}\">idle {}</span><span class=\"why {tone}\">{what}</span>", a.trim_end_matches(')'))), "{label}:\n{page}");
    }
    assert!(!page.contains("<span class=\"nw a\">idle </span><span class=act>"), "no bare idle row");
    // #43: the sheet header is the row's card (reason included) above the Stop / Kill / Clean actions.
    assert!(page.contains("<div class=card hidden><p>#42</p><p>idle · pushed fix 605297c"), "{page}");
    assert!(std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/serve.js")).unwrap().contains("<div class=top></div><div class=acts><button type=button data-op=stop>Stop</button>"));
}

/// #43: a killed lane falls back to its prior state; the issue's surviving lane tells the orch what to respawn.
#[test]
fn a_killed_lane_waits_on_the_orch_to_respawn_it() {
    let c = ctx(&tmpdir("waiting-killed"), CONFIG);
    let t = NOW - 1500;
    spawn(&c, t, 42, "impl", "sendmeter-impl-42");
    spawn(&c, t + 10, 42, "rev", "sendmeter-rev-42-r1");
    ev(&c, t + 20, Some(42), "lane-killed", json!({"role": "rev", "agent": "sendmeter-rev-42-r1"}));
    spawn(&c, t, 3, "impl", "sendmeter-impl-3");
    spawn(&c, t + 10, 3, "rev", "sendmeter-rev-3-r1");
    verdict(&c, t + 20, 3, "FAIL", &sha("3"));
    spawn(&c, t + 30, 3, "impl", "sendmeter-impl-3");
    ev(&c, t + 40, Some(3), "lane-killed", json!({"role": "impl", "agent": "sendmeter-impl-3"}));
    let r = Fake::default()
        .reply("gh pr view issue-42 ", &pr(&sha("605297c"), "pending"))
        .reply("git -C /wt/sendmeter-impl-42 ", &format!("{} {}", sha("605297c"), NOW - 480))
        .reply("gh pr view issue-3 ", &pr(&sha("3"), "pending"));
    let got: Vec<_> = waits(&c, &r, &[]).into_iter().map(|w| (w.issue, w.lane, w.who, w.what, w.next_cmd)).collect();
    let s = |n: &str| Some(format!("herdsman {n}"));
    assert_eq!(got, vec![
        (3, "sendmeter-rev-3-r1".into(), "orch", "lane killed, waiting for orch to respawn impl".into(), s("spawn sendmeter 3 impl")),
        (42, "sendmeter-impl-42".into(), "orch", "pushed 605297c, waiting for orch to spawn review".into(), s("spawn sendmeter 42 rev")),
    ]);
}

/// #53: a 120-char reason gets its own line under a one-line name; an older lane of the issue says `superseded by`.
#[test]
fn a_long_reason_never_squeezes_the_name() {
    let c = ctx(&tmpdir("waiting-long"), CONFIG);
    let id = "q".repeat(120 - "waiting on your answer ()".len());
    spawn(&c, NOW - 2000, 2859, "rev", "synergy-rev-2859-r2");
    spawn(&c, NOW - 1900, 2859, "impl", "synergy-impl-2859");
    ev(&c, NOW - 600, Some(2859), "needs-decision", json!({"id": id, "agent": "synergy-impl-2859", "question": "?", "options": [], "recommend": null}));
    let list: Vec<_> = ["synergy-rev-2859-r2", "synergy-impl-2859"].iter().map(|n| json!({"name": n, "agent_status": "idle", "pane_id": format!("w1:{n}"), "cwd": "/x"})).collect();
    let r = Fake::default().reply("herdr agent list", &json!({"result": {"agents": list}}).to_string()).reply("herdr workspace list", "{}").reply("herdr pane read", "");
    let page = String::from_utf8_lossy(&Server::new(&c, &r, None).respond("GET /?tab=lanes HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    let reason = format!("waiting on your answer ({id})");
    assert_eq!(reason.len(), 120);
    assert!(page.contains(&format!("<span class=\"nw el\">synergy-impl-2859</span><span class=\"nw r\">idle 10m</span><span class=\"why r\">{reason}</span>")), "{page}");
    assert!(page.contains("<span class=\"nw el\">synergy-rev-2859-r2</span><span class=\"nw m\">idle 33m</span><span class=\"why m\">superseded by synergy-impl-2859</span>"), "{page}");
    assert_eq!(page.matches(&format!("{reason}</span>")).count(), 1, "the older lane does not repeat it; no `(10m)` suffix in the row");
}
