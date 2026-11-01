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
    assert!(
        !r.called("herdr pane close")
            && !r.called(&format!("git -C {wt} push"))
            && !r.called(&format!("git -C {wt} add"))
    );
    assert_eq!(kinds(&c, "lane-killed"), 0);
    let r = git(&wt, "?? .brief.md\n", "0\n", "");
    assert!(run(&c, &r, &["kill", LANE])
        .1
        .starts_with(&format!("{LANE} · impl · nothing unpushed · pane closes")));
}

#[test]
fn kill_pushes_wip_then_closes_and_logs() {
    let (c, wt) = lane("kill-yes");
    let r = git(&wt, " M src/a.rs\n", "2\n", "+fn x() {}\n");
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("killed {LANE}: work pushed to wip/{LANE}-20261031-1200")),
        "{out}"
    );
    let at = |p: &str| {
        r.index(&format!("git -C {wt} {p}"))
            .unwrap_or_else(|| panic!("no git {p}"))
    };
    assert!(
        at("add -A") < at("reset -q -- .brief.md")
            && at("reset -q -- .brief.md") < at("commit -q --no-verify -m wip: kill snapshot")
    );
    assert!(at("commit") < at(&format!("push origin HEAD:refs/heads/wip/{LANE}-20261031-1200")));
    assert!(at("push") < r.index("herdr pane close w1:p2").unwrap());
    // Never main/staging, never force: the one push names the wip ref.
    let pushes: Vec<String> = r
        .calls
        .borrow()
        .iter()
        .filter(|l| l.contains(" push "))
        .cloned()
        .collect();
    assert_eq!(
        pushes,
        [format!(
            "git -C {wt} push origin HEAD:refs/heads/wip/{LANE}-20261031-1200"
        )]
    );
    let e = read(&c.state).unwrap().pop().unwrap();
    assert_eq!(
        (e.kind.as_str(), e.str("wip"), e.str("role")),
        ("lane-killed", Some(&*format!("wip/{LANE}-20261031-1200")), Some("impl"))
    );
    // The impl goes back to queued (re-spawnable); a second kill finds no lane.
    let s = fold(&read(&c.state).unwrap())
        .remove(&("sendmeter".into(), 12))
        .unwrap();
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
    assert!(
        !r.called(&format!("git -C {wt} push"))
            && !r.called("herdr pane close")
            && !r.called(&format!("git -C {wt} commit"))
    );
    assert!(r.called(&format!("git -C {wt} reset -q")), "unstaged again");
    assert_eq!(kinds(&c, "lane-killed"), 0);
}

/// Hash-shaped lockfile checksums, headers and removed lines are not secrets; a token anywhere else is, even on a lockfile line.
#[test]
fn kill_skips_lockfile_checksums_but_not_secrets() {
    let head = "diff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n@@ -1,3 +1,3 @@\n";
    let lock = format!(
        "{head}+checksum = \"{}\"\n+  \"integrity\": \"sha512-{}==\",\n-OLD_TOKEN=ghp_abcdefghijklmnopqrstu1234\n",
        "ab".repeat(32),
        "Qx".repeat(43)
    );
    let (c, wt) = lane("kill-lock");
    let r = git(&wt, "", "1\n", &lock);
    assert_eq!(run(&c, &r, &["kill", LANE, "--yes"]).0, 0);
    assert!(r.called(&format!("git -C {wt} push origin HEAD:refs/heads/wip/")));
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc";
    for (i, bad) in [
        "+checksum = \"ghp_abcdefghijklmnopqrstu1234\"".to_string(),
        "+checksum = \"sk-abcdefghijklmnopqrstuvwxyz123456\"".into(),
        format!("+  \"integrity\": \"{jwt}\","),
        "+++ b/token.txt ghp_abcdefghijklmnopqrstu1234".into(),
        "diff --git a/.env b/.env\n+++ b/.env\n@@ -0,0 +1 @@\n+API_KEY=sk-abcdefghijklmnopqrstuvwxyz123456".into(),
    ]
    .iter()
    .enumerate()
    {
        let (c, wt) = lane(&format!("kill-lock-secret-{i}"));
        let r = git(&wt, "", "1\n", &format!("{lock}{bad}\n"));
        let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
        assert!(code == 3 && out.contains("REFUSED SECRET_IN_WIP"), "{bad}: {out}");
        assert!(
            !r.called(&format!("git -C {wt} push")) && !r.called("herdr pane close"),
            "{bad}"
        );
    }
}

/// Real git for the lane's worktree (a clone of a bare origin, `.brief.md` gitignored, the lane
/// scratch written), failing any git call containing `fail`; herdr and date are faked.
struct RealGit(Fake, &'static str);

impl herdsman::runner::Runner for RealGit {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<herdsman::runner::Out> {
        if prog != "git" {
            return self.0.run(prog, args);
        }
        self.0.calls.borrow_mut().push(format!("git {}", args.join(" ")));
        if !self.1.is_empty() && args.join(" ").contains(self.1) {
            return Ok(herdsman::runner::Out {
                code: 1,
                stdout: String::new(),
                stderr: "forced".into(),
            });
        }
        herdsman::runner::Real.run(prog, args)
    }
}

fn repo(wt: &str, fail: &'static str) -> RealGit {
    let origin = format!("{wt}.git");
    let sh = |a: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .args(a)
                .output()
                .unwrap()
                .status
                .success(),
            "{a:?}"
        )
    };
    sh(&["init", "-q", "--bare", &origin]);
    sh(&["clone", "-q", &origin, wt]);
    for (k, v) in [("user.name", "t"), ("user.email", "t@t"), ("commit.gpgsign", "false")] {
        sh(&["-C", wt, "config", k, v]);
    }
    std::fs::write(format!("{wt}/.gitignore"), ".brief.md\n").unwrap();
    sh(&["-C", wt, "add", "-A"]);
    sh(&["-C", wt, "commit", "-qm", "init"]);
    sh(&["-C", wt, "push", "-q", "origin", "HEAD"]);
    for f in [".brief.md", ".addendum.md", ".mcp.lane.json", "verdict.md"] {
        std::fs::write(format!("{wt}/{f}"), "scratch").unwrap();
    }
    RealGit(
        Fake::default()
            .reply("date -r", "20261031-1200\n")
            .reply("herdr pane", ""),
        fail,
    )
}

fn status(wt: &str) -> String {
    let o = std::process::Command::new("git")
        .args(["-C", wt, "status", "--porcelain", "--ignored"])
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn kill_counts_lane_scratch_as_clean_and_never_pushes_it() {
    let (c, wt) = lane("kill-scratch");
    let r = repo(&wt, "");
    let before = status(&wt);
    assert!(run(&c, &r, &["kill", LANE])
        .1
        .starts_with(&format!("{LANE} · impl · nothing unpushed · pane closes")));
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(
        (code, out.trim()),
        (
            0,
            &*format!("{LANE} · impl · nothing unpushed · pane closes, worktree kept\nkilled {LANE}")
        )
    );
    assert!(
        r.0.called("herdr pane close w1:p2")
            && !r.0.called(&format!("git -C {wt} push"))
            && !r.0.called(&format!("git -C {wt} add"))
    );
    assert_eq!(status(&wt), before);
}

#[test]
fn kill_snapshots_only_the_real_dirty_file() {
    let (c, wt) = lane("kill-real");
    let r = repo(&wt, "");
    std::fs::write(format!("{wt}/real.rs"), "fn x() {}\n").unwrap();
    assert!(run(&c, &r, &["kill", LANE])
        .1
        .contains("0 unpushed commit(s), 1 dirty file(s)"));
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 0, "{out}");
    let o = std::process::Command::new("git")
        .args([
            "-C",
            &format!("{wt}.git"),
            "show",
            "--name-only",
            "--format=",
            &format!("wip/{LANE}-20261031-1200"),
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(o.stdout).unwrap().trim(), "real.rs");
    assert!(r.0.called("herdr pane close w1:p2"));
}

#[test]
fn kill_restores_the_index_when_the_snapshot_fails() {
    let (c, wt) = lane("kill-snapfail");
    let r = repo(&wt, "diff --cached");
    std::fs::write(format!("{wt}/real.rs"), "fn x() {}\n").unwrap();
    let before = status(&wt);
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED SNAPSHOT_FAILED"), "{out}");
    assert!(
        r.0.called(&format!("git -C {wt} add -A"))
            && !r.0.called(&format!("git -C {wt} push"))
            && !r.0.called("herdr pane close")
    );
    assert_eq!(status(&wt), before);
    assert_eq!(kinds(&c, "lane-killed"), 0);
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
fn close_shuts_an_others_pane_and_refuses_lanes() {
    let (c, _) = lane("close");
    let list = agents(&[(LANE, "idle", "/x", "w1:p2"), ("dsh-tui-test", "idle", "/tmp", "w9:p1")]);
    let r = Fake::default()
        .reply("herdr agent list", &list)
        .reply("herdr pane close", "");
    let (code, out) = run(&c, &r, &["close", "dsh-tui-test"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "closed dsh-tui-test's pane; its worktree and files stay\n")
    );
    assert!(r.called("herdr pane close w9:p1") && !r.called("git"));
    let e = read(&c.state).unwrap().pop().unwrap();
    assert_eq!(
        (e.kind.as_str(), e.str("agent")),
        ("closed-other", Some("dsh-tui-test"))
    );
    // A lane, by name or by its pane id: IS_LANE, nothing closes.
    for who in [LANE, "w1:p2"] {
        let (code, out) = run(&c, &r, &["close", who]);
        assert!(code == 3 && out.starts_with("REFUSED IS_LANE"), "{out}");
    }
    assert!(!r.called("herdr pane close w1:p2"));
    assert_eq!(kinds(&c, "closed-other"), 1);
}

#[test]
fn clean_lane_refuses_unsafe_lanes() {
    let (c, wt) = lane("clean-lane");
    let r = git(&wt, "", "0\n", "").reply("herdr agent list", NO_AGENTS);
    let clean = |r: &Fake| run(&c, r, &["clean", "sendmeter", "--lane", LANE]);
    let (code, out) = clean(&r);
    assert!(
        code == 3 && out.contains("REFUSED UNSAFE_CLEAN: sendmeter-impl-12: only after merge"),
        "{out}"
    );
    // #83: an open PR is named, so the sheet's disabled [clean] says why without a tap.
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("pr-sendmeter-12.json"),
        json!({"ts": 1, "pr": {"number": 42, "state": "OPEN"}}).to_string(),
    )
    .unwrap();
    assert!(clean(&r)
        .1
        .contains("UNSAFE_CLEAN: sendmeter-impl-12: only after merge (PR #42 open)"));
    append(&c.state, &Event::new(T, "sendmeter", Some(12), "merged", json!({}))).unwrap();
    let live = git(&wt, "", "0\n", "").reply("herdr agent list", &agents(&[(LANE, "idle", "/x", "w1:p2")]));
    assert!(clean(&live).1.contains("UNSAFE_CLEAN: sendmeter-impl-12: live agent"));
    let dirty = git(&wt, " M a.rs\n", "0\n", "").reply("herdr agent list", NO_AGENTS);
    assert!(clean(&dirty)
        .1
        .contains("UNSAFE_CLEAN: sendmeter-impl-12: 1 uncommitted change(s)"));
    let (code, out) = clean(&r);
    assert!(code == 0 && out.contains(&format!("would remove {wt}")), "{out}");
    assert!(run(&c, &r, &["clean", "sendmeter", "--lane", "nobody"])
        .1
        .contains("UNSAFE_CLEAN: nobody: no worktree"));
}

fn req(s: &Server, path: &str, body: &str, extra: &str) -> String {
    let r = format!(
        "POST {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    String::from_utf8_lossy(&s.respond(&r, T)).into_owned()
}

#[test]
fn act_needs_a_paired_cookie_and_logs_via_web() {
    let (c, wt) = lane("act");
    let r = git(&wt, "", "0\n", "");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    assert!(req(&s, "/act", &format!("agent={LANE}&op=stop"), "").starts_with("HTTP/1.1 401 "));
    assert!(req(
        &s,
        "/act",
        &format!("agent={LANE}&op=stop"),
        "Origin: https://evil.example\r\n"
    )
    .starts_with("HTTP/1.1 403 "));
    assert!(!r.called("herdr pane send-keys"));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let res = req(&s, "/act", &format!("agent={LANE}&op=stop"), &cookie);
    assert!(
        res.starts_with("HTTP/1.1 200 OK") && res.ends_with("sent Esc to sendmeter-impl-12\n"),
        "{res}"
    );
    let e = read(&c.state).unwrap().pop().unwrap();
    let label = herdsman::devices::load(&c)[0].label.clone();
    assert_eq!(
        (e.kind.as_str(), e.str("via"), e.str("device"), e.str("op")),
        ("act", Some("web"), Some(&*label), Some("stop"))
    );
    // Kill without confirm is the dry-run line; nothing closes, nothing logged.
    let res = req(&s, "/act", &format!("agent={LANE}&op=kill"), &cookie);
    assert!(
        res.contains("pane closes, worktree kept") && !r.called("herdr pane close"),
        "{res}"
    );
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
        assert_eq!(
            items[1]["text"],
            format!("⏺ Done {i}, a long reply that wraps\n  onto a second line")
        );
    }
    let tail = herdsman::lanes::turns(&lines[198..]);
    let hermes = &tail[0]["items"].as_array().unwrap()[1];
    assert_eq!(
        (&hermes["tool"], &hermes["body"]),
        (&json!("┊ 💻 terminal ls"), &json!("┊   a.rs b.rs"))
    );
}

#[test]
fn lane_loads_more_from_the_scrollback() {
    let (c, _) = lane("more");
    let list = agents(&[(LANE, "working", "/x", "w1:p2")]);
    let r = Fake::default()
        .reply("herdr agent list", &list)
        .reply("herdr workspace list", "{}")
        .reply("herdr pane read", &screen());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "", T);
    let get = |q: &str| {
        let res = String::from_utf8_lossy(&s.respond(
            &format!("GET /lane?agent={LANE}{q} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}\r\n"),
            T,
        ))
        .into_owned();
        serde_json::from_str::<serde_json::Value>(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    };
    let first = get("&lines=500");
    assert_eq!(
        (&first["before"], &first["more"]),
        (&json!(200), &json!(true)),
        "the cap stays at 200"
    );
    let older = get("&lines=200&before=200");
    assert_eq!((&older["before"], &older["more"]), (&json!(202), &json!(false)));
    assert_eq!(older["turns"][0]["items"][0]["tool"], "⏺ Bash(cargo test 0)");
    assert!(r.called("herdr pane read w1:p2 --source recent --lines 400"));
}

/// iOS zooms on focus under 16 px; the fix is the font size. #114: the page is app-like, so no pinch or double-tap zoom.
#[test]
fn inputs_are_16px_and_zoom_is_off() {
    let css = include_str!("../assets/serve.css");
    let rule = css.lines().find(|l| l.starts_with(".reply textarea{")).unwrap();
    assert!(
        rule.ends_with("font-size:var(--fs-input)}") && css.contains("--fs-input:16px"),
        "{rule}"
    );
    let (c, _) = lane("zoom");
    let page = String::from_utf8_lossy(&Server::new(&c, &Fake::default(), None).respond("GET / HTTP/1.1\r\n\r\n", T))
        .into_owned();
    assert!(page.contains("initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover"));
    assert!(css.contains(".lock body{position:fixed") && include_str!("../assets/serve.js").contains("visualViewport"));
}

/// #96: the push guard flags real secret shapes only, never code, test names or words like `key`/`token`.
#[test]
fn secret_flags_real_shapes_only() {
    use herdsman::lanes::secret;
    for ok in [
        "fn check_prompts_an_idle_orch_after_5_min_then_alerts_after_15() {",
        "    let api_key = config.key();",
        "fn token_for(fleet: &str) -> Option<String> {",
        "let token = std::env::var(\"GITHUB_TOKEN\").unwrap_or_default();",
        "KEY_PATH=/etc/ssl/private/server_2026.pem",
        "let secret_name = \"orch_idle_after_15_min_alert\";",
        "checksum = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"",
        "a3f9c2e1b7d4058a6e3f9c2e1b7d4058a6e3f9c2 src/x.rs",
        "    \"integrity\": \"sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==\",",
        "    \"apiKey\": \"config_api_key_from_env\",",
    ] {
        assert_eq!(secret(ok), None, "{ok}");
    }
    let pem = "-----BEGIN RSA PRIVATE KEY-----";
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    for (bad, v) in [
        (
            format!("TOKEN = \"ghp_{}\"", "aB3".repeat(12)),
            format!("ghp_{}", "aB3".repeat(12)),
        ),
        (
            "key: sk-ant-api03-abcdefghijklmnop1234".into(),
            "sk-ant-api03-abcdefghijklmnop1234".into(),
        ),
        (
            "aws_access_key_id = AKIAIOSFODNN7EXAMPLE".into(),
            "AKIAIOSFODNN7EXAMPLE".into(),
        ),
        (format!("  {pem}"), pem.into()),
        (format!("Authorization: Bearer {jwt}"), jwt.into()),
        ("SECRET=\"h7Gk2pQ9xLm4vB8nR3tZ\"".into(), "h7Gk2pQ9xLm4vB8nR3tZ".into()),
        (
            "API_KEY=\"h7Gk2pQ9xLm4vB8nR3tZ\" // prod".into(),
            "h7Gk2pQ9xLm4vB8nR3tZ".into(),
        ),
        (
            "API_KEY=h7Gk2pQ9xLm4vB8nR3tZ # note".into(),
            "h7Gk2pQ9xLm4vB8nR3tZ".into(),
        ),
        (
            "  \"apiKey\": \"h7Gk2pQ9xLm4vB8nR3tZ\",".into(),
            "h7Gk2pQ9xLm4vB8nR3tZ".into(),
        ),
        ("api_key: h7Gk2pQ9xLm4vB8nR3tZ".into(), "h7Gk2pQ9xLm4vB8nR3tZ".into()),
        (
            format!("x = gho_{}", "aB3".repeat(12)),
            format!("gho_{}", "aB3".repeat(12)),
        ),
        (
            format!("github_pat_11{}", "aB3".repeat(20)),
            format!("github_pat_11{}", "aB3".repeat(20)),
        ),
        (
            "slack: xoxb-1234567890-abcdefghijkl".into(),
            "xoxb-1234567890-abcdefghijkl".into(),
        ),
        (
            "slack: xoxa-2-1234567890-abcdefghij".into(),
            "xoxa-2-1234567890-abcdefghij".into(),
        ),
        (
            "slack: xoxp-1234567890-abcdefghijkl".into(),
            "xoxp-1234567890-abcdefghijkl".into(),
        ),
        (
            "bot 123456789:AAHfK3x9Lm4vB8nR3tZqW7eY2uI5oP1aS6d ok".into(),
            "123456789:AAHfK3x9Lm4vB8nR3tZqW7eY2uI5oP1aS6d".into(),
        ),
        (
            "TELEGRAM_BOT_TOKEN=123456789:AAHfK3x9Lm4vB8nR3tZqW7eY2uI5oP1aS6d".into(),
            "123456789:AAHfK3x9Lm4vB8nR3tZqW7eY2uI5oP1aS6d".into(),
        ),
    ] {
        assert_eq!(secret(&bad), Some(v.as_str()), "{bad}");
    }
}

/// #96: #89's diff (a long snake_case test name) saves; a secret's refusal names file:line with the value masked.
#[test]
fn kill_saves_code_and_names_the_secret_line() {
    let ok = "diff --git a/src/check.rs b/src/check.rs\n+++ b/src/check.rs\n@@ -10,2 +10,3 @@\n \
              fn a() {}\n+fn check_prompts_an_idle_orch_after_5_min_then_alerts_after_15() {}\n";
    let (c, wt) = lane("kill-96-ok");
    let r = git(&wt, "", "2\n", ok);
    assert_eq!(run(&c, &r, &["kill", LANE, "--yes"]).0, 0);
    assert!(r.called(&format!("git -C {wt} push origin HEAD:refs/heads/wip/")));
    let bad = format!(
        "{ok}diff --git a/.env b/.env\n+++ b/.env\n@@ -3,1 +3,2 @@\n A=1\n-B=2\n+SECRET=\"h7Gk2pQ9xLm4vB8nR3tZ\" ghp_{}\n",
        "aB3".repeat(12)
    );
    let (c, wt) = lane("kill-96-bad");
    let r = git(&wt, "", "2\n", &bad);
    let (code, out) = run(&c, &r, &["kill", LANE, "--yes"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("(.env:4: SECRET=\"•••\" •••)") && !out.contains("h7Gk") && !out.contains("ghp_"),
        "{out}"
    );
    assert!(!r.called(&format!("git -C {wt} push")));
}
