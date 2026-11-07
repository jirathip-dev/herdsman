//! #413: `thread handoff`: a new or an existing thread linked both ways, the text typed into its conductor as from the
//! source, flow-back rows (an issue filed, an ask answered), the board's chips, and a conductor's `thread new` refused.
mod common;
use common::*;
use herdsman::events::{read, Event};
use herdsman::serve::Server;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

const ADDR: &str = "127.0.0.1:7777";

fn setup(name: &str) -> herdsman::Ctx {
    let d = tmpdir(name);
    let c = ctx(
        &d,
        &format!("{CONFIG}\n[fleet.sendmeter.role.conductor]\nharness = \"claude\"\nmodel = \"opus\"\n"),
    );
    let profile = c.config.with_file_name("claude").join("fleet-conductor");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("settings.json"), "{}").unwrap();
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill #12"]).1,
        "t1\n"
    );
    c
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state).unwrap().into_iter().filter(|e| e.kind == kind).collect()
}

/// herdr with `agents` live, ready to start a conductor in a new workspace or type into one.
fn herdr(agents: &str) -> Fake {
    Fake::default()
        .reply("herdr agent list", agents)
        .reply(
            "herdr workspace create",
            r#"{"result":{"root_pane":{"pane_id":"w9:p1"}}}"#,
        )
        .reply("herdr workspace close", "")
        .reply("herdr pane run", "")
        .reply("herdr pane send-keys", "")
        .reply("herdr agent start", "")
        .reply("herdr agent wait", "")
        .reply("herdr agent prompt", "")
}

/// #492: a handoff from t1 to `repo`, then the owner's tap on its card's new-thread row: the new thread's id.
fn to_new(c: &herdsman::Ctx, r: &Fake, repo: &str, title: &str, text: &str) -> String {
    let (code, out) = run(c, r, &["thread", "handoff", "t1", repo, title, text]);
    assert_eq!(code, 0, "{out}");
    let card = kinds(c, "needs-decision").pop().unwrap();
    let targets = card.data["handoff"]["targets"].as_array().unwrap().clone();
    let n = targets.iter().position(|t| t == "new").unwrap() + 1;
    let (code, out) = run(c, r, &["answer", &card.fleet, card.str("id").unwrap(), &n.to_string()]);
    assert_eq!(code, 0, "{out}");
    out.trim().rsplit(' ').next().unwrap().to_string()
}

fn show(c: &herdsman::Ctx, id: &str) -> String {
    run(c, &Fake::default(), &["thread", "show", id]).1
}

/// The real `herdsman` binary on `c`'s files, in a conductor lane of thread `thread` (`None`: the owner's shell).
fn bin(c: &herdsman::Ctx, thread: Option<&str>, args: &[&str], stdin: &str) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdsman"));
    for k in [
        "HERDSMAN_LANE",
        "FLEET_LANE",
        "HERDSMAN_THREAD",
        "HERDSMAN_OUTBOX",
        "SHED",
        "BERTH",
    ] {
        cmd.env_remove(k);
    }
    if let Some(t) = thread {
        cmd.env("HERDSMAN_LANE", "sendmeter:-:conductor")
            .env("HERDSMAN_THREAD", t);
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

/// The conductor of `thread` ran `cmd` in its shell, which printed `stdout` (a `PostToolUse` hook call).
fn ran(c: &herdsman::Ctx, thread: &str, cmd: &str, stdout: &str) {
    let input = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_input": {"command": cmd},
        "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false}});
    let (code, out) = bin(c, Some(thread), &["hook", "--role", "conductor"], &input.to_string());
    assert_eq!(code, 0, "{out}");
}

#[test]
fn a_handoff_to_a_repo_starts_a_linked_thread_and_wakes_its_conductor_with_the_text() {
    let c = setup("handoff-new");
    let r = herdr(NO_AGENTS);
    assert_eq!(to_new(&c, &r, "sendmeter", "Rate limits", "429s from #19"), "t2");
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
    // t2's conductor starts, the handoff its thread's last message, marked as from t1 (not the owner).
    assert!(r.called("herdr agent start conductor-t2"), "{:?}", r.calls.borrow());
    let dir = c.state.with_file_name("conductor").join("t2");
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

    let (code, out) = run(&c, &r, &["thread", "handoff", "t1", "t1", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED BAD_VALUE"), "{out}");
    let (code, out) = run(&c, &r, &["thread", "handoff", "t1", "t9", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED NO_SUCH_THREAD"), "{out}");
    assert_eq!(kinds(&c, "thread-new").len(), 2, "refusals start no thread");
}

/// A handoff to a repo with no conductor is refused before a thread or a link is logged.
#[test]
fn a_handoff_to_a_repo_without_a_conductor_logs_nothing() {
    let c = setup("handoff-noconductor");
    let cfg = std::fs::read_to_string(&c.config).unwrap();
    let other = "\n[fleet.other]\nrepo = \"jirathip-dev/other\"\nlocal = \"/tmp/does-not-matter/other\"\nintegration = \"staging\"\n";
    std::fs::write(&c.config, cfg + other).unwrap();
    let (code, out) = run(&c, &herdr(NO_AGENTS), &["thread", "handoff", "t1", "other", "x", "y"]);
    assert!(code == 3 && out.contains("REFUSED NO_CONDUCTOR"), "{out}");
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
fn a_handoff_to_an_existing_thread_appends_to_it_and_types_into_its_awake_conductor() {
    let c = setup("handoff-reuse");
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t2\n"
    );
    let r = herdr(&agents(&[("conductor-t2", "idle", "/x", "w9:p2")]));
    // An awake t2 conductor: its start is logged.
    let st = json!({"thread": "t2", "agent": "conductor-t2", "pane": "w9:p2", "workspace": "w9", "harness": "claude"});
    herdsman::events::append(&c.state, &Event::new(c.now, "sendmeter", None, "conductor-start", st)).unwrap();
    let (code, out) = run(
        &c,
        &r,
        &["thread", "handoff", "t1", "t2", "Burst size", "burst 100, not 50"],
    );
    assert_eq!((code, out.as_str()), (0, "t2\n"));
    assert_eq!(kinds(&c, "thread-new").len(), 2, "no new thread");
    assert!(
        kinds(&c, "needs-decision").is_empty(),
        "a thread id is delivered at once, no card"
    );
    assert!(
        r.called("herdr agent prompt conductor-t2 [t2] t1: handoff to Search · t2: burst 100, not 50"),
        "{:?}",
        r.calls.borrow()
    );
    assert!(!r.called("herdr agent start"));
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

/// #492: t1 and t2–t8 in sendmeter, each t<n> with a conductor reply `n` minutes in (t5's a long one); t8 archived.
fn threads(name: &str) -> herdsman::Ctx {
    let c = setup(name);
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
        let at = herdsman::Ctx {
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
    let c = threads("handoff-card");
    let at = herdsman::Ctx {
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
    // Newest first, at most 4, then the new thread, then the rest (the board's `more`); t1 itself and archived t8 out.
    assert_eq!(
        card.data["options"],
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
        card.data["handoff"]["targets"],
        json!(["t7", "t6", "t5", "t4", "new", "t3", "t2"])
    );
    // Nothing delivered: no link, no message, no thread, no conductor touched.
    assert_eq!(
        (
            kinds(&c, "thread-handoff").len(),
            kinds(&c, "thread-msg").len(),
            kinds(&c, "thread-new").len()
        ),
        (0, msgs, 8)
    );
    assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());

    // The board draws it with the existing choice rows: 5 shown, 2 behind `more`.
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
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
    let c = threads("handoff-pick");
    let r = herdr(NO_AGENTS);
    let hand = |title: &str, text: &str| run(&c, &r, &["thread", "handoff", "t1", "sendmeter", title, text]).0;
    assert_eq!(hand("Rate limits", "429s from #19"), 0);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "3"]);
    assert!(
        code == 0 && out.starts_with("sendmeter q1: Billing · t5 — invoices") && out.ends_with(" -> t5\n"),
        "{out}"
    );
    assert_eq!(kinds(&c, "thread-new").len(), 8, "an existing thread, no new one");
    assert!(r.called("herdr agent start conductor-t5"), "{:?}", r.calls.borrow());
    // Both rows name the target as `<title> · <id>`.
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
    // #621: another tap, on another row too, gets the one delivery (t5), nothing sent again.
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

    // The owner may type a thread id instead; anything else leaves the card open.
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
    let c = setup("handoff-forward");
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Test"]).1,
        "t2\n"
    );
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t3\n"
    );
    let r = herdr(NO_AGENTS);
    assert_eq!(
        run(&c, &r, &["thread", "handoff", "t1", "t2", "Facets", "add facets"]).1,
        "t2\n"
    );
    // t2's conductor: not mine; sends it on to the repo.
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
    // A row the owner wrote after is not forwarded by a later handoff (the conductor took it on).
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
    // The board shows the mark too.
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
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
    let c = setup("handoff-flow");
    assert_eq!(to_new(&c, &herdr(NO_AGENTS), "sendmeter", "Rate limits", "go"), "t2");
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
    // The number unknown (no URL printed): the title alone.
    ran(&c, "t2", "gh issue create --title='Burst docs' -F -", "");
    assert!(
        show(&c, "t1").ends_with("from t2: filed: Burst docs\n"),
        "{}",
        show(&c, "t1")
    );
    // Another command, or an issue filed in a thread nobody handed off to, adds nothing.
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
    let r = herdr(&agents(&[("conductor-t2", "idle", "/x", "w9:p1")]));
    assert_eq!(run(&c, &r, &["answer", "sendmeter", "q2", "1"]).0, 0);
    let t1 = show(&c, "t1");
    assert_eq!(t1.lines().count(), rows + 1, "{t1}");
    assert!(t1.ends_with("from t2: answered q2: Which scope? → small\n"), "{t1}");
}

#[test]
fn a_conductors_thread_new_is_refused_naming_handoff_and_the_owners_still_works() {
    let c = setup("handoff-perm");
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
    // Any conductor may hand off to any repo (`all` too).
    assert_eq!(to_new(&c, &herdr(NO_AGENTS), "all", "Cross-repo", "both fleets"), "t3");
    assert_eq!(kinds(&c, "thread-new")[2].fleet, "all");
    let usage = run(&c, &Fake::default(), &["help", "--agent"]).1;
    assert!(
        usage.contains("`herdsman thread handoff <FROM> <TO> <TITLE> <TEXT>`"),
        "{usage}"
    );
    let brief = std::fs::read_to_string(c.state.with_file_name("conductor").join("t3/.brief.md")).unwrap();
    assert!(
        brief.contains("herdsman thread handoff <thread> <repo|t<n>>"),
        "{brief}"
    );
}

fn req(s: &Server, method: &str, path: &str, extra: &str, now: u64) -> String {
    let r = format!("{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: 0\r\n\r\n");
    let res = String::from_utf8_lossy(&s.respond(&r, now)).into_owned();
    res[res.find("\r\n\r\n").unwrap() + 4..].to_string()
}

/// A linked pair, t1 → t2, with a flow-back line in t1.
fn linked(name: &str) -> herdsman::Ctx {
    let c = setup(name);
    assert_eq!(to_new(&c, &herdr(NO_AGENTS), "sendmeter", "Rate limits", "go"), "t2");
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
    let c = linked("handoff-tab");
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let chat = &parts(&s, ADDR, &cookie, c.now)[2]; // #269: the [chat] tab as the board draws it
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

/// A tap on a chip, in a thread or in the list, opens the other thread (WebKit, serve.js's click handler).
#[test]
fn a_tap_on_a_chip_opens_the_other_thread() {
    if Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let c = linked("handoff-tap");
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", c.now);
    let page = req(&s, "GET", "/", &cookie, c.now);
    let html = drawn(&page, &s, ADDR, &cookie, c.now); // #269: the [chat] tab as the board draws it
    assert!(html.contains("class=lc data-go"), "the chat tab is on the page");
    let d = tmpdir("handoff-tap-page");
    let (file, png) = (d.join("page.html"), d.join("tap.png"));
    std::fs::write(&file, html).unwrap();
    // 1: t1's handoff chip opens t2; 2: t2's message chip opens t1; 3: t1's row chip in the list opens t2;
    // 5: every chip lays out inline, one line high, inside its list row's line.
    let js = "window.fetch = async () => new Response('', {status: 404}); const tap = s => document.querySelector(s).click(); \
        chatOpen('t1'); tap('.cvp[data-t=\"t1\"] p.qs .lc[data-go=\"t2\"]'); const a = THR === 't2' ? 1 : 0; \
        tap('.cvp[data-t=\"t2\"] summary .lc[data-go=\"t1\"]'); const b = THR === 't1' ? 1 : 0; \
        chatOpen(''); const r = document.querySelector('.tr[data-t=\"t1\"] .lc').getBoundingClientRect(), \
        tp = document.querySelector('.tr[data-t=\"t1\"] .tp').getBoundingClientRect(); \
        const e = [...document.querySelectorAll('#chat .lc')].every(c => getComputedStyle(c).display === 'inline') \
        && r.height > 0 && r.height < 24 && r.top >= tp.top && r.bottom <= tp.bottom + 1 ? 1 : 0; \
        tap('.tr[data-t=\"t1\"] .lc[data-go=\"t2\"]'); const d = THR === 't2' ? 1 : 0; \
        window.measure = [a, b, d, document.querySelector('.cvp[data-t=\"t2\"] summary').parentElement.open ? 1 : 0, e]";
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
    let o = swift()
        .args([
            script,
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

/// #621: herdr types `agent start`'s command into the pane's shell, whose canonical mode takes lines of at most 1,024
/// bytes: the harness command (after `--`) of each start `r` ran.
fn starts(r: &Fake) -> Vec<String> {
    let calls = r.calls.borrow();
    let start = calls.iter().filter(|c| c.starts_with("herdr agent start"));
    start.map(|c| c.split_once(" -- ").unwrap().1.to_string()).collect()
}

/// #621: a 2,400-byte text, in lines like a conductor's proposal.
fn long_text() -> String {
    let text: String = (0..300).map(|i| format!("line {i:03}. ")).collect();
    text[..2400].to_string()
}

/// #621: a 2,400-byte handoff lands in full as a new thread's first conductor message, and again in full once that
/// thread's conductor is asleep (its resume); no start's command line passes herdr's 1,024-byte limit.
#[test]
fn a_long_handoff_lands_in_full_in_a_new_thread_and_in_an_asleep_one_within_herdrs_line_limit() {
    let c = setup("handoff-long");
    let r = herdr(NO_AGENTS);
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
    let md = c.state.with_file_name("conductor").join("t2/.thread.md");
    assert_eq!(std::fs::read_to_string(&md).unwrap(), format!("from t1: {line}\n"));

    // t2's conductor is gone (asleep): the next long handoff resumes it, the text in full in its thread file.
    let (code, out) = run(&c, &r, &["thread", "handoff", "t1", "t2", "More", &text]);
    assert_eq!((code, out.as_str()), (0, "t2\n"));
    assert!(
        std::fs::read_to_string(&md)
            .unwrap()
            .ends_with(&format!("from t1: {line}\n")),
        "{}",
        std::fs::read_to_string(&md).unwrap()
    );
    let s = starts(&r);
    assert_eq!(s.len(), 2, "{s:?}");
    assert!(s[1].contains("--resume"), "{}", s[1]);
    for l in &s {
        assert!(l.len() <= 1024, "a {}-byte start line: {l}", l.len());
    }
}

/// #621: a card from t1 to sendmeter, whose rows are t2 (an existing thread) and a new one.
fn card(name: &str, text: &str) -> herdsman::Ctx {
    let c = setup(name);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Search"]).1,
        "t2\n"
    );
    let hand = ["thread", "handoff", "t1", "sendmeter", "Facets", text];
    assert_eq!(run(&c, &Fake::default(), &hand).0, 0);
    let card = kinds(&c, "needs-decision").pop().unwrap();
    assert_eq!(card.data["handoff"]["targets"], json!(["t2", "new"]));
    c
}

/// #621: the copies of handoff `text` in thread `id` and the `continued in` rows in t1.
fn copies(c: &herdsman::Ctx, id: &str, text: &str) -> (usize, usize) {
    let msgs = kinds(c, "thread-msg");
    let into = msgs
        .iter()
        .filter(|e| e.str("thread") == Some(id) && e.str("text").is_some_and(|t| t.ends_with(text)));
    (
        into.count(),
        show(c, "t1").matches(&format!("continued in {id}: ")).count(),
    )
}

/// #621: three taps on one handoff card's row (a retry, a double submit) deliver it once; each tap gets the first
/// delivery's result.
#[test]
fn three_taps_on_one_handoff_deliver_it_once_and_each_gets_the_first_result() {
    let c = card("handoff-thrice", "add facets");
    let r = herdr(NO_AGENTS);
    for _ in 0..3 {
        let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
        assert_eq!((code, out.as_str()), (0, "sendmeter q1: Search · t2 (0s ago) -> t2\n"));
    }
    assert_eq!(copies(&c, "t2", "add facets"), (1, 1));
    assert_eq!(kinds(&c, "thread-handoff").len(), 1);
    assert_eq!(kinds(&c, "decision").len(), 1);
    assert_eq!(r.count("herdr agent start"), 1, "{:?}", r.calls.borrow());
}

/// #621: a tap whose delivery fails shows its full error in t1; the retry delivers it, still one copy and one row.
#[test]
fn a_failed_handoff_tap_shows_its_error_and_its_retry_is_the_one_delivery() {
    let c = card("handoff-retry", "add facets");
    let why = "input contains a 1486-byte line but the terminal is in canonical mode with a 1024-byte limit; wait for the shell prompt and retry";
    // #623 retries a failed delivery once: both tries fail.
    let r = herdr(NO_AGENTS)
        .once_err("herdr agent start", 1, why)
        .once_err("herdr agent start", 1, why);
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_ne!(code, 0, "{out}");
    assert!(show(&c, "t1").contains(why), "{}", show(&c, "t1"));
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: Search · t2 (0s ago) -> t2\n"));
    assert_eq!(copies(&c, "t2", "add facets"), (1, 1));
    assert_eq!(r.count("herdr agent start"), 3, "{:?}", r.calls.borrow());
    let md = c.state.with_file_name("conductor").join("t2/.thread.md");
    assert_eq!(
        std::fs::read_to_string(md).unwrap(),
        "from t1: handoff to Search · t2: add facets\n"
    );
    assert!(show(&c, "t1").contains(why), "{}", show(&c, "t1"));
}
