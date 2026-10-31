//! #5: per-role Anthropic accounts with cap fallback; per-role MCP allowlist.
mod common;
use common::*;
use herdsman::events::read;
use serde_json::Value;
use std::path::Path;

const TOKEN_A: &str = "sk-ant-oat01-AAAA-secret";
const TOKEN_B: &str = "sk-ant-oat01-BBBB-secret";

/// Claude impl role with `role_extra` lines, `[accounts]` A/B/C from `<dir>/tokens.env`, local repo `<dir>/repo`.
fn config(d: &Path, role_extra: &str) -> String {
    let env = d.join("tokens.env");
    std::fs::write(&env, format!("CLAUDE_CODE_OAUTH_TOKEN_A={TOKEN_A}\nexport CLAUDE_CODE_OAUTH_TOKEN_B=\"{TOKEN_B}\"\nCLAUDE_CODE_OAUTH_TOKEN_C=c\n")).unwrap();
    let repo = d.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let mcp = r#"{"mcpServers":{"supabase-dev":{"command":"npx","args":["dev"]},"supabase-prod":{"command":"npx","args":["prod"]},"other":{"url":"http://x"}}}"#;
    std::fs::write(repo.join(".mcp.json"), mcp).unwrap();
    CONFIG
        .replace("harness  =  \"hermes\"    # toggled in tests", &format!("harness = \"claude\"\n{role_extra}"))
        .replace("/tmp/does-not-matter/sendmeter", repo.to_str().unwrap())
        + &format!(
            "\n[accounts]\naccounts_env = \"{}\"\nA = \"CLAUDE_CODE_OAUTH_TOKEN_A\"\nB = \"CLAUDE_CODE_OAUTH_TOKEN_B\"\nC = \"CLAUDE_CODE_OAUTH_TOKEN_C\"\n",
            env.display()
        )
}

/// A real (faked herdr/gh/git) impl spawn of issue `n` in pane `w9:p<n>`.
fn spawn(c: &herdsman::Ctx, n: u64) -> Fake {
    let wt = c.worktrees.join(format!("sendmeter/issue-{n}"));
    std::fs::create_dir_all(&wt).unwrap();
    let pane = format!("w9:p{n}");
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply(&format!("gh issue view {n}"), r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply("herdr worktree open", r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#)
        .reply("herdr pane list --workspace w9", &format!(r#"{{"result":{{"panes":[{{"pane_id":"{pane}","agent":null}}]}}}}"#))
        .reply(&format!("herdr pane run {pane} export"), "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr agent wait", "{}");
    let (code, out) = run(c, &r, &["spawn", "sendmeter", &n.to_string(), "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert!(!out.contains(TOKEN_A) && !out.contains(TOKEN_B), "token in stdout: {out}");
    r
}

fn dry_account(c: &herdsman::Ctx, n: u64) -> Value {
    let (code, out) = run(c, &Fake::default(), &["spawn", "sendmeter", &n.to_string(), "impl", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str::<Value>(&out).unwrap()["account"].clone()
}

fn no_token_in_log(c: &herdsman::Ctx) {
    let log = std::fs::read_to_string(&c.state).unwrap_or_default();
    assert!(!log.contains(TOKEN_A) && !log.contains(TOKEN_B), "token in events: {log}");
}

#[test]
fn role_account_b_exports_its_token_into_the_lane_and_nowhere_else() {
    let d = tmpdir("acct-b");
    let c = ctx(&d, &config(&d, "account = \"B\""));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let r = spawn(&c, 12);
    // The lane shell runs the exported assignment: evaluate it as the pane shell would.
    let calls = r.calls.borrow();
    let export = calls.iter().find(|l| l.starts_with("herdr pane run w9:p12 export")).unwrap();
    assert!(!calls.iter().any(|l| l.contains(TOKEN_B)), "token passed through argv");
    let cmd = export.trim_start_matches("herdr pane run w9:p12 ");
    let sh = std::process::Command::new("sh").arg("-c").arg(format!("{cmd}; printf %s \"$CLAUDE_CODE_OAUTH_TOKEN\"")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&sh.stdout), TOKEN_B);
    assert!(r.index("herdr pane run w9:p12 export").unwrap() < r.index("herdr agent start").unwrap());
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.last().unwrap().str("account"), Some("B"));
    no_token_in_log(&c);

    // The binary's stdout and stderr (dry run, `accounts`) never show a token either.
    for args in [&["spawn", "sendmeter", "13", "impl", "--dry-run", "--json"][..], &["accounts"]] {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_herdsman"))
            .args(args)
            .env("FLEET_CONFIG", &c.config)
            .env("FLEET_STATE", &c.state)
            .env("FLEET_WORKTREES", &c.worktrees)
            .output()
            .unwrap();
        let (so, se) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert!(o.status.success(), "{args:?}: {so} {se}");
        assert!(so.contains('B'), "{so}");
        for t in [TOKEN_A, TOKEN_B] {
            assert!(!so.contains(t) && !se.contains(t), "token leaked: {so} {se}");
        }
    }
}

#[test]
fn capped_account_is_skipped_until_reset() {
    let d = tmpdir("acct-cap");
    let c = ctx(&d, &config(&d, "accounts = [\"A\", \"B\"]"));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    spawn(&c, 12);
    assert_eq!(read(&c.state).unwrap().last().unwrap().str("account"), Some("A"));

    let wt = c.worktrees.join("sendmeter/issue-12");
    let r = Fake::default()
        .reply("herdr agent list", &agents(&[("sendmeter-impl-12", "idle", wt.to_str().unwrap(), "w9:p12")]))
        .reply("herdr pane read w9:p12", "  ⎿  You've hit your limit · resets 3pm (Asia/Bangkok)\n");
    let (code, out) = run(&c, &r, &["check"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("account-capped: A:"), "{out}");
    let capped: Vec<_> = read(&c.state).unwrap().into_iter().filter(|e| e.kind == "account-capped").collect();
    assert_eq!((capped.len(), capped[0].str("account")), (1, Some("A")));
    assert_eq!(run(&c, &r, &["check"]).1, "", "fires once per lane");

    assert_eq!(dry_account(&c, 13), "B");
    let (_, list) = run(&c, &Fake::default(), &["accounts"]);
    assert!(list.lines().any(|l| l.starts_with("A ") && l.contains("capped ") && l.contains(" · resets ") && l.contains("1 lanes sendmeter-impl-12")), "{list}");
    assert!(list.lines().any(|l| l.starts_with("B ") && l.contains(" ok")), "{list}");

    let (code, out) = run(&c, &Fake::default(), &["accounts", "reset", "A"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(dry_account(&c, 14), "A");
    // The capped lane still shows the banner, but its cap was already reported: no re-cap.
    assert_eq!(run(&c, &r, &["check"]).1, "");
    assert_eq!(dry_account(&c, 14), "A");
    no_token_in_log(&c);
}

#[test]
fn cap_expires_after_cap_reset_minutes() {
    let d = tmpdir("acct-expire");
    let mut c = ctx(&d, &config(&d, "accounts = [\"A\", \"B\"]").replace("[accounts]\n", "[accounts]\ncap_reset_minutes = 10\n"));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    spawn(&c, 12);
    let wt = c.worktrees.join("sendmeter/issue-12");
    let r = Fake::default()
        .reply("herdr agent list", &agents(&[("sendmeter-impl-12", "idle", wt.to_str().unwrap(), "w9:p12")]))
        .reply("herdr pane read w9:p12", "Claude usage limit reached. Your limit will reset at 3pm.");
    assert!(run(&c, &r, &["check"]).1.contains("account-capped"));
    assert_eq!(dry_account(&c, 13), "B");
    c.now += 601;
    assert_eq!(dry_account(&c, 13), "A");
}

#[test]
fn mcp_allowlist_is_the_only_mcp_and_prod_is_refused() {
    let d = tmpdir("mcp");
    let c = ctx(&d, &config(&d, "mcp = [\"supabase-dev\"]"));
    let (code, out) = run(&c, &Fake::default(), &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    let p: Value = serde_json::from_str(&out).unwrap();
    let start: Vec<&str> = p["commands"][3].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    let file = c.worktrees.join("sendmeter/issue-12/.mcp.lane.json");
    let i = start.iter().position(|w| *w == "--mcp-config").unwrap();
    assert_eq!(start[i + 1..i + 3], [file.to_str().unwrap(), "--strict-mcp-config"]);

    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    spawn(&c, 12);
    let lane: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let names: Vec<&String> = lane["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["supabase-dev"]);
    assert_eq!(lane["mcpServers"]["supabase-dev"]["args"][0], "dev");

    // Default: no MCP at all, still strict.
    let d2 = tmpdir("mcp-none");
    let c2 = ctx(&d2, &config(&d2, ""));
    assert_eq!(run(&c2, &Fake::default(), &["profiles", "sync"]).0, 0);
    spawn(&c2, 12);
    let lane: Value =
        serde_json::from_str(&std::fs::read_to_string(c2.worktrees.join("sendmeter/issue-12/.mcp.lane.json")).unwrap()).unwrap();
    assert_eq!(lane["mcpServers"], serde_json::json!({}));

    for bad in ["supabase-prod", "Supabase-PROD-ro"] {
        let c = ctx(&d, &config(&d, &format!("mcp = [\"supabase-dev\", \"{bad}\"]")));
        let (code, out) = run(&c, &Fake::default(), &["spawn", "sendmeter", "13", "impl", "--dry-run"]);
        assert_eq!(code, 3, "{out}");
        assert!(out.starts_with("REFUSED MCP_FORBIDDEN"), "{out}");
        let (code, out) = run(&c, &Fake::default(), &["profiles", "sync"]);
        assert_eq!((code, out.contains("MCP_FORBIDDEN")), (3, true), "{out}");
    }
}

#[test]
fn hermes_profile_gets_only_the_listed_mcp_servers() {
    let d = tmpdir("mcp-hermes");
    let cfg = config(&d, "mcp = [\"supabase-dev\"]").replace("harness = \"claude\"\nmcp", "harness = \"hermes\"\nmcp");
    let c = ctx(&d, &cfg);
    let yaml = c.hermes.join("fleet-impl/config.yaml");
    std::fs::create_dir_all(yaml.parent().unwrap()).unwrap();
    std::fs::write(&yaml, "model:\n  default: opus\nmcp_servers:\n  supabase-prod:\n    command: npx\nagent:\n  max_turns: 90\n").unwrap();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let text = std::fs::read_to_string(&yaml).unwrap();
    assert!(text.contains("mcp_servers:\n  \"supabase-dev\": {\"args\":[\"dev\"],\"command\":\"npx\"}\n"), "{text}");
    assert!(!text.contains("prod") && text.contains("agent:\n  max_turns: 90"), "{text}");
}
