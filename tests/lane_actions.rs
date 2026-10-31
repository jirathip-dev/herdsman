//! #43: lane actions (`herdsman stop|kill|clean --lane`, the page's `/act`) and the per-turn transcript.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;
const LANE: &str = "sendmeter-impl-12";

/// sendmeter #12's impl lane in a real (empty) worktree dir, pane w1:p2; git answers per test.
fn lane(name: &str) -> (herdsman::Ctx, String) {
    let d = tmpdir(name);
    let c = ctx(&d, CONFIG);
    let wt = d.join("wt").to_string_lossy().into_owned();
    std::fs::create_dir_all(&wt).unwrap();
    let sp = json!({"role": "impl", "agent": LANE, "pane": "w1:p2", "worktree": wt, "branch": "issue-12"});
    append(&c.state, &Event::new(T - 60, "sendmeter", Some(12), "spawn", sp)).unwrap();
    (c, wt)
}

fn git(wt: &str, status: &str, ahead: &str, log: &str) -> Fake {
    Fake::default()
        .reply(&format!("git -C {wt} status --porcelain"), status)
        .reply(&format!("git -C {wt} rev-list --count"), ahead)
        .reply(&format!("git -C {wt} log -p"), log)
        .reply(&format!("git -C {wt} "), "")
        .reply("date -r", "20261031-1200\n")
        .reply("herdr pane", "")
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> usize {
    read(&c.state).unwrap().iter().filter(|e| e.kind == kind).count()
}

#[test]
fn kill_dry_run_lists_what_is_lost() {
    let (c, wt) = lane("kill-dry");
    let r = git(&wt, "?? .brief.md\n M src/a.rs\n?? new.rs\n", "3\n", "");
    let (code, out) = run(&c, &r, &["kill", LANE]);
    assert_eq!(code, 0, "{out}");
    let want = format!("{LANE} · impl · 3 unpushed commit(s), 2 dirty file(s) → wip/{LANE}-20261031-1200 · pane closes, worktree kept\n");
    assert!(out.starts_with(&want) && out.contains("dry run"), "{out}");
    assert!(!r.called("herdr pane close") && !r.called(&format!("git -C {wt} push")) && !r.called(&format!("git -C {wt} add")));
    assert_eq!(kinds(&c, "lane-killed"), 0);
    let r = git(&wt, "?? .brief.md\n", "0\n", "");
    assert!(run(&c, &r, &["kill", LANE]).1.starts_with(&format!("{LANE} · impl · nothing unpushed · pane closes")));
}

#[test]
fn kill_pushes_wip_then_closes_and_logs() {
    let (c, wt) = lane("kill-yes");
    let r = git(&wt, " M src/a.rs\n", "2\n", "+fn x() {}\n");
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("killed {LANE}: work pushed to wip/{LANE}-20261031-1200")), "{out}");
    let at = |p: &str| r.index(&format!("git -C {wt} {p}")).unwrap_or_else(|| panic!("no git {p}"));
    assert!(at("add -A -- . :(exclude).brief.md") < at("commit -q --no-verify -m wip: kill snapshot"));
    assert!(at("commit") < at(&format!("push origin HEAD:refs/heads/wip/{LANE}-20261031-1200")));
    assert!(at("push") < r.index("herdr pane close w1:p2").unwrap());
    // Never main/staging, never force: the one push names the wip ref.
    let pushes: Vec<String> = r.calls.borrow().iter().filter(|l| l.contains(" push ")).cloned().collect();
    assert_eq!(pushes, [format!("git -C {wt} push origin HEAD:refs/heads/wip/{LANE}-20261031-1200")]);
    let e = read(&c.state).unwrap().pop().unwrap();
    assert_eq!((e.kind.as_str(), e.str("wip"), e.str("role")), ("lane-killed", Some(&*format!("wip/{LANE}-20261031-1200")), Some("impl")));
    // The impl goes back to queued (re-spawnable); a second kill finds no lane.
    let s = fold(&read(&c.state).unwrap()).remove(&("sendmeter".into(), 12)).unwrap();
    assert_eq!((s.state.as_str(), s.unfinished("impl")), ("queued", false));
    assert!(run(&c, &r, &["kill", LANE]).1.starts_with("REFUSED NO_SUCH_AGENT"));
}

#[test]
fn kill_refuses_a_secret_in_the_wip_diff() {
    let (c, wt) = lane("kill-secret");
    let r = git(&wt, "", "1\n", "+TOKEN = \"ghp_abcdefghijklmnopqrstu1234\"\n");
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED SECRET_IN_WIP"), "{out}");
    assert!(!out.contains("ghp_abc"), "the secret is never printed");
    assert!(!r.called(&format!("git -C {wt} push")) && !r.called("herdr pane close") && !r.called(&format!("git -C {wt} commit")));
    assert!(r.called(&format!("git -C {wt} reset -q")), "unstaged again");
    assert_eq!(kinds(&c, "lane-killed"), 0);
}

/// Lockfile checksums and removed lines are not secrets; a real token beside them still is.
#[test]
fn kill_skips_lockfile_checksums_but_not_secrets() {
    let (c, wt) = lane("kill-lock");
    let lock = format!("+++ b/Cargo.lock\n+checksum = \"{}\"\n+  \"integrity\": \"sha512-{}==\",\n-OLD_TOKEN=ghp_abcdefghijklmnopqrstu1234\n", "ab".repeat(32), "Qx".repeat(40));
    let r = git(&wt, "", "1\n", &lock);
    assert_eq!(run(&c, &r, &["kill", LANE, "--yes"]).0, 0);
    let (c, wt) = lane("kill-lock-secret");
    let r = git(&wt, "", "1\n", &format!("{lock}+API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456\n"));
    assert!(run(&c, &r, &["kill", LANE, "--yes"]).1.contains("REFUSED SECRET_IN_WIP"));
}

#[test]
fn stop_sends_escape() {
    let (c, wt) = lane("stop");
    let r = git(&wt, "", "0\n", "");
    let (code, out) = run(&c, &r, &["stop", LANE]);
    assert_eq!((code, out.as_str()), (0, "sent Esc to sendmeter-impl-12\n"));
    assert!(r.called("herdr pane send-keys w1:p2 Escape"));
    assert_eq!(kinds(&c, "stop"), 1);
    let (code, out) = run(&c, &r, &["stop", "nobody"]);
    assert!(code == 3 && out.starts_with("REFUSED NO_SUCH_AGENT"), "{out}");
}

#[test]
fn clean_lane_refuses_unsafe_lanes() {
    let (c, wt) = lane("clean-lane");
    let r = git(&wt, "", "0\n", "").reply("herdr agent list", NO_AGENTS);
    let clean = |r: &Fake| run(&c, r, &["clean", "sendmeter", "--lane", LANE]);
    let (code, out) = clean(&r);
    assert!(code == 3 && out.contains("REFUSED UNSAFE_CLEAN: sendmeter-impl-12: not merged"), "{out}");
    append(&c.state, &Event::new(T, "sendmeter", Some(12), "merged", json!({}))).unwrap();
    let live = git(&wt, "", "0\n", "").reply("herdr agent list", &agents(&[(LANE, "idle", "/x", "w1:p2")]));
    assert!(clean(&live).1.contains("UNSAFE_CLEAN: sendmeter-impl-12: live agent"));
    let dirty = git(&wt, " M a.rs\n", "0\n", "").reply("herdr agent list", NO_AGENTS);
    assert!(clean(&dirty).1.contains("UNSAFE_CLEAN: sendmeter-impl-12: 1 uncommitted change(s)"));
    let (code, out) = clean(&r);
    assert!(code == 0 && out.contains(&format!("would remove {wt}")), "{out}");
    assert!(run(&c, &r, &["clean", "sendmeter", "--lane", "nobody"]).1.contains("UNSAFE_CLEAN: nobody: no worktree"));
}

fn req(s: &Server, path: &str, body: &str, extra: &str) -> String {
    let r = format!("POST {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    String::from_utf8_lossy(&s.respond(&r, T)).into_owned()
}

#[test]
fn act_needs_a_paired_cookie_and_logs_via_web() {
    let (c, wt) = lane("act");
    let r = git(&wt, "", "0\n", "");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    assert!(req(&s, "/act", &format!("agent={LANE}&op=stop"), "").starts_with("HTTP/1.1 401 "));
    assert!(req(&s, "/act", &format!("agent={LANE}&op=stop"), "Origin: https://evil.example\r\n").starts_with("HTTP/1.1 403 "));
    assert!(!r.called("herdr pane send-keys"));
    let (_, code) = run(&c, &Fake::default(), &["serve", "pair"]);
    let code: String = code.chars().filter(char::is_ascii_digit).take(6).collect();
    let res = req(&s, "/pair", &format!("code={code}"), "User-Agent: Mozilla/5.0 (iPhone)\r\n");
    let set = res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap();
    let cookie = format!("Cookie: {}\r\n", &set[..set.find(';').unwrap()]);
    let res = req(&s, "/act", &format!("agent={LANE}&op=stop"), &cookie);
    assert!(res.starts_with("HTTP/1.1 200 OK") && res.ends_with("sent Esc to sendmeter-impl-12\n"), "{res}");
    let e = read(&c.state).unwrap().pop().unwrap();
    let label = herdsman::devices::load(&c)[0].label.clone();
    assert_eq!((e.kind.as_str(), e.str("via"), e.str("device"), e.str("op")), ("act", Some("web"), Some(&*label), Some("stop")));
    // Kill without confirm is the dry-run line; nothing closes, nothing logged.
    let res = req(&s, "/act", &format!("agent={LANE}&op=kill"), &cookie);
    assert!(res.contains("pane closes, worktree kept") && !r.called("herdr pane close"), "{res}");
    assert_eq!(kinds(&c, "act"), 1);
}

/// 20 Claude turns of 10 lines (prompt, a tool call with 6 output lines, a 2-line reply) and a Hermes `┊` run.
fn screen() -> String {
    let mut s = String::new();
    for i in 0..20 {
        s += &format!("> prompt {i}\n⏺ Bash(cargo test {i})\n  ⎿ running\n");
        s += &(0..5).map(|k| format!("    out {k}\n")).collect::<String>();
        s += &format!("⏺ Done {i}, a long reply that wraps\n  onto a second line\n");
    }
    s + "┊ 💻 terminal ls\n┊   a.rs b.rs\n"
}

#[test]
fn transcript_groups_200_lines_into_turns_with_tools_collapsed() {
    let lines = herdsman::lanes::tail(&screen(), 202);
    let turns = herdsman::lanes::turns(&lines[..200]);
    assert_eq!(turns.len(), 20);
    for (i, t) in turns.iter().enumerate() {
        let items = t["items"].as_array().unwrap();
        assert_eq!(t["me"], format!("> prompt {i}"));
        assert_eq!(items.len(), 2, "{t}");
        assert_eq!(items[0]["tool"], format!("⏺ Bash(cargo test {i})"));
        assert_eq!(items[0]["body"].as_str().unwrap().lines().count(), 6);
        assert_eq!(items[1]["text"], format!("⏺ Done {i}, a long reply that wraps\n  onto a second line"));
    }
    let tail = herdsman::lanes::turns(&lines[198..]);
    let hermes = &tail[0]["items"].as_array().unwrap()[1];
    assert_eq!((&hermes["tool"], &hermes["body"]), (&json!("┊ 💻 terminal ls"), &json!("┊   a.rs b.rs")));
}

#[test]
fn lane_loads_more_from_the_scrollback() {
    let (c, _) = lane("more");
    let list = agents(&[(LANE, "working", "/x", "w1:p2")]);
    let r = Fake::default().reply("herdr agent list", &list).reply("herdr workspace list", "{}").reply("herdr pane read", &screen());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (_, code) = run(&c, &Fake::default(), &["serve", "pair"]);
    let code: String = code.chars().filter(char::is_ascii_digit).take(6).collect();
    let set = req(&s, "/pair", &format!("code={code}"), "");
    let cookie = format!("Cookie: {}\r\n", &set[set.find("hd=").unwrap()..set.find("; Path").unwrap()]);
    let get = |q: &str| {
        let res = String::from_utf8_lossy(&s.respond(&format!("GET /lane?agent={LANE}{q} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}\r\n"), T)).into_owned();
        serde_json::from_str::<serde_json::Value>(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    };
    let first = get("&lines=500");
    assert_eq!((&first["before"], &first["more"]), (&json!(200), &json!(true)), "the cap stays at 200");
    let older = get("&lines=200&before=200");
    assert_eq!((&older["before"], &older["more"]), (&json!(202), &json!(false)));
    assert_eq!(older["turns"][0]["items"][0]["tool"], "⏺ Bash(cargo test 0)");
    assert!(r.called("herdr pane read w1:p2 --source recent --lines 400"));
}

/// iOS zooms on focus under 16 px; the fix is the font size, never `maximum-scale`.
#[test]
fn inputs_are_16px_and_zoom_stays_allowed() {
    let css = include_str!("../assets/serve.css");
    let rule = css.lines().find(|l| l.starts_with(".reply textarea,.pairf input{")).unwrap();
    assert!(rule.ends_with("font-size:16px}"), "{rule}");
    let (c, _) = lane("zoom");
    let page = String::from_utf8_lossy(&Server::new(&c, &Fake::default(), None).respond("GET / HTTP/1.1\r\n\r\n", T)).into_owned();
    assert!(page.contains("<meta name=viewport") && !page.contains("maximum-scale"));
    assert!(css.contains(".lock body{position:fixed") && include_str!("../assets/serve.js").contains("visualViewport"));
}
