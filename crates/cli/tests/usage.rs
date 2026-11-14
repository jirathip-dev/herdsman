mod common;
use common::*;
use drover::events::{read, Event};
use drover::runner::{Out, Runner};
use drover::serve::Server;
use drover::usage::{epoch, lane_secs, parse_balance};
use serde_json::json;
use std::path::Path;

const DS_KEY: &str = concat!("sk", "-deepseekfixturekey0123456789");

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

fn stdin(f: Fake) -> Stdin {
    Stdin(f, Shared::default())
}

fn http(status: u32, head: &str, body: &str) -> String {
    format!("HTTP/2 {status}\r\ncontent-type: application/json\r\n{head}\r\n{body}")
}

fn ev(ts: u64, issue: Option<u64>, kind: &str, d: serde_json::Value) -> Event {
    Event::new(ts, "sendmeter", issue, kind, d)
}

fn spawn_ev(ts: u64, issue: u64, agent: &str, account: &str) -> Event {
    ev(
        ts,
        Some(issue),
        "spawn",
        json!({"agent": agent, "role": "impl", "account": account}),
    )
}

#[test]
fn lane_hours_clip_overlap_and_count_running_lanes() {
    let h = 3600;
    let now = 100 * h;
    let events = vec![
        spawn_ev(now - 30 * h, 1, "sendmeter-impl-1", "A"),
        ev(now - 20 * h, Some(1), "merged", json!({})),
        spawn_ev(now - 11 * h, 2, "sendmeter-impl-2", "A"),
        spawn_ev(now - 2 * h, 3, "sendmeter-impl-3", "A"),
        ev(
            now - h,
            Some(2),
            "lane-dead",
            json!({"key": format!("{}:sendmeter-impl-2", now - 11 * h)}),
        ),
        spawn_ev(now - 3 * h, 4, "sendmeter-impl-4", "B"),
        ev(
            now - 2 * h,
            Some(4),
            "supersede",
            json!({"agent": "sendmeter-impl-4", "killed": ["sendmeter-impl-4"]}),
        ),
        ev(now - 2 * h, Some(1), "clean", json!({})),
    ];
    assert_eq!(lane_secs(&events, "A", now, 5 * h), 4 * h + 2 * h);
    assert_eq!(lane_secs(&events, "A", now, 7 * 86400), 10 * h + 10 * h + 2 * h);
    assert_eq!(lane_secs(&events, "B", now, 5 * h), h);
    assert_eq!(lane_secs(&events, "A", now, 90 * h), 22 * h);
    let old = vec![spawn_ev(0, 9, "x", "C")];
    assert_eq!(
        lane_secs(&old, "C", 8 * 86400, 7 * 86400),
        7 * 86400,
        "a running lane is clipped to the window"
    );
}

#[test]
fn balance_and_usage_fixtures_parse() {
    let b = parse_balance(r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"2.28","granted_balance":"0.00","topped_up_balance":"2.28"}]}"#, 7).unwrap();
    assert_eq!((b.currency.as_str(), b.total, b.ts), ("USD", 2.28, 7));
    assert!(parse_balance("{}", 0).is_none());
    assert_eq!(epoch("2026-10-31T12:00:00.123+07:00"), Some(1_793_422_800));
    assert_eq!(epoch("2026-10-31T05:00:00Z"), Some(1_793_422_800));
}

#[test]
fn secret_in_config_is_refused() {
    const JWT: &str = concat!(
        "ey",
        "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJlZml4dHVyZQ"
    );
    for (i, secret) in [DS_KEY, JWT].into_iter().enumerate() {
        let c = ctx(
            &tmpdir(&format!("usage-secret-{i}")),
            &format!("{CONFIG}\n[providers.deepseek]\nkey_env = \"{secret}\"\n"),
        );
        let (code, out) = run(&c, &Fake::default(), &["accounts"]);
        assert_eq!(code, 3, "{out}");
        assert!(
            out.contains("SECRET_IN_CONFIG") && out.contains("providers.deepseek.key_env") && !out.contains(secret),
            "{out}"
        );
    }
    let legit = format!(
        "{CONFIG}\nmodels = [\"deepseek-ai/DeepSeek-V3.2-Exp-instruct-20261031-preview\"]\n[accounts]\nA = \"CLAUDE_CODE_OAUTH_TOKEN_ACCOUNT_A1_PERSONAL\"\n[providers.deepseek]\nkey_env = \"DEEPSEEK_API_KEY\"\nenv_file = \"/Users/someone/.config/claude-accounts/a1b2c3d4e5f6/tokens-for-personal-account.env\"\n"
    );
    let ok = ctx(&tmpdir("usage-nosecret"), &legit);
    let (code, out) = run(&ok, &Fake::default(), &["accounts"]);
    assert_eq!(code, 0, "{out}");
}

fn config(d: &Path) -> String {
    let env = d.join("tokens.env");
    std::fs::write(
        &env,
        format!("CLAUDE_CODE_OAUTH_TOKEN_A=a\nCLAUDE_CODE_OAUTH_TOKEN_B=b\nexport DEEPSEEK_API_KEY=\"{DS_KEY}\"\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
    CONFIG.replace("harness  =  \"hermes\"    # toggled in tests", "harness = \"claude\"\naccounts = [\"A\", \"B\"]")
        + &format!(
            "\n[accounts]\nA = \"CLAUDE_CODE_OAUTH_TOKEN_A\"\nB = \"CLAUDE_CODE_OAUTH_TOKEN_B\"\n\n[accounts.usage]\nA = \"{}\"\n\n[providers.deepseek]\nkey_env = \"DEEPSEEK_API_KEY\"\nenv_file = \"{}\"\n",
            d.join("logins/A").display(),
            env.display()
        )
}

fn no_secret(what: &str, text: &str) {
    assert!(!text.contains(DS_KEY), "{what} leaks a secret: {text}");
}

fn assert_no_leaks(c: &drover::Ctx, r: &Stdin, outs: &[&str], page: &str) {
    let calls = r.0.calls.borrow().join("\n");
    no_secret("argv", &calls);
    assert!(!calls.contains("security "), "{calls}");
    for o in outs {
        no_secret("CLI output", o);
    }
    no_secret("event log", &drover::events::jsonl(&c.state).unwrap_or_default());
    for f in std::fs::read_dir(c.state.parent().unwrap().join("usage"))
        .into_iter()
        .flatten()
    {
        no_secret("cache", &std::fs::read_to_string(f.unwrap().path()).unwrap_or_default());
    }
    no_secret("page", page);
}

#[test]
fn meter_balance_and_lane_hours_show_without_leaking_secrets() {
    let d = tmpdir("usage-show");
    let c = ctx(&d, &config(&d));
    row(&c, "A", c.now, 9.0, 50.0);
    let r = stdin(
        Fake::default()
            .reply(
                "curl -sS -m 15 -D - -K - https://api.deepseek.com/user/balance",
                &http(
                    200,
                    "",
                    r#"{"balance_infos":[{"currency":"USD","total_balance":"2.28"}]}"#,
                ),
            )
            .reply("date -r", "Mon 19\n"),
    );
    let (code, out) = run(&c, &r, &["accounts"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("session 9% (resets 60m) · week 50% (resets Mon 19)"),
        "{out}"
    );
    assert!(out.contains("5h 0.0 · 7d 0.0 drover lane-hours"), "{out}");
    assert!(out.contains("deepseek · USD 2.28 · low"), "{out}");
    let sent = r.1.borrow().join("\n");
    assert!(
        sent.contains(&format!("Bearer {DS_KEY}")) && !r.0.called("curl -sS -m 15 -D - -K - https://api.anthropic.com"),
        "keys go on stdin: {sent}"
    );

    let n = r.0.count("curl");
    let page = host_tab(&Server::new(&c, &r, None), "", c.now);
    assert_eq!(r.0.count("curl"), n);
    assert!(
        page.contains("▏</span> <b class=g>50%</b></span>") && page.contains("drover lane-hours"),
        "{page}"
    );
    assert!(page.contains("<span class=\"pvs a\">✓ USD 2.28 · low</span>"), "{page}");
    assert_no_leaks(&c, &r, &[&out], &page);

    let (_, out) = run(&c, &r, &["check"]);
    assert!(
        out.contains("deepseek provider-low: balance USD 2.28 (warn below 5)"),
        "{out}"
    );
    assert_eq!(run(&c, &r, &["check"]).1, "");
    assert_eq!(
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "provider-low")
            .count(),
        1
    );
}

#[test]
fn deepseek_hints_when_the_env_file_lacks_the_key() {
    let d = tmpdir("usage-nokey");
    let missing = d.join("nope.env");
    let c = ctx(
        &d,
        &format!(
            "{CONFIG}\n[providers.deepseek]\nkey_env = \"DEEPSEEK_API_KEY\"\nenv_file = \"{}\"\n",
            missing.display()
        ),
    );
    let (code, out) = run(&c, &Fake::default(), &["accounts"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("deepseek · key not found in {}", missing.display())),
        "{out}"
    );
    let page = host_tab(&Server::new(&c, &Fake::default(), None), "", c.now);
    assert!(
        page.contains(&format!(
            "<article class=pvc data-p=deepseek><div class=pvch><b>DeepSeek</b><span class=m>API keys</span></div><div class=pvr data-t=a><p class=pvh><b class=pvn>balance</b><span class=\"pvs a\">key not found in {}</span>",
            missing.display()
        )),
        "{page}"
    );
}

#[test]
fn a_deepseek_env_file_others_can_read_is_refused() {
    let d = tmpdir("usage-0644");
    let c = ctx(&d, &config(&d));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(d.join("tokens.env"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let r = stdin(Fake::default());
    let (code, out) = run(&c, &r, &["accounts"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("deepseek · REFUSED SECRET_FILE_OPEN: "), "{out}");
    assert!(!r.0.called("curl"), "{:?}", r.0.calls.borrow());
    no_secret("CLI output", &out);
}

#[test]
fn spawn_prefers_the_lowest_fresh_weekly_usage() {
    let d = tmpdir("usage-pick");
    let c = ctx(&d, &config(&d));
    let _sh = FakeShed::attached(&c);
    let dry = || {
        let (code, out) = run(
            &c,
            &Fake::default().reply("which shed", "/opt/shed/bin/shed\n"),
            &["spawn", "sendmeter", "7", "impl", "--dry-run", "--json"],
        );
        assert_eq!(code, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["account"].clone()
    };
    let rows = d.join("state/usage.jsonl");
    row(&c, "A", c.now - 60, 0.0, 80.0);
    row(&c, "B", c.now - 60, 0.0, 20.0);
    assert_eq!(dry(), "B");
    std::fs::remove_file(&rows).unwrap();
    row(&c, "A", c.now - 60, 0.0, 80.0);
    row(&c, "B", c.now - 1900, 0.0, 20.0);
    assert_eq!(dry(), "A", "B's reading is over 30 min old: configured order");
}

fn status(five: f64, week: f64, now: u64) -> String {
    json!({"model": {"display_name": "Opus"}, "rate_limits": {
        "five_hour": {"used_percentage": five, "resets_at": now + 3600},
        "seven_day": {"used_percentage": week, "resets_at": now + 86400}}})
    .to_string()
}

fn row(c: &drover::Ctx, account: &str, ts: u64, five: f64, week: f64) {
    let mut c = c.clone();
    c.now = ts;
    drover::usage::statusline(&c, account, "", &status(five, week, ts), &mut Vec::new());
}

#[test]
fn statusline_appends_a_throttled_row_and_shrugs_off_garbage() {
    let d = tmpdir("usage-statusline");
    let mut c = ctx(&d, &config(&d));
    let log = d.join("state/usage.jsonl");
    let line = |c: &drover::Ctx, account: &str, input: &str| {
        let mut out = Vec::new();
        drover::usage::statusline(c, account, "synergy:2940:impl", input, &mut out);
        String::from_utf8(out).unwrap()
    };
    let rows = || std::fs::read_to_string(&log).unwrap_or_default().lines().count();
    assert_eq!(
        line(&c, "A", &status(41.2, 63.0, c.now)),
        "synergy #2940 · 5h 41% · 7d 63%\n"
    );
    let got: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&log).unwrap().trim()).unwrap();
    assert_eq!(
        got,
        json!({"ts": c.now, "account": "A", "five_hour_pct": 41.2, "five_hour_reset": c.now + 3600,
            "seven_day_pct": 63.0, "seven_day_reset": c.now + 86400})
    );
    line(&c, "A", &status(50.0, 70.0, c.now));
    assert_eq!(rows(), 1, "one row per account per minute");
    line(&c, "B", &status(5.0, 7.0, c.now));
    c.now += 60;
    line(&c, "A", &status(50.0, 70.0, c.now));
    assert_eq!(rows(), 3);
    for junk in ["not json", "{}", r#"{"rate_limits": 7}"#, ""] {
        assert_eq!(line(&c, "C", junk), "synergy #2940\n");
    }
    line(&c, "", &status(1.0, 1.0, c.now));
    assert_eq!(rows(), 3, "garbage, or no account: no row");
}

#[test]
fn meter_is_the_newest_status_line_row_fresh_or_stale() {
    let d = tmpdir("usage-rows");
    let c = ctx(&d, &config(&d));
    let accounts = || {
        let (code, out) = run(&c, &Fake::default().reply("date -r", "Mon 19\n"), &["accounts"]);
        assert_eq!(code, 0, "{out}");
        out
    };
    row(&c, "A", c.now - 60, 41.0, 63.0);
    row(&c, "B", c.now - 3600, 12.0, 30.0);
    let out = accounts();
    assert!(
        out.contains("session 41% (resets 59m, read 60s ago) · week 63%"),
        "{out}"
    );
    assert!(out.contains("session 0% (reset, read 60m ago) · week 30%"), "{out}");
    std::fs::remove_file(d.join("state/usage.jsonl")).unwrap();
    row(&c, "A", c.now - 700, 41.0, 63.0);
    let out = accounts();
    assert!(out.contains("read 11m ago) · week 63%"), "{out}");
}

#[test]
fn headroom_picking_takes_the_most_headroom() {
    let d = tmpdir("usage-headroom");
    let order = config(&d);
    let dry = |cfg: &str| {
        let c = ctx(&d, cfg);
        let _sh = FakeShed::attached(&c);
        let (code, out) = run(
            &c,
            &Fake::default().reply("which shed", "/opt/shed/bin/shed\n"),
            &["spawn", "sendmeter", "7", "impl", "--dry-run", "--json"],
        );
        assert_eq!(code, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["account"].clone()
    };
    let headroom = order.replace("[accounts]\n", "[accounts]\npick = \"headroom\"\n");
    let c = ctx(&d, &order);
    row(&c, "A", c.now, 90.0, 10.0);
    row(&c, "B", c.now, 20.0, 30.0);
    assert_eq!(dry(&order), "A", "default `order`: unchanged");
    assert_eq!(dry(&headroom), "B");
    std::fs::remove_file(d.join("state/usage.jsonl")).unwrap();
    row(&c, "A", c.now - 4000, 90.0, 10.0);
    row(&c, "B", c.now, 20.0, 30.0);
    assert_eq!(dry(&headroom), "A");
}

#[test]
fn a_reading_past_its_reset_reads_as_reset_with_its_age() {
    let d = tmpdir("usage-785-reset");
    let c = ctx(&d, &config(&d));
    row(&c, "A", c.now - 4 * 86400, 88.0, 88.0);
    let (code, out) = run(&c, &Fake::default().reply("date -r", "Mon 19\n"), &["accounts"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("session 0% (reset, read 4d ago) · week 0% (reset)"),
        "{out}"
    );
    assert!(out.contains("ok: 5h 0%, 7d 0%"), "{out}");
    assert!(!out.contains("88%"), "{out}");
}
