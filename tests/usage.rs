//! #31: herdsman lane-hours, the Anthropic usage meter, the DeepSeek balance, SECRET_IN_CONFIG.
mod common;
use common::*;
use herdsman::events::{read, Event};
use herdsman::runner::{Out, Runner};
use herdsman::serve::Server;
use herdsman::usage::{epoch, lane_secs, parse_balance, parse_usage};
use serde_json::json;
use std::path::Path;

const DS_KEY: &str = "sk-deepseekfixturekey0123456789";
const ACCESS: &str = "sk-ant-oat01-ACCESS-fixture-secret";
const REFRESH: &str = "sk-ant-ort01-REFRESH-fixture-secret";
const NEW_ACCESS: &str = "sk-ant-oat01-NEWACCESS-fixture-secret";

/// `Fake` that also records what each `run_in` sent on stdin.
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
        // Ran 10 h, ended 20 h ago: outside 5 h, inside 7 d.
        spawn_ev(now - 30 * h, 1, "sendmeter-impl-1", "A"),
        ev(now - 20 * h, Some(1), "merged", json!({})),
        // Overlapping lanes on A: 6 h before the 5 h window opened until 1 h ago (clipped to 4 h), and one still running (2 h).
        spawn_ev(now - 11 * h, 2, "sendmeter-impl-2", "A"),
        spawn_ev(now - 2 * h, 3, "sendmeter-impl-3", "A"),
        ev(
            now - h,
            Some(2),
            "lane-dead",
            json!({"key": format!("{}:sendmeter-impl-2", now - 11 * h)}),
        ),
        // B's lane is superseded after 1 h; a spawn starting before the 7 d window is clipped to it.
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
    let (s, w) = parse_usage(r#"{"five_hour":{"utilization":9.0,"resets_at":"2026-10-31T05:00:00+00:00"},"seven_day":{"utilization":22.0,"resets_at":null}}"#).unwrap();
    assert_eq!((s, w), (Some((9.0, 1_793_422_800)), None));
}

#[test]
fn secret_in_config_is_refused() {
    const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.c2lnbmF0dXJlZml4dHVyZQ";
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
    // Long env-var names, paths and model ids are not secrets.
    let legit = format!(
        "{CONFIG}\nmodels = [\"deepseek-ai/DeepSeek-V3.2-Exp-instruct-20261031-preview\"]\n[accounts]\naccounts_env = \"/Users/someone/.config/claude-accounts/a1b2c3d4e5f6/tokens-for-personal-account.env\"\nA = \"CLAUDE_CODE_OAUTH_TOKEN_ACCOUNT_A1_PERSONAL\"\n[providers.deepseek]\nkey_env = \"DEEPSEEK_API_KEY\"\n"
    );
    let ok = ctx(&tmpdir("usage-nosecret"), &legit);
    let (code, out) = run(&ok, &Fake::default(), &["accounts"]);
    assert_eq!(code, 0, "{out}");
}

/// A config with accounts A/B (lane tokens in `tokens.env`, which also holds the DeepSeek key) and A's usage login.
fn config(d: &Path) -> String {
    let env = d.join("tokens.env");
    std::fs::write(
        &env,
        format!("CLAUDE_CODE_OAUTH_TOKEN_A=a\nCLAUDE_CODE_OAUTH_TOKEN_B=b\nexport DEEPSEEK_API_KEY=\"{DS_KEY}\"\n"),
    )
    .unwrap();
    CONFIG.replace("harness  =  \"hermes\"    # toggled in tests", "harness = \"claude\"\naccounts = [\"A\", \"B\"]")
        + &format!(
            "\n[accounts]\naccounts_env = \"{}\"\nA = \"CLAUDE_CODE_OAUTH_TOKEN_A\"\nB = \"CLAUDE_CODE_OAUTH_TOKEN_B\"\n\n[accounts.usage]\nA = \"{}\"\n\n[providers.deepseek]\nkey_env = \"DEEPSEEK_API_KEY\"\n",
            env.display(),
            d.join("logins/A").display()
        )
}

fn creds(expires_ms: u64) -> String {
    json!({"claudeAiOauth": {"accessToken": ACCESS, "refreshToken": REFRESH, "expiresAt": expires_ms, "scopes": ["user:profile"]}}).to_string()
}

/// `security`'s attribute dump of the login's item (account `owner`).
const ATTRS: &str = "keychain: \"/Users/owner/Library/Keychains/login.keychain-db\"\nattributes:\n    \"acct\"<blob>=\"owner\"\n    \"svce\"<blob>=\"x\"\n";

fn no_secret(what: &str, text: &str) {
    for s in [DS_KEY, ACCESS, REFRESH, NEW_ACCESS] {
        assert!(!text.contains(s), "{what} leaks a secret: {text}");
    }
}

/// Every secret went to stdin only: never argv, CLI output, the event log, a cache file or the page. Every
/// Keychain call names the configured login's hashed item, never the default `Claude Code-credentials`.
fn assert_no_leaks(c: &herdsman::Ctx, r: &Stdin, outs: &[&str], page: &str) {
    let calls = r.0.calls.borrow().join("\n");
    no_secret("argv", &calls);
    let svc = herdsman::usage::keychain_service(&c.config.parent().unwrap().parent().unwrap().join("logins/A"));
    for call in r.0.calls.borrow().iter().filter(|l| l.starts_with("security find")) {
        assert!(call.ends_with(&format!("-s {svc}")), "{call}");
    }
    for script in r.1.borrow().iter().filter(|s| s.contains("generic-password")) {
        assert!(script.contains(&format!("-s \"{svc}\"")), "{script}");
    }
    for o in outs {
        no_secret("CLI output", o);
    }
    no_secret("event log", &std::fs::read_to_string(&c.state).unwrap_or_default());
    for f in std::fs::read_dir(c.state.parent().unwrap().join("usage")).unwrap() {
        no_secret("cache", &std::fs::read_to_string(f.unwrap().path()).unwrap_or_default());
    }
    no_secret("page", page);
}

const USAGE: &str = r#"{"five_hour":{"utilization":9.0,"resets_at":"1970-01-12T13:56:40+00:00"},"seven_day":{"utilization":50.0,"resets_at":"1970-01-17T13:46:40+00:00"}}"#;

#[test]
fn meter_balance_and_lane_hours_show_without_leaking_secrets() {
    let d = tmpdir("usage-show");
    let c = ctx(&d, &config(&d));
    let r = stdin(
        Fake::default()
            .reply("security find-generic-password -s", ATTRS)
            .reply("security find-generic-password -w", &creds(2_000_000_000_000))
            .reply(
                "curl -sS -m 15 -D - -K - https://api.anthropic.com/api/oauth/usage",
                &http(200, "", USAGE),
            )
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
        out.contains("session 9% (resets 10m) · week 50% (resets Mon 19) · at this pace: out Mon evening"),
        "{out}"
    );
    assert!(out.contains("5h 0.0 · 7d 0.0 herdsman lane-hours"), "{out}");
    assert!(out.contains("deepseek · USD 2.28 · low"), "{out}");
    let sent = r.1.borrow().join("\n");
    assert!(
        sent.contains(&format!("Bearer {DS_KEY}")) && sent.contains(&format!("Bearer {ACCESS}")),
        "keys go on stdin: {sent}"
    );

    // Cached for 10 min: the page re-reads nothing, and shows the bars.
    let n = r.0.count("curl");
    let page = String::from_utf8_lossy(&Server::new(&c, &r, None).respond("GET / HTTP/1.1\r\nHost: x\r\n\r\n", c.now))
        .into_owned();
    assert_eq!(r.0.count("curl"), n);
    assert!(
        page.contains("<meter min=0 max=100 low=75 high=89.9 optimum=0 value=50>")
            && page.contains("herdsman lane-hours"),
        "{page}"
    );
    assert!(page.contains("deepseek · USD 2.28 · <b class=a>low</b>"), "{page}");
    assert_no_leaks(&c, &r, &[&out], &page);

    // `check` alerts once per crossing below warn_below.
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
fn rate_limited_meter_backs_off_past_retry_after() {
    let d = tmpdir("usage-429");
    let mut c = ctx(&d, &config(&d));
    let r = stdin(
        Fake::default()
            .reply("security find-generic-password -s", ATTRS)
            .reply("security find-generic-password -w", &creds(2_000_000_000_000))
            .reply(
                "curl -sS -m 15 -D - -K - https://api.anthropic.com",
                &http(429, "retry-after: 3000\r\n", "{}"),
            ),
    );
    let m = herdsman::usage::meter(&c, &herdsman::config::Config::load(&c.config).unwrap(), &r, "A").unwrap();
    assert_eq!((m.fails, m.retry_at), (1, c.now + 3000));
    c.now += 2000;
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    herdsman::usage::meter(&c, &cfg, &r, "A");
    assert_eq!(r.0.count("curl"), 1, "no poll before retry-after");
    c.now += 1001;
    let m = herdsman::usage::meter(&c, &cfg, &r, "A").unwrap();
    assert_eq!(
        (r.0.count("curl"), m.fails, m.retry_at),
        (2, 2, c.now + 3000),
        "the larger of retry-after and 600 << fails: {m:?}"
    );
}

#[test]
fn expired_login_refreshes_or_falls_back_to_login_needed() {
    let d = tmpdir("usage-expired");
    let c = ctx(&d, &config(&d));
    let r = stdin(
        Fake::default()
            .reply("security find-generic-password -s", ATTRS)
            .reply("security find-generic-password -w", &creds(1000))
            .reply(
                "curl -sS -m 15 -D - -K - https://platform.claude.com",
                &http(400, "", r#"{"error":"invalid_grant"}"#),
            ),
    );
    let (code, out) = run(&c, &r, &["accounts"]);
    assert_eq!(code, 0, "{out}");
    let dir = d.join("logins/A");
    assert!(
        out.contains(&format!("usage: login needed (claude /login in {})", dir.display()))
            && out.contains("herdsman lane-hours"),
        "{out}"
    );
    assert!(!r.0.called("security -i"), "a failed refresh writes nothing");
    let page = String::from_utf8_lossy(&Server::new(&c, &r, None).respond("GET / HTTP/1.1\r\nHost: x\r\n\r\n", c.now))
        .into_owned();
    assert!(page.contains("usage: login needed"), "{page}");
    assert_no_leaks(&c, &r, &[&out], &page);
}

/// An expired login whose refresh succeeds; `stored` is what the Keychain holds when re-read after the write-back.
fn refreshed(d: &Path, stored: &str) -> (herdsman::Ctx, Stdin) {
    let c = ctx(d, &config(d));
    let rotated =
        json!({"access_token": NEW_ACCESS, "refresh_token": "sk-ant-ort01-NEWREFRESH-fixture", "expires_in": 3600});
    let r = stdin(
        Fake::default()
            .reply("security find-generic-password -s", ATTRS)
            .once("security find-generic-password -w", 0, &creds(1000))
            .reply("security find-generic-password -w", stored)
            .reply(
                "curl -sS -m 15 -D - -K - https://platform.claude.com",
                &http(200, "", &rotated.to_string()),
            )
            .reply("security -i", "")
            .reply(
                "curl -sS -m 15 -D - -K - https://api.anthropic.com",
                &http(200, "", USAGE),
            ),
    );
    (c, r)
}

#[test]
fn a_refresh_is_written_back_to_the_same_item_and_verified() {
    let d = tmpdir("usage-refresh");
    let stored =
        json!({"claudeAiOauth": {"accessToken": NEW_ACCESS, "refreshToken": "x", "expiresAt": 2_000_000_000_000u64}})
            .to_string();
    let (c, r) = refreshed(&d, &stored);
    let (_, out) = run(&c, &r, &["accounts"]);
    assert!(out.contains("session 9%"), "{out}");
    let sent = r.1.borrow().join("\n");
    assert!(sent.contains(&format!("Bearer {NEW_ACCESS}")), "{sent}");
    let svc = herdsman::usage::keychain_service(&d.join("logins/A"));
    assert!(
        sent.contains(&format!("add-generic-password -U -a \"owner\" -s \"{svc}\" -X ")),
        "the item's own account: {sent}"
    );
    assert!(r
        .0
        .called(&format!("security find-generic-password -w -a owner -s {svc}")));
    assert_eq!(
        r.0.count("security find-generic-password -w"),
        2,
        "re-read after the write-back"
    );
    assert!(!read(&c.state).unwrap().iter().any(|e| e.kind == "usage-login-lost"));
    assert_no_leaks(&c, &r, &[&out], "");
}

#[test]
fn a_write_back_that_does_not_stick_alerts_usage_login_lost() {
    let d = tmpdir("usage-lost");
    // `security -i` exits 0, but the item still holds the old login.
    let (c, r) = refreshed(&d, &creds(1000));
    let (code, out) = run(&c, &r, &["accounts"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("usage: login needed"), "{out}");
    let lost: Vec<Event> = read(&c.state)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "usage-login-lost")
        .collect();
    assert_eq!(lost.len(), 1);
    let why = lost[0].str("reason").unwrap();
    assert!(
        lost[0].str("account") == Some("A")
            && why.contains(&format!("claude /login in {}", d.join("logins/A").display())),
        "{why}"
    );
    assert!(
        !r.0.called("curl -sS -m 15 -D - -K - https://api.anthropic.com"),
        "the unstored token is not used"
    );
    assert_no_leaks(&c, &r, &[&out], "");
}

#[test]
fn an_unsafe_keychain_account_is_never_refreshed() {
    let d = tmpdir("usage-unsafe");
    let c = ctx(&d, &config(&d));
    let r = stdin(
        Fake::default()
            .reply(
                "security find-generic-password -s",
                "    \"acct\"<blob>=\"own\\\"er\"\n",
            )
            .reply("security find-generic-password -w", &creds(1000)),
    );
    assert!(run(&c, &r, &["accounts"]).1.contains("usage: login needed"));
    assert!(
        !r.0.called("curl -sS -m 15 -D - -K - https://platform.claude.com") && !r.0.called("security -i"),
        "the old login stays usable by Claude Code"
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
    let page = String::from_utf8_lossy(
        &Server::new(&c, &Fake::default(), None).respond("GET / HTTP/1.1\r\nHost: x\r\n\r\n", c.now),
    )
    .into_owned();
    assert!(
        page.contains(&format!(
            "<p class=m>deepseek · key not found in {}</p>",
            missing.display()
        )),
        "{page}"
    );
}

#[test]
fn keychain_service_is_the_hashed_config_dir() {
    // Our own derivation (from Claude Code 2.1.308, unverified against a live Keychain): this only pins its shape.
    let s = herdsman::usage::keychain_service(Path::new("/tmp/claude-accounts/A"));
    let want = &herdsman::devices::sha256(b"/tmp/claude-accounts/A")[..8];
    assert_eq!(s, format!("Claude Code-credentials-{want}"));
}

#[test]
fn spawn_prefers_the_lowest_fresh_weekly_usage() {
    let d = tmpdir("usage-pick");
    let c = ctx(&d, &config(&d));
    let usage = d.join("state/usage");
    std::fs::create_dir_all(&usage).unwrap();
    let dry = || {
        let (code, out) = run(
            &c,
            &Fake::default(),
            &["spawn", "sendmeter", "7", "impl", "--dry-run", "--json"],
        );
        assert_eq!(code, 0, "{out}");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["account"].clone()
    };
    let meter = |name: &str, ts: u64, week: f64| {
        std::fs::write(
            usage.join(format!("{name}.json")),
            json!({"checked": ts, "ts": ts, "week": [week, c.now + 86400]}).to_string(),
        )
        .unwrap();
    };
    meter("A", c.now - 60, 80.0);
    meter("B", c.now - 60, 20.0);
    assert_eq!(dry(), "B");
    meter("B", c.now - 1900, 20.0);
    assert_eq!(dry(), "A", "B's reading is over 30 min old: configured order");
}
