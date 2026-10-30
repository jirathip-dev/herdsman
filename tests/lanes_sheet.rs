//! #30: Lanes like Corral: state-first grouping, the bottom sheet's transcript, the paired
//! reply box (pairing, refusals, `prompt` events without text), vector/ASCII horses, no dashes.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

fn two() -> String {
    format!("{CONFIG}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\n")
}

fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str, now: u64) -> String {
    let r = format!("{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    String::from_utf8_lossy(&s.respond(&r, now)).into_owned()
}

/// Agents in every state across sendmeter, synergy and Others; sendmeter-impl-14 is dead (log only).
fn fixture(name: &str) -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir(name), &two());
    let spawn = |fleet: &str, issue: u64, ts: u64, role: &str, agent: &str| {
        let d = json!({"role": role, "agent": agent, "harness": if role == "rev" { "hermes" } else { "claude" }});
        append(&c.state, &Event::new(ts, fleet, Some(issue), "spawn", d)).unwrap();
    };
    spawn("sendmeter", 12, 900_100, "impl", "sendmeter-impl-12");
    spawn("sendmeter", 9, 900_200, "rev", "sendmeter-rev-9-r1");
    spawn("sendmeter", 14, 900_300, "impl", "sendmeter-impl-14");
    spawn("synergy", 7, 900_400, "impl", "synergy-impl-7");
    spawn("synergy", 8, 900_500, "impl", "synergy-impl-8");
    spawn("synergy", 5, 900_600, "design", "synergy-design-5");
    spawn("synergy", 20, 900_700, "impl", "synergy-impl-20");
    let a = |n: &str, st: &str, p: &str| json!({"name": n, "agent_status": st, "pane_id": p, "cwd": "/x"});
    let list = json!({"result": {"agents": [
        a("sendmeter-impl-12", "working", "w1:p2"),
        a("sendmeter-rev-9-r1", "blocked", "w1:p3"),
        a("synergy-impl-7", "working", "w2:p1"),
        a("synergy-impl-8", "working", "w2:p2"),
        a("synergy-design-5", "done", "w2:p3"),
        a("synergy-impl-20", "unknown", "w2:p4"),
        {"agent": "codex", "agent_status": "idle", "pane_id": "w4:p1", "cwd": "/home/me/side", "workspace_id": "w4"},
        {"agent": "claude", "agent_status": "working", "pane_id": "w5:p1", "cwd": "/home/me/scratch", "workspace_id": "w5"},
    ]}});
    let ws = json!({"result": {"workspaces": [{"workspace_id": "w4", "label": "side-project"}, {"workspace_id": "w5", "label": "scratch"}]}});
    let screen = "⏺ Read(src/serve.rs)\n  ⎿ Read 120 lines\n> fix the test\n⏺ Bash(cargo test)\n  ⎿ ok\n";
    let r = Fake::default()
        .reply("herdr agent list", &list.to_string())
        .reply("herdr workspace list", &ws.to_string())
        .reply("herdr pane read", screen)
        .reply("herdr agent prompt", "")
        .reply("herdr pane send", "");
    (c, r)
}

fn section<'a>(page: &'a str, id: &str) -> &'a str {
    let a = page.find(&format!("<section id={id}>")).unwrap();
    &page[a..a + page[a..].find("</section>").unwrap()]
}

#[test]
fn lanes_group_by_state_then_repo_with_counts() {
    let (c, r) = fixture("group");
    let page = req(&Server::new(&c, &r, None), "GET", "/", "", "", T);
    let lanes = section(&page, "lanes");
    let mut got = vec![];
    for part in lanes.split("<h").skip(1) {
        let text = &part[..part.find("</h").unwrap()];
        let text = text.trim_end_matches("</span>").rsplit('>').next().unwrap();
        got.push(if part.starts_with('2') { text.to_string() } else { format!("  {text}") });
        let mut rows = part.split("data-agent=\"").skip(1).map(|r| r[..r.find('"').unwrap()].to_string());
        got.extend(rows.by_ref().map(|a| format!("    {a}")));
    }
    let want = [
        "blocked (1)", "  sendmeter (1)", "    sendmeter-rev-9-r1",
        "working (4)", "  sendmeter (1)", "    sendmeter-impl-12", "  synergy (2)", "    synergy-impl-8", "    synergy-impl-7", "  Others (1)", "    scratch",
        "idle (2)", "  synergy (1)", "    synergy-impl-20", "  Others (1)", "    side-project",
        "done (1)", "  synergy (1)", "    synergy-design-5",
        "dead (1)", "  sendmeter (1)", "    sendmeter-impl-14",
    ];
    assert_eq!(got, want);
    // Rows open the sheet: a button plus the hidden details card, no inline <details>.
    assert!(!lanes.contains("<details") && lanes.contains("<button type=button class=lr>") && lanes.contains("<div class=card hidden>"));
    assert!(lanes.contains("<p>impl · round 0 · claude · "), "{lanes}");
}

#[test]
fn sheet_transcript_is_the_masked_pane_tail() {
    let (c, r) = fixture("tail");
    let s = Server::new(&c, &r, None);
    let res = req(&s, "GET", "/lane?agent=sendmeter-impl-12", "", "", T);
    assert!(res.starts_with("HTTP/1.1 200 OK") && res.contains("application/json"), "{res}");
    let v: serde_json::Value = serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["lines"].as_array().unwrap().len(), 5);
    assert!(r.called("herdr pane read w1:p2 --source recent --lines 80"));
    assert!(req(&s, "GET", "/lane?agent=nobody", "", "", T).starts_with("HTTP/1.1 404 "));
}

/// The pairing code `herdsman serve pair` printed.
fn code(c: &herdsman::Ctx) -> String {
    let (rc, out) = run(c, &Fake::default(), &["serve", "pair"]);
    assert_eq!(rc, 0, "{out}");
    let code: String = out.chars().filter(char::is_ascii_digit).take(6).collect();
    assert!(out.starts_with(&format!("pairing code {code} (5 min")), "{out}");
    code
}

/// Pair at `now` and return the `Cookie:` header line.
fn paired(s: &Server, c: &herdsman::Ctx) -> String {
    let res = req(s, "POST", "/pair", &format!("code={}", code(c)), "User-Agent: Mozilla/5.0 (iPhone)\r\n", T);
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    let set = res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap();
    assert!(set.contains("; HttpOnly; Secure; SameSite=Strict"), "{set}");
    let tok = &set[3..set.find(';').unwrap()];
    assert_eq!(tok.len(), 64, "32 random bytes, hex");
    format!("Cookie: hd={tok}\r\n")
}

#[test]
fn pairing_codes_devices_and_prompt_events() {
    let (c, r) = fixture("pair");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let send = |cookie: &str, text: &str| req(&s, "POST", "/send", &format!("agent=sendmeter-impl-12&text={text}"), cookie, T + 10);
    // No cookie: 401, nothing sent.
    let res = send("", "hi");
    assert!(res.starts_with("HTTP/1.1 401 ") && res.contains("herdsman serve pair"), "{res}");
    // A wrong code is refused (and burns the code).
    let good = code(&c);
    let wrong = if good == "000000" { "111111" } else { "000000" };
    assert!(req(&s, "POST", "/pair", &format!("code={wrong}"), "", T).starts_with("HTTP/1.1 403 "));
    assert!(req(&s, "POST", "/pair", &format!("code={good}"), "", T).starts_with("HTTP/1.1 403 "), "one attempt per code");
    // An expired code is refused.
    let late = code(&c);
    assert!(req(&s, "POST", "/pair", &format!("code={late}"), "", T + 301).starts_with("HTTP/1.1 403 "));
    // A foreign Origin never reaches pairing or sending.
    assert!(req(&s, "POST", "/pair", "code=1", "Origin: https://evil.example\r\n", T).starts_with("HTTP/1.1 403 "));
    // A valid code sets the cookie; only the token's hash is stored, 0600.
    let cookie = paired(&s, &c);
    let file = c.state.parent().unwrap().join("serve/devices.json");
    let stored = std::fs::read_to_string(&file).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    let tok = &cookie[11..cookie.len() - 2];
    assert!(!stored.contains(tok) && stored.contains(&herdsman::devices::sha256(tok.as_bytes())), "{stored}");
    // The next send succeeds: prompt + Enter, and a `prompt` event without the text.
    let res = send(&cookie, "hello+secret+words");
    assert!(res.starts_with("HTTP/1.1 200 OK") && res.ends_with("sent to sendmeter-impl-12\n"), "{res}");
    assert!(r.called("herdr agent prompt sendmeter-impl-12 hello secret words"));
    assert!(r.index("herdr agent prompt") < r.index("herdr pane send-keys w1:p2 Enter"));
    let e = read(&c.state).unwrap().pop().unwrap();
    let label = herdsman::devices::load(&c)[0].label.clone();
    assert!(label.starts_with("iPhone "), "{label}");
    assert_eq!((e.kind.as_str(), e.fleet.as_str(), e.issue), ("prompt", "sendmeter", Some(12)));
    assert_eq!(serde_json::Value::Object(e.data), json!({"via": "web", "agent": "sendmeter-impl-12", "device": label, "chars": 18}));
    assert!(!std::fs::read_to_string(&c.state).unwrap().contains("secret"), "no message text in the log");
    // The Host tab lists the device; Remove unpairs it and its next send is 401.
    let page = req(&s, "GET", "/", "", "", T);
    let host = section(&page, "host");
    assert!(host.contains(&format!("<span>{label} <span class=m>paired ")) && host.contains("<button>Remove</button>"), "{host}");
    let mt = &page[page.find("name=mtime value=").unwrap() + 17..];
    let mt: String = mt.chars().take_while(char::is_ascii_digit).collect();
    let hash = herdsman::devices::sha256(tok.as_bytes());
    let res = req(&s, "POST", "/", &format!("op=unpair&fleet=&hash={hash}&mtime={mt}"), "", T);
    assert!(res.starts_with("HTTP/1.1 303 ") && res.contains("device%20removed"), "{res}");
    assert!(herdsman::devices::load(&c).is_empty());
    assert!(send(&cookie, "again").starts_with("HTTP/1.1 401 "));
}

#[test]
fn reply_refusals_rate_limit_and_read_only() {
    let (c, r) = fixture("refuse");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = paired(&s, &c);
    let send = |agent: &str, text: &str| req(&s, "POST", "/send", &format!("agent={agent}&text={text}"), &cookie, T);
    let body = |res: String| res[res.find("\r\n\r\n").unwrap() + 4..].trim().to_string();
    assert!(body(send("sendmeter-rev-9-r1", "stop")).starts_with("REFUSED REV_READONLY: "));
    assert!(body(send("sendmeter-impl-12", &"x".repeat(2001))).starts_with("REFUSED BAD_VALUE: "));
    assert!(body(send("sendmeter-impl-12", "+++")).starts_with("REFUSED BAD_VALUE: "), "blank");
    assert_eq!(run(&c, &Fake::default(), &["pause", "sendmeter"]).0, 0);
    assert!(body(send("sendmeter-impl-12", "go")).starts_with("REFUSED PAUSED: "));
    assert!(!r.called("herdr agent prompt") && !read(&c.state).unwrap().iter().any(|e| e.kind == "prompt"));
    // 10 sends a minute per device.
    for _ in 0..6 {
        send("synergy-impl-8", "go");
    }
    assert!(send("synergy-impl-8", "go").starts_with("HTTP/1.1 429 "));
    assert_eq!(r.count("herdr agent prompt synergy-impl-8"), 6, "10 attempts incl. the 4 refused");
    let ro = Server::new(&c, &r, None);
    assert!(req(&ro, "POST", "/send", "agent=synergy-impl-8&text=go", &cookie, T).starts_with("HTTP/1.1 405 "));
}

#[test]
fn page_has_no_dash_placeholders() {
    let (c, r) = fixture("dash");
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "18", "12"]);
    let page = req(&Server::new(&c, &r, Some(ADDR.into())), "GET", "/", "", "", T);
    let mut html = page[page.find("<body").unwrap()..].to_string();
    html.truncate(html.find("<script>").unwrap());
    assert!(!html.contains(" - ") && !html.contains(">-<") && !html.contains(" · -") && !html.contains('—'), "{html}");
    assert!(html.contains("<p>#18 queued</p>"), "{html}");
    assert!(html.contains("<option value=\"\" selected>default (profile)</option>"), "unset model");
    assert!(html.contains("<option value=\"\" selected>default (none)</option>"), "unset effort");
}

/// 3-line ASCII horses, per state; working gallops in 2 frames.
#[test]
fn ascii_horse_snapshots() {
    let asc = |state: &str| {
        let a = herdsman::serve::avatar("x", state);
        a[a.find("</i>").unwrap() + 4..a.find("</i><svg").unwrap() + 4].to_string()
    };
    let stand = "<i class=asc><b>    _/>\n~(___)\n  | |</b></i>";
    assert_eq!(asc("working"), "<i class=\"asc run\"><b>    _/>\n~(___)\n /  \\</b><b>    _/>\n~(___)\n  ><</b></i>");
    assert_eq!(asc("idle"), stand);
    assert_eq!(asc("unknown"), stand);
    assert_eq!(asc("done"), "<i class=asc><b>\n    _/>\n~(___)_</b></i>");
    assert_eq!(asc("blocked"), "<i class=asc><b>     />\n ~(_/\n  / \\</b></i>");
    assert_eq!(asc("dead"), asc("blocked"));
}

/// Corral's vector horse: pose per state, a stable coat per name, the gait on working legs.
#[test]
fn vector_horses_keep_a_coat_per_name() {
    use herdsman::serve::avatar;
    let w = avatar("sendmeter-impl-12", "working");
    assert!(w.contains("class=\"av g working\"") && w.contains("class=\"leg leg-nf\" style=\"--a:28deg\"") && w.contains("fill=\"var(--c)\""));
    assert!(avatar("a", "blocked").contains("class=\"av r blocked\"") && avatar("a", "done").contains("av g done"));
    let coat = |n: &str| {
        let a = avatar(n, "idle");
        a[a.find("--c:").unwrap()..a.find(";--d").unwrap()].to_string()
    };
    assert_eq!(coat("synergy-impl-7"), coat("synergy-impl-7"));
    let coats: std::collections::BTreeSet<String> = (0..40).map(|i| coat(&format!("agent-{i}"))).collect();
    assert_eq!(coats.len(), 8, "all 8 Corral coats in use: {coats:?}");
    assert_eq!(herdsman::devices::sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(herdsman::devices::sha256(&[b'a'; 56]), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
}

/// The PR's 390 px screenshots (fixture config): Lanes in 3 modes, the sheet at 78 % and
/// full, the pairing prompt, a sent reply; each with no horizontal scroll. Runs where
/// WebKit is at hand; PNGs go to `$HERDSMAN_SNAPSHOTS` when set.
#[test]
fn lanes_and_sheet_fit_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = fixture("shots");
    std::fs::create_dir_all(c.state.parent().unwrap().join("cache")).unwrap();
    std::fs::write(c.state.parent().unwrap().join("cache/issue-sendmeter-12.json"), json!({"title": "Send meter readings nightly"}).to_string()).unwrap();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = req(&s, "GET", "/", "", "", T);
    let body = |res: String| res[res.find("\r\n\r\n").unwrap() + 4..].to_string();
    let lane = body(req(&s, "GET", "/lane?agent=sendmeter-impl-12", "", "", T));
    let cookie = paired(&s, &c);
    let (pair, sent) = (body(req(&s, "POST", "/send", "agent=sendmeter-impl-12&text=x", "", T)), body(req(&s, "POST", "/send", "agent=sendmeter-impl-12&text=rerun+the+tests", &cookie, T)));
    // fetch() answered with this server's own responses.
    let stub = |send: (u16, &str)| {
        format!(
            "window.fetch = async u => u.startsWith('/lane') ? new Response({}, {{status: 200}}) : new Response({}, {{status: {}}});",
            json!(lane), json!(send.1), send.0
        )
    };
    let open = "open(document.querySelector('.lane[data-agent=\"sendmeter-impl-12\"]'))";
    let submit = "document.querySelector('.reply textarea').value = 'rerun the tests'; document.querySelector('.reply').requestSubmit()";
    let dir = std::env::var("HERDSMAN_SNAPSHOTS").ok();
    let d = tmpdir("shots-png");
    let file = d.join("page.html");
    std::fs::write(&file, &page[page.find("<!doctype").unwrap()..]).unwrap();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
    let scenes = [
        ("lanes-horse", "horse", String::new()),
        ("lanes-ascii", "ascii", String::new()),
        ("lanes-glyph", "glyph", String::new()),
        ("sheet-78", "horse", format!("{}; {open}", stub((200, &sent)))),
        ("sheet-full", "horse", format!("{}; {open}; sh.classList.add('full')", stub((200, &sent)))),
        ("pairing-prompt", "horse", format!("{}; {open}; {submit}", stub((401, &pair)))),
        ("reply-sent", "horse", format!("{}; {open}; {submit}", stub((200, &sent)))),
    ];
    for (name, mode, js) in scenes {
        let png = dir.as_ref().map_or(d.join(format!("{name}.png")), |p| std::path::Path::new(p).join(format!("{name}.png")));
        let o = std::process::Command::new("swift").args([script, file.to_str().unwrap(), "390", "lanes", mode, png.to_str().unwrap(), &js]).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let w: Vec<u32> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(o.status.success() && w.len() == 2, "{name}: {out} {}", String::from_utf8_lossy(&o.stderr));
        assert!(w[0] <= 390 && w[1] == 390, "{name}: scrollWidth {} at {}", w[0], w[1]);
    }
}
