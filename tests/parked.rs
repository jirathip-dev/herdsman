//! #83: a lane whose pane is gone but whose work is safe (clean tree, nothing unpushed) is `parked`, not `dead`.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const NOW: u64 = 1_000_000;
const SAFE: &str = "sendmeter-impl-12";
const RISKY: &str = "sendmeter-impl-14";

/// impl #12 (clean, pushed, PR #42 open) and impl #14 (dirty), both without a herdr pane.
fn fixture(name: &str, paused: bool) -> (herdsman::Ctx, Fake) {
    let cfg = CONFIG.replace(
        "integration = \"staging\"   # never main",
        &format!("integration = \"staging\"\npaused = {paused}"),
    );
    let c = ctx(&tmpdir(name), &cfg);
    for (n, agent) in [(12, SAFE), (14, RISKY)] {
        let d = json!({"role": "impl", "agent": agent, "harness": "claude", "pane": format!("w1:{n}"), "worktree": format!("/wt/{agent}")});
        append(&c.state, &Event::new(NOW - 3 * 3600, "sendmeter", Some(n), "spawn", d)).unwrap();
    }
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let pr = json!({"number": 42, "state": "OPEN", "headRefOid": "6a258db".to_string() + &"0".repeat(33), "statusCheckRollup": []});
    std::fs::write(
        cache.join("pr-sendmeter-12.json"),
        json!({"ts": 1, "branch": "issue-12", "pr": pr}).to_string(),
    )
    .unwrap();
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("herdr workspace list", r#"{"result":{"workspaces":[]}}"#)
        .reply(&format!("git -C /wt/{SAFE} status"), "?? .brief.md\n")
        .reply(&format!("git -C /wt/{SAFE} rev-list"), "0\n")
        .reply(&format!("git -C /wt/{SAFE} rev-parse"), "6a258db\n")
        .reply(
            &format!("git -C /wt/{SAFE} log"),
            &format!("6a258db{} {}", "0".repeat(33), NOW - 7200),
        )
        .reply(&format!("git -C /wt/{RISKY} status"), " M src/a.rs\n");
    (c, r)
}

fn get(s: &Server, path: &str) -> String {
    let res = String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\n\r\n"), NOW)).into_owned();
    res[res.find("\r\n\r\n").unwrap() + 4..].to_string()
}

/// The lane's row in the Lanes tab (its button line plus the hidden sheet card).
fn lane<'a>(page: &'a str, agent: &str) -> &'a str {
    let a = page
        .find(&format!("<div class=lane data-agent=\"{agent}\""))
        .unwrap_or_else(|| panic!("{agent} row missing"));
    &page[a..a + page[a..].find("</div></div>").unwrap()]
}

#[test]
fn gone_pane_with_safe_work_is_parked_dirty_is_dead() {
    let (c, r) = fixture("parked-sheet", true);
    let s = Server::new(&c, &r, None);
    let page = get(&s, "/");
    let safe = lane(&page, SAFE);
    assert!(
        safe.contains("data-role=impl data-gone>"),
        "no pane: stop/kill hidden: {safe}"
    );
    assert!(safe.contains("<p>parked · work on PR #42</p>"), "{safe}");
    assert!(
        safe.contains("<span class=\"why m\">parked: fleet paused (resume to review PR #42)</span>"),
        "{safe}"
    );
    assert!(
        !safe.contains("class=r") && !safe.contains("mismatch"),
        "neutral, no red: {safe}"
    );
    let risky = lane(&page, RISKY);
    assert!(
        risky.contains("⚠ log/pane mismatch") && risky.contains("<span class=\"nw r\">dead"),
        "{risky}"
    );
    assert!(
        page.contains("<h2><span class=m>parked (1)</span></h2>")
            && page.contains("<h2><span class=r>dead (1)</span></h2>")
    );
    // The Queue tab's rows: parked in neutral, dead in red.
    assert!(
        page.contains("<span>#12</span><span class=m>parked</span>")
            && page.contains("<span>#14</span><span class=r>dead</span>"),
        "queue rows"
    );
    let herd: serde_json::Value = serde_json::from_str(&get(&s, "/herd.json")).unwrap();
    let state = |n: &str| {
        herd["fleets"][0]["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["name"] == n)
            .map(|l| (l["state"].clone(), l["shown"].clone()))
    };
    assert_eq!(state(SAFE), Some((json!("parked"), json!("parked · work on PR #42"))));
    assert_eq!(state(RISKY).map(|s| s.0), Some(json!("dead")));
    // `status`: the paused fleet's reason.
    let (code, out) = run(&c, &r, &["status", "sendmeter"]);
    assert_eq!(code, 0, "{out}");
    let row = out
        .lines()
        .find(|l| l.contains("#12") || l.contains(" 12 "))
        .unwrap_or_default();
    assert!(row.ends_with("parked: fleet paused (resume to review PR #42)"), "{out}");
}

#[test]
fn parked_lane_alerts_once_and_never_stalls() {
    let (mut c, r) = fixture("parked-check", true);
    for at in [NOW, NOW + 3600, NOW + 7200] {
        c.now = at;
        let (code, out) = run(&c, &r, &["check"]);
        assert_eq!(code, 0, "{out}");
    }
    let n = |kind: &str, issue: u64| {
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind && e.issue == Some(issue))
            .count()
    };
    assert_eq!(
        (n("lane-dead", 12), n("stall", 12)),
        (1, 0),
        "parked: one lane-dead, no stall"
    );
    assert_eq!(
        (n("lane-dead", 14), n("stall", 14)),
        (1, 1),
        "dead (dirty) keeps its alerts"
    );
}

#[test]
fn unpaused_fleet_resumes_a_parked_impl_to_review() {
    let (c, r) = fixture("parked-resume", false);
    let events = read(&c.state).unwrap();
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    let waits = herdsman::waiting::scan(&c, &cfg, &r, &events, &[], false);
    let w = &waits.iter().find(|(s, _)| s.issue == 12).unwrap().1;
    assert_eq!(
        (w.who, w.what.as_str()),
        ("orch", "pushed 6a258db, waiting for orch to spawn review")
    );
    assert_eq!(w.next_cmd.as_deref(), Some("herdsman spawn sendmeter 12 rev"));
}

/// #128 amendment A: a lane `clean` removed (issue closed on GitHub) is never dead; a truly vanished lane still is.
#[test]
fn cleaned_lane_of_a_closed_issue_is_not_dead() {
    let (c, r) = fixture("closed-not-dead", false);
    let check = |c: &herdsman::Ctx| {
        let (code, out) = run(c, &r, &["check"]);
        assert_eq!(code, 0, "{out}");
        read(&c.state).unwrap()
    };
    let hits = |ev: &[Event], n: u64| {
        ev.iter()
            .any(|e| e.issue == Some(n) && matches!(e.kind.as_str(), "stall" | "lane-dead"))
    };
    let ev = check(&c);
    assert!(hits(&ev, 14), "a vanished lane with no clean is dead");
    let page = get(&Server::new(&c, &r, None), "/");
    assert!(lane(&page, RISKY).contains("dead"), "{page}");

    let c = fixture("closed-cleaned", false).0;
    let d = json!({"worktree": format!("/wt/{RISKY}"), "pane": "w1:14", "why": "issue closed"});
    append(&c.state, &Event::new(NOW - 3600, "sendmeter", Some(14), "clean", d)).unwrap();
    let st = herdsman::events::fold(&read(&c.state).unwrap());
    assert_eq!(st[&("sendmeter".to_string(), 14)].state, "closed");
    assert!(!hits(&check(&c), 14), "no stall or lane-dead for a closed issue");
    let page = get(&Server::new(&c, &r, None), "/");
    assert!(
        !page.contains(&format!("data-agent=\"{RISKY}\"")),
        "no Dead row: {page}"
    );
    let (code, waiting) = run(&c, &r, &["waiting"]);
    assert!(code == 0 && !waiting.contains("#14"), "{waiting}");
}
