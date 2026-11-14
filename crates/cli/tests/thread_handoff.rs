mod common;
use common::*;
use drover::events::{read, Event};
use drover::serve::Server;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

const ADDR: &str = "127.0.0.1:7777";

fn setup(name: &str) -> drover::Ctx {
    let d = tmpdir(name);
    let c = ctx(
        &d,
        &format!("{CONFIG}\n[fleet.sendmeter.role.orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n"),
    );
    let profile = c.config.with_file_name("claude").join("fleet-orchestrator");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("settings.json"), "{}").unwrap();
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill #12"]).1,
        "t1\n"
    );
    c
}

fn acp_setup(name: &str, b: &FakeShed) -> drover::Ctx {
    let cfg = format!(
        "{CONFIG}\n[fleet.sendmeter.role.orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\naccount = \"C\"\n\n\
         [fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        b.sock.display()
    );
    let mut c = ctx(&tmpdir(name), &cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill #12"]).1,
        "t1\n"
    );
    c
}

fn acp() -> Fake {
    let agents = json!([{"id": "claude-acp", "version": "0.101.0", "distribution": "npx", "cmd": ["echo"]}]);
    Fake::default()
        .reply("shed agents --json", &agents.to_string())
        .reply("shed run", "")
        .reply("shed send", "")
}

fn kinds(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn to_new(c: &drover::Ctx, r: &dyn drover::runner::Runner, repo: &str, title: &str, text: &str) -> String {
    let (code, out) = run(c, r, &["thread", "handoff", "t1", repo, title, text]);
    assert_eq!(code, 0, "{out}");
    let card = kinds(c, "needs-decision").pop().unwrap();
    let targets = card.val("handoff")["targets"].as_array().unwrap().clone();
    let n = targets.iter().position(|t| t == "new").unwrap() + 1;
    let (code, out) = run(c, r, &["answer", &card.fleet, card.str("id").unwrap(), &n.to_string()]);
    assert_eq!(code, 0, "{out}");
    out.trim().rsplit(' ').next().unwrap().to_string()
}

fn show(c: &drover::Ctx, id: &str) -> String {
    run(c, &Fake::default(), &["thread", "show", id]).1
}

fn bin(c: &drover::Ctx, thread: Option<&str>, args: &[&str], stdin: &str) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_drover"));
    for k in [
        "DROVER_LANE",
        "FLEET_LANE",
        "DROVER_THREAD",
        "DROVER_OUTBOX",
        "SHED",
        "BERTH",
    ] {
        cmd.env_remove(k);
    }
    if let Some(t) = thread {
        cmd.env("DROVER_LANE", "sendmeter:-:orchestrator")
            .env("DROVER_THREAD", t);
    }
    let mut child = cmd
        .args(args)
        .env("FLEET_CONFIG", &c.config)
        .env("FLEET_STATE", &c.state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    (o.status.code().unwrap_or(-1), text)
}

fn ran(c: &drover::Ctx, thread: &str, cmd: &str, stdout: &str) {
    let input = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": cmd},
        "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false}});
    let (code, out) = bin(c, Some(thread), &["hook", "--role", "orchestrator"], &input.to_string());
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_handoff_to_a_repo_starts_a_linked_thread_and_wakes_its_orchestrator_with_the_text() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-new", &b);
    let r = acp();
    let cli = b.cli(&r);
    assert_eq!(to_new(&c, &cli, "sendmeter", "Rate limits", "429s from #19"), "t2");
    let new = kinds(&c, "thread-new");
    assert_eq!(
        (new.len(), new[1].str("title"), new[1].fleet.as_str()),
        (2, Some("Rate limits"), "sendmeter")
    );
    let h = kinds(&c, "thread-handoff");
    assert_eq!(h.len(), 1);
    assert_eq!(
        (h[0].str("from"), h[0].str("to"), h[0].str("title"), h[0].str("text")),
        (Some("t1"), Some("t2"), Some("Rate limits"), Some("429s from #19"))
    );
    assert!(
        r.called("shed run --acp claude-acp --policy allow --name orchestrator-t2"),
        "{:?}",
        r.calls.borrow()
    );
    let sent = r.calls.borrow().join("\n");
    assert!(
        sent.contains("\n\n[t2] t1: handoff to Rate limits · t2: 429s from #19"),
        "{sent}"
    );
    let dir = c.state.with_file_name("orchestrator").join("t2");
    assert_eq!(
        std::fs::read_to_string(dir.join(".thread.md")).unwrap(),
        "from t1: handoff to Rate limits · t2: 429s from #19\n"
    );
    let msg = kinds(&c, "thread-msg");
    assert_eq!((msg[0].str("thread"), msg[0].str("from")), (Some("t2"), Some("t1")));
    assert!(
        show(&c, "t1").ends_with("continued in t2: Rate limits → Rate limits · t2\n"),
        "{}",
        show(&c, "t1")
    );
    assert!(
        show(&c, "t2").ends_with("from t1: handoff to Rate limits · t2: 429s from #19\n"),
        "{}",
        show(&c, "t2")
    );

    let (code, out) = run(&c, &cli, &["thread", "handoff", "t1", "t1", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED BAD_VALUE"), "{out}");
    let (code, out) = run(&c, &cli, &["thread", "handoff", "t1", "t9", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED NO_SUCH_THREAD"), "{out}");
    assert_eq!(kinds(&c, "thread-new").len(), 2, "refusals start no thread");
}

#[test]
fn a_handoff_to_a_repo_without_a_orchestrator_logs_nothing() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-noorchestrator", &b);
    let cfg = std::fs::read_to_string(&c.config).unwrap();
    let other = "\n[fleet.other]\nrepo = \"jirathip-dev/other\"\nlocal = \"/tmp/does-not-matter/other\"\nintegration = \"staging\"\n";
    std::fs::write(&c.config, cfg + other).unwrap();
    let (code, out) = run(&c, &b.cli(&acp()), &["thread", "handoff", "t1", "other", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED NO_ORCHESTRATOR"), "{out}");
    assert_eq!(
        (
            kinds(&c, "thread-new").len(),
            kinds(&c, "thread-handoff").len(),
            kinds(&c, "needs-decision").len()
        ),
        (1, 0, 0)
    );
}

#[test]
fn a_handoff_to_an_existing_thread_appends_to_it_and_types_into_its_awake_orchestrator() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-reuse", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t2\n"
    );
    b.add(
        "orchestrator-t2",
        json!({"fleet": "sendmeter", "role": "orchestrator", "thread": "t2"}),
    );
    let st = json!({"thread": "t2", "agent": "orchestrator-t2", "pane": "orchestrator-t2", "runtime": "shed-acp", "harness": "claude"});
    drover::events::append(
        &c.state,
        &Event::new(c.now, "sendmeter", None, "orchestrator-start", st),
    )
    .unwrap();
    let r = acp();
    let (code, out) = run(
        &c,
        &b.cli(&r),
        &["thread", "handoff", "t1", "t2", "Burst size", "burst 100, not 50"],
    );
    assert_eq!((code, out.as_str()), (0, "t2\n"));
    assert_eq!(kinds(&c, "thread-new").len(), 2, "no new thread");
    assert!(
        kinds(&c, "needs-decision").is_empty(),
        "a thread id is delivered at once, no card"
    );
    assert!(
        r.called("shed send orchestrator-t2 [t2] t1: handoff to Search · t2: burst 100, not 50"),
        "{:?}",
        r.calls.borrow()
    );
    assert!(!r.called("shed run"));
    assert!(
        show(&c, "t2").ends_with("from t1: handoff to Search · t2: burst 100, not 50\n"),
        "{}",
        show(&c, "t2")
    );
    assert!(
        show(&c, "t1").ends_with("continued in t2: Burst size → Search · t2\n"),
        "{}",
        show(&c, "t1")
    );
}

fn threads(name: &str, b: &FakeShed) -> drover::Ctx {
    let c = acp_setup(name, b);
    let titles = [
        "Main chat",
        "Test",
        "Search",
        "Billing",
        "Rate limits",
        "Docs",
        "Release",
    ];
    for (i, t) in titles.iter().enumerate() {
        let n = i as u64 + 2;
        let at = drover::Ctx {
            now: c.now + n * 60,
            ..c.clone()
        };
        let id = format!("t{n}");
        assert_eq!(
            run(&at, &Fake::default(), &["thread", "new", "sendmeter", t]).1,
            format!("{id}\n")
        );
        let topic = if n == 5 {
            "invoices double-charge on retry when the card issuer times out mid-capture".to_string()
        } else {
            format!("topic {n}")
        };
        assert_eq!(run(&at, &Fake::default(), &["thread", "reply", &id, &topic]).0, 0);
    }
    assert_eq!(run(&c, &Fake::default(), &["thread", "archive", "t8"]).0, 0);
    c
}

#[test]
fn a_handoff_to_a_repo_posts_a_choice_card_of_its_threads_and_delivers_nothing_before_the_tap() {
    let b = FakeShed::new();
    let c = threads("handoff-card", &b);
    let at = drover::Ctx {
        now: c.now + 1000,
        ..c.clone()
    };
    let msgs = kinds(&c, "thread-msg").len();
    let r = Fake::default();
    let (code, out) = run(
        &at,
        &r,
        &["thread", "handoff", "t1", "sendmeter", "Rate limits", "429s from #19"],
    );
    assert_eq!(
        (code, out.as_str()),
        (0, "q1: the owner picks the thread; nothing is delivered until then\n")
    );
    let card = kinds(&c, "needs-decision").pop().unwrap();
    assert_eq!((card.str("thread"), card.str("kind")), (Some("t1"), Some("owner")));
    assert!(
        card.str("question")
            .unwrap()
            .contains("Nothing is delivered until you tap"),
        "{card:?}"
    );
    assert_eq!(
        *card.val("options"),
        json!([
            "Docs · t7 — topic 7 (9m ago)",
            "Rate limits · t6 — topic 6 (10m ago)",
            "Billing · t5 — invoices double-charge on retry when the card issuer times o (11m ago)",
            "Search · t4 — topic 4 (12m ago)",
            "new thread: Rate limits",
            "Test · t3 — topic 3 (13m ago)",
            "Main chat · t2 — topic 2 (14m ago)"
        ])
    );
    assert_eq!(
        card.val("handoff")["targets"],
        json!(["t7", "t6", "t5", "t4", "new", "t3", "t2"])
    );
    assert_eq!(
        (
            kinds(&c, "thread-handoff").len(),
            kinds(&c, "thread-msg").len(),
            kinds(&c, "thread-new").len()
        ),
        (0, msgs, 8)
    );
    assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());
    assert!(b.methods().is_empty(), "{:?}", b.methods());

    let f = Fake::default();
    let r = b.on(&f);
    let s = Server::new(&at, &r, Some(ADDR.into()));
    let cookie = pair(&s, &at, "Mozilla/5.0 (iPhone)", at.now);
    let chat = &parts(&s, ADDR, &cookie, at.now)[2];
    let v = &chat[chat.find("<div class=cvp data-t=\"t1\">").unwrap()..];
    let v = &v[..v.find("</form></div>").unwrap()];
    let more = v.find("<details class=more><summary>more (2)</summary>").expect(v);
    assert_eq!(v.matches("name=choice").count(), 7, "{v}");
    assert_eq!(v[..more].matches("name=choice").count(), 5, "{v}");
    assert!(v[..more].contains("5. new thread: Rate limits"), "{v}");
    assert!(
        v[more..].contains("7. Main chat · <span class=tl data-go=\"t2\">t2</span>"),
        "{v}"
    );
}

#[test]
fn a_tap_on_a_thread_delivers_there_and_a_tap_on_new_starts_a_thread() {
    let b = FakeShed::new();
    let c = threads("handoff-pick", &b);
    let f = acp();
    let r = b.cli(&f);
    let hand = |title: &str, text: &str| run(&c, &r, &["thread", "handoff", "t1", "sendmeter", title, text]).0;
    assert_eq!(hand("Rate limits", "429s from #19"), 0);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "3"]);
    assert!(
        code == 0 && out.starts_with("sendmeter q1: Billing · t5 — invoices") && out.ends_with(" -> t5\n"),
        "{out}"
    );
    assert_eq!(kinds(&c, "thread-new").len(), 8, "an existing thread, no new one");
    assert!(
        f.called("shed run --acp claude-acp --policy allow --name orchestrator-t5"),
        "{:?}",
        f.calls.borrow()
    );
    assert!(
        show(&c, "t5").ends_with("from t1: handoff to Billing · t5: 429s from #19\n"),
        "{}",
        show(&c, "t5")
    );
    assert!(
        show(&c, "t1").ends_with("continued in t5: Rate limits → Billing · t5\n"),
        "{}",
        show(&c, "t1")
    );
    let (code, again) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, again), (0, out));
    assert_eq!(kinds(&c, "thread-handoff").len(), 1);

    assert_eq!(hand("Webhooks", "retry storms"), 0);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q2", "5"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q2: new thread: Webhooks -> t9\n"));
    let new = kinds(&c, "thread-new").pop().unwrap();
    assert_eq!((new.str("thread"), new.str("title")), (Some("t9"), Some("Webhooks")));
    assert!(
        show(&c, "t9").ends_with("from t1: handoff to Webhooks · t9: retry storms\n"),
        "{}",
        show(&c, "t9")
    );

    assert_eq!(hand("Search", "facets"), 0);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q3", "nowhere"]);
    assert!(code == 3 && out.contains("REFUSED BAD_VALUE"), "{out}");
    assert_eq!(
        run(&c, &r, &["answer", "sendmeter", "q3", "t4"]).1,
        "sendmeter q3: t4 -> t4\n"
    );
    assert!(
        show(&c, "t4").ends_with("from t1: handoff to Search · t4: facets\n"),
        "{}",
        show(&c, "t4")
    );
}

#[test]
fn a_forward_marks_the_received_row_and_a_handoff_after_the_owner_wrote_does_not() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-forward", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Test"]).1,
        "t2\n"
    );
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t3\n"
    );
    let f = acp();
    let r = b.cli(&f);
    assert_eq!(
        run(&c, &r, &["thread", "handoff", "t1", "t2", "Facets", "add facets"]).1,
        "t2\n"
    );
    assert_eq!(
        run(
            &c,
            &r,
            &["thread", "handoff", "t2", "sendmeter", "Facets", "add facets"]
        )
        .0,
        0
    );
    assert!(
        show(&c, "t2").contains("from t1: handoff to Test · t2: add facets (forwarded)\n"),
        "{}",
        show(&c, "t2")
    );
    assert_eq!(
        run(&c, &r, &["thread", "handoff", "t1", "t2", "Docs", "doc it"]).1,
        "t2\n"
    );
    assert_eq!(run(&c, &r, &["thread", "say", "t2", "yes, do it here"]).0, 0);
    assert_eq!(
        run(&c, &r, &["thread", "handoff", "t2", "t3", "Index", "reindex"]).1,
        "t3\n"
    );
    let t2 = show(&c, "t2");
    assert!(t2.contains("handoff to Test · t2: doc it\n"), "{t2}");
    assert_eq!(t2.matches("(forwarded)").count(), 1, "{t2}");
    let f = Fake::default();
    let r = b.on(&f);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let chat = &parts(&s, ADDR, &cookie, c.now)[2];
    assert_eq!(
        chat.matches("<p>handoff to Test · <span class=tl data-go=\"t2\">t2</span>: add facets (forwarded)</p>")
            .count(),
        1,
        "{chat}"
    );
}

#[test]
fn an_issue_filed_and_an_ask_answered_in_the_target_each_add_one_row_to_the_source() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-flow", &b);
    assert_eq!(to_new(&c, &b.cli(&acp()), "sendmeter", "Rate limits", "go"), "t2");
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Unlinked"]).1,
        "t3\n"
    );
    let before = show(&c, "t1");

    let cmd = "gh issue create -R acme/x -t \"Rate-limit /v1/search\" -F -";
    ran(&c, "t2", cmd, "https://github.com/acme/x/issues/52\n");
    assert_eq!(
        show(&c, "t1"),
        format!("{before}from t2: filed #52: Rate-limit /v1/search\n")
    );
    ran(&c, "t2", "gh issue create --title='Burst docs' -F -", "");
    assert!(
        show(&c, "t1").ends_with("from t2: filed: Burst docs\n"),
        "{}",
        show(&c, "t1")
    );
    let rows = show(&c, "t1").lines().count();
    ran(&c, "t2", "gh issue list", "#52 Rate-limit");
    ran(&c, "t3", cmd, "https://github.com/acme/x/issues/53\n");
    assert_eq!(show(&c, "t1").lines().count(), rows);
    assert!(!show(&c, "t3").contains("filed"));

    let ask = [
        "ask",
        "sendmeter",
        "-",
        "Which scope?",
        "--option",
        "small",
        "--option",
        "full",
        "--thread",
        "t2",
    ];
    assert_eq!(run(&c, &Fake::default(), &ask).1, "q2\n");
    let r = acp();
    assert_eq!(run(&c, &b.cli(&r), &["answer", "sendmeter", "q2", "1"]).0, 0);
    assert!(
        r.called("shed send orchestrator-t2 DECISION q2: small") && !r.called("shed run"),
        "{:?}",
        r.calls.borrow()
    );
    let t1 = show(&c, "t1");
    assert_eq!(t1.lines().count(), rows + 1, "{t1}");
    assert!(t1.ends_with("from t2: answered q2: Which scope? → small\n"), "{t1}");
}

#[test]
fn a_orchestrators_thread_new_is_refused_naming_handoff_and_the_owners_still_works() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-perm", &b);
    let (code, out) = bin(&c, Some("t1"), &["thread", "new", "sendmeter", "Side quest"], "");
    assert!(
        code == 3 && out.contains("REFUSED USE_HANDOFF") && out.contains("thread handoff"),
        "{code} {out}"
    );
    assert_eq!(kinds(&c, "thread-new").len(), 1);
    assert_eq!(
        bin(&c, None, &["thread", "new", "sendmeter", "Side quest"], ""),
        (0, "t2\n".into())
    );
    assert_eq!(to_new(&c, &b.cli(&acp()), "all", "Cross-repo", "both fleets"), "t3");
    assert_eq!(kinds(&c, "thread-new")[2].fleet, "all");
    let usage = run(&c, &Fake::default(), &["help", "--agent"]).1;
    assert!(
        usage.contains("`drover thread handoff <FROM> <TO> <TITLE> <TEXT>`"),
        "{usage}"
    );
    let brief = std::fs::read_to_string(c.state.with_file_name("orchestrator").join("t3/.brief.md")).unwrap();
    assert!(brief.contains("drover thread handoff <thread> <repo|t<n>>"), "{brief}");
}

fn req(s: &Server, method: &str, path: &str, extra: &str, now: u64) -> String {
    let r = format!("{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: 0\r\n\r\n");
    let res = String::from_utf8_lossy(&s.respond(&r, now)).into_owned();
    res[res.find("\r\n\r\n").unwrap() + 4..].to_string()
}

fn linked(name: &str, b: &FakeShed) -> drover::Ctx {
    let c = acp_setup(name, b);
    assert_eq!(to_new(&c, &b.cli(&acp()), "sendmeter", "Rate limits", "go"), "t2");
    ran(
        &c,
        "t2",
        "gh issue create -t \"Rate-limit search\"",
        "https://github.com/acme/x/issues/52",
    );
    c
}

#[test]
fn the_chat_tab_shows_a_chip_on_each_linked_row_and_thread_row() {
    let b = FakeShed::new();
    let c = linked("handoff-tab", &b);
    let f = Fake::default();
    let r = b.on(&f);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let chat = &parts(&s, ADDR, &cookie, c.now)[2];
    let view = |t: &str| {
        let v = &chat[chat.find(&format!("<div class=cvp data-t=\"{t}\">")).unwrap()..];
        v[..v.find("</form></div>").unwrap()].to_string()
    };
    let (go1, go2) = (
        "<span class=lc data-go=\"t1\">from t1</span>",
        "<span class=lc data-go=\"t2\">continued in t2</span>",
    );
    assert!(
        view("t1").contains(&format!(
            " class=qs>{go2} · Rate limits → Rate limits · <span class=tl data-go=\"t2\">t2</span></p>"
        )),
        "{}",
        view("t1")
    );
    let back = " class=\"qs note\"><span class=lc data-go=\"t2\">from t2</span> filed #52: Rate-limit search</p>";
    assert!(view("t1").contains(back), "{}", view("t1"));
    assert!(
        view("t2").contains(&format!(" class=\"blk ai\" open><summary><span class=k>{go1}</span>")),
        "{}",
        view("t2")
    );
    let row = |t: &str| {
        let r = &chat[chat.find(&format!("href=#chat data-t=\"{t}\">")).unwrap()..];
        r[..r.find("</a>").unwrap()].to_string()
    };
    assert!(row("t1").contains(&format!("<span class=tp>{go2} ")), "{}", row("t1"));
    assert!(row("t2").contains(&format!("<span class=tp>{go1} ")), "{}", row("t2"));
}

#[test]
fn a_tap_on_a_chip_opens_the_other_thread() {
    if no_webkit() {
        return;
    }
    let b = FakeShed::new();
    let c = linked("handoff-tap", &b);
    let f = Fake::default();
    let r = b.on(&f);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let page = req(&s, "GET", "/", &cookie, c.now);
    let html = drawn(&page, &s, ADDR, &cookie, c.now);
    assert!(html.contains("class=lc data-go"), "the chat tab is on the page");
    let d = tmpdir("handoff-tap-page");
    let (file, png) = (d.join("page.html"), d.join("tap.png"));
    std::fs::write(&file, html).unwrap();
    let js = "window.fetch = async () => new Response('', {status: 404}); const tap = s => document.querySelector(s).click(); \
        chatOpen('t1'); tap('.cvp[data-t=\"t1\"] p.qs .lc[data-go=\"t2\"]'); const a = THR === 't2' ? 1 : 0; \
        tap('.cvp[data-t=\"t2\"] summary .lc[data-go=\"t1\"]'); const b = THR === 't1' ? 1 : 0; \
        chatOpen(''); const r = document.querySelector('.tr[data-t=\"t1\"] .lc').getBoundingClientRect(), \
        tp = document.querySelector('.tr[data-t=\"t1\"] .tp').getBoundingClientRect(); \
        const e = [...document.querySelectorAll('#chat .lc')].every(c => getComputedStyle(c).display === 'inline') \
        && r.height > 0 && r.height < 24 && r.top >= tp.top && r.bottom <= tp.bottom + 1 ? 1 : 0; \
        tap('.tr[data-t=\"t1\"] .lc[data-go=\"t2\"]'); const d = THR === 't2' ? 1 : 0; \
        window.measure = [a, b, d, document.querySelector('.cvp[data-t=\"t2\"] summary').parentElement.open ? 1 : 0, e]";
    let o = webview()
        .args([
            file.to_str().unwrap(),
            "390",
            "chat",
            "horse",
            png.to_str().unwrap(),
            js,
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    let v: Vec<f64> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
    assert_eq!(
        v.get(2..),
        Some(&[1.0, 1.0, 1.0, 1.0, 1.0][..]),
        "{out} {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn starts(b: &FakeShed) -> Vec<String> {
    let sent = b.sent("orchestrator-t2").into_iter();
    sent.filter(|l| l.starts_with("exec ")).collect()
}

fn long_text() -> String {
    let text: String = (0..300).map(|i| format!("line {i:03}. ")).collect();
    text[..2400].to_string()
}

#[test]
fn a_long_handoff_lands_in_full_in_a_new_thread_and_in_an_asleep_one() {
    let b = FakeShed::new();
    let c = acp_setup("handoff-long-acp", &b);
    let f = acp();
    let r = b.cli(&f);
    let text = long_text();
    assert_eq!(to_new(&c, &r, "sendmeter", "Big plan", &text), "t2");
    let first = kinds(&c, "thread-msg")
        .into_iter()
        .find(|e| e.str("thread") == Some("t2"))
        .unwrap();
    let line = format!("handoff to Big plan · t2: {text}");
    assert_eq!(
        (first.str("from"), first.str("text")),
        (Some("t1"), Some(line.as_str()))
    );
    let md = c.state.with_file_name("orchestrator").join("t2/.thread.md");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), format!("from t1: {line}\n"));

    b.remove("orchestrator-t2");
    let (code, out) = run(&c, &r, &["thread", "handoff", "t1", "t2", "More", &text]);
    assert_eq!((code, out.as_str()), (0, "t2\n"));
    assert!(
        std::fs::read_to_string(&md)
            .unwrap()
            .ends_with(&format!("from t1: {line}\n")),
        "{}",
        std::fs::read_to_string(&md).unwrap()
    );
    assert_eq!(f.count("shed run --acp"), 2, "{:?}", f.calls.borrow());
    let prompts: Vec<String> = f
        .calls
        .borrow()
        .iter()
        .filter(|l| l.starts_with("shed send orchestrator-t2 "))
        .cloned()
        .collect();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    for p in &prompts {
        assert!(p.ends_with(&format!("\n\n[t2] t1: {line}")), "{p}");
    }
}

#[test]
fn a_long_handoff_lands_in_full_in_a_new_thread_and_in_an_asleep_one_within_the_shells_line_limit() {
    let c = setup("handoff-long");
    let b = FakeShed::attached(&c);
    let r = Fake::default();
    let text = long_text();
    assert_eq!(text.len(), 2400);
    assert_eq!(to_new(&c, &r, "sendmeter", "Big plan", &text), "t2");
    let first = kinds(&c, "thread-msg")
        .into_iter()
        .find(|e| e.str("thread") == Some("t2"))
        .unwrap();
    let line = format!("handoff to Big plan · t2: {text}");
    assert_eq!(
        (first.str("from"), first.str("text")),
        (Some("t1"), Some(line.as_str()))
    );
    let md = c.state.with_file_name("orchestrator").join("t2/.thread.md");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), format!("from t1: {line}\n"));

    b.remove("orchestrator-t2");
    let (code, out) = run(&c, &r, &["thread", "handoff", "t1", "t2", "More", &text]);
    assert_eq!((code, out.as_str()), (0, "t2\n"));
    assert!(
        std::fs::read_to_string(&md)
            .unwrap()
            .ends_with(&format!("from t1: {line}\n")),
        "{}",
        std::fs::read_to_string(&md).unwrap()
    );
    let s = starts(&b);
    assert_eq!(s.len(), 2, "{s:?}");
    assert!(s[1].contains("--resume"), "{}", s[1]);
    for l in &s {
        assert!(l.len() <= 1024, "a {}-byte start line: {l}", l.len());
    }
}

fn card(name: &str, text: &str, b: &FakeShed) -> drover::Ctx {
    let c = acp_setup(name, b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t2\n"
    );
    let hand = ["thread", "handoff", "t1", "sendmeter", "Facets", text];
    assert_eq!(run(&c, &Fake::default(), &hand).0, 0);
    let card = kinds(&c, "needs-decision").pop().unwrap();
    assert_eq!(card.val("handoff")["targets"], json!(["t2", "new"]));
    c
}

fn copies(c: &drover::Ctx, id: &str, text: &str) -> (usize, usize) {
    let msgs = kinds(c, "thread-msg");
    let into = msgs
        .iter()
        .filter(|e| e.str("thread") == Some(id) && e.str("text").is_some_and(|t| t.ends_with(text)));
    (
        into.count(),
        show(c, "t1").matches(&format!("continued in {id}: ")).count(),
    )
}

#[test]
fn three_taps_on_one_handoff_deliver_it_once_and_each_gets_the_first_result() {
    let b = FakeShed::new();
    let c = card("handoff-thrice", "add facets", &b);
    let f = acp();
    let r = b.cli(&f);
    for _ in 0..3 {
        let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
        assert_eq!((code, out.as_str()), (0, "sendmeter q1: Search · t2 (0s ago) -> t2\n"));
    }
    assert_eq!(copies(&c, "t2", "add facets"), (1, 1));
    assert_eq!(kinds(&c, "thread-handoff").len(), 1);
    assert_eq!(kinds(&c, "decision").len(), 1);
    assert_eq!(f.count("shed run"), 1, "{:?}", f.calls.borrow());
}

#[test]
fn a_failed_first_try_of_a_handoff_tap_is_retried_and_delivered_once() {
    let b = FakeShed::new();
    let c = card("handoff-retry-once", "add facets", &b);
    let f = acp().once_err("shed run", 1, "timed out waiting for agent startup");
    let (code, out) = run(&c, &b.cli(&f), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: Search · t2 (0s ago) -> t2\n"));
    assert_eq!(copies(&c, "t2", "add facets"), (1, 1));
    assert_eq!(f.count("shed run"), 2, "{:?}", f.calls.borrow());
    assert!(!show(&c, "t1").contains("not delivered"), "{}", show(&c, "t1"));
}

#[test]
fn a_failed_handoff_tap_shows_its_error_and_its_retry_is_the_one_delivery() {
    let b = FakeShed::new();
    let c = card("handoff-retry", "add facets", &b);
    let why = "input contains a 1486-byte line but the terminal is in canonical mode with a 1024-byte limit; wait for the shell prompt and retry";
    let f = acp().once_err("shed run", 1, why).once_err("shed run", 1, why);
    let r = b.cli(&f);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_ne!(code, 0, "{out}");
    assert!(show(&c, "t1").contains(why), "{}", show(&c, "t1"));
    assert!(
        kinds(&c, "orchestrator-start").is_empty(),
        "{:?}",
        kinds(&c, "orchestrator-start")
    );
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: Search · t2 (0s ago) -> t2\n"));
    assert_eq!(copies(&c, "t2", "add facets"), (1, 1));
    assert_eq!(f.count("shed run"), 3, "{:?}", f.calls.borrow());
    let md = c.state.with_file_name("orchestrator").join("t2/.thread.md");
    assert_eq!(
        std::fs::read_to_string(md).unwrap(),
        "from t1: handoff to Search · t2: add facets\n"
    );
    assert!(show(&c, "t1").contains(why), "{}", show(&c, "t1"));
}

fn v1_post(s: &Server, cookie: &str, now: u64, op: &str, body: &serde_json::Value) -> serde_json::Value {
    let body = body.to_string();
    let req = format!(
        "POST /api/v1/{op} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&req, now)).into_owned();
    serde_json::from_str(res.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn items(s: &Server, cookie: &str, now: u64, t: &str) -> Vec<serde_json::Value> {
    let page: serde_json::Value = serde_json::from_str(&api(s, ADDR, cookie, now, &format!("thread/{t}"))).unwrap();
    let msgs = page["msgs"].as_array().unwrap().iter();
    msgs.filter(|m| !m["handoff"].is_null())
        .map(|m| m["handoff"].clone())
        .collect()
}

#[test]
fn a_pending_repo_handoff_is_one_item_with_choices_and_its_pick_delivers_it_in_both_threads() {
    let b = FakeShed::new();
    let c = card("handoff-v1", "add facets\nthen reindex", &b);
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let sent = items(&s, &ck, c.now, "t1");
    assert_eq!(sent.len(), 1, "{sent:?}");
    let h = &sent[0];
    let card = kinds(&c, "needs-decision").pop().unwrap();
    assert_eq!(
        (&h["kind"], &h["id"], &h["state"], &h["folded"]),
        (&json!("handoff"), &json!(card.seq), &json!("pending"), &json!(true))
    );
    assert_eq!(
        h["from"],
        json!({"agent": "conductor", "thread": "t1", "repo": "sendmeter"})
    );
    assert_eq!(h["to"], json!({"repo": "sendmeter", "pending": true}));
    assert_eq!(
        (&h["title"], &h["summary"], &h["text"]),
        (
            &json!("Facets"),
            &json!("add facets"),
            &json!("add facets\nthen reindex")
        )
    );
    let rows: Vec<_> = h["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["n"].clone(), c["thread"].clone()))
        .collect();
    assert_eq!(rows, [(json!(1), json!("t2")), (json!(2), json!("new"))]);
    assert_eq!(h["choices"][1]["text"], "new thread: Facets");
    assert!(
        items(&s, &ck, c.now, "t2").is_empty(),
        "nothing delivered before the pick"
    );

    let pick = &h["choices"][0];
    let mtime =
        serde_json::from_str::<serde_json::Value>(&api(&s, ADDR, &ck, c.now, "state")).unwrap()["mtime"].clone();
    let body = json!({"fleet": pick["fleet"], "id": pick["id"], "choice": pick["n"], "mtime": mtime});
    let got = v1_post(&s, &ck, c.now, pick["op"].as_str().unwrap(), &body);
    assert_eq!(got["ok"], json!(true), "{got}");
    for (t, folded) in [("t1", true), ("t2", false)] {
        let got = items(&s, &ck, c.now, t);
        assert_eq!(got.len(), 1, "{t}: {got:?}");
        let h = &got[0];
        assert_eq!(
            (&h["id"], &h["state"], &h["to"], &h["folded"], &h["choices"]),
            (
                &json!(card.seq),
                &json!("delivered"),
                &json!("t2"),
                &json!(folded),
                &json!([])
            ),
            "{t}"
        );
        assert_eq!(h["text"], "add facets\nthen reindex", "{t}");
    }
}

#[test]
fn a_repos_threads_list_newest_active_first_with_newest_on_the_first() {
    let b = FakeShed::new();
    let c = acp_setup("repo-threads", &b);
    for (n, t) in [(2, "Search"), (3, "Billing")] {
        let at = drover::Ctx {
            now: c.now + n * 60,
            ..c.clone()
        };
        assert_eq!(
            run(&at, &Fake::default(), &["thread", "new", "sendmeter", t]).1,
            format!("t{n}\n")
        );
        assert_eq!(
            run(&at, &Fake::default(), &["thread", "reply", &format!("t{n}"), "on it"]).0,
            0
        );
    }
    let late = drover::Ctx {
        now: c.now + 600,
        ..c.clone()
    };
    assert_eq!(run(&late, &Fake::default(), &["thread", "reply", "t1", "grilled"]).0, 0);
    let r = Fake::default();
    let s = Server::new(&late, &r, Some(ADDR.into()));
    let v: serde_json::Value = serde_json::from_str(&api(&s, ADDR, "", late.now, "repo/sendmeter/threads")).unwrap();
    assert_eq!(v["repo"], "sendmeter");
    let rows: Vec<_> = v["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["id"].clone(), t["newest"].clone(), t["last"]["line"].clone()))
        .collect();
    assert_eq!(
        rows,
        [
            (json!("t1"), json!(true), json!("conductor: grilled")),
            (json!("t3"), json!(false), json!("conductor: on it")),
            (json!("t2"), json!(false), json!("conductor: on it"))
        ]
    );
    assert_eq!(v["threads"][0]["last"]["ts"], json!(late.now));
    assert_eq!(
        (&v["threads"][0]["title"], &v["threads"][0]["archived"]),
        (&json!("Grill #12"), &json!(false))
    );
    let req = format!("GET /api/v1/repo/nowhere/threads HTTP/1.1\r\nHost: {ADDR}\r\n\r\n");
    let res = String::from_utf8_lossy(&s.respond(&req, late.now)).into_owned();
    assert!(res.starts_with("HTTP/1.1 404") && res.contains("NOT_FOUND"), "{res}");
}
