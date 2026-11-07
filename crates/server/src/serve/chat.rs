//! #386: the `[chat]` tab (docs/design/conductor-chat/): per repo its conductor threads, and each thread as #138's chat
//! blocks with choice rows, images and files; the owner's writes (`POST /thread`) and the files (`GET /thread/<t>/<name>`).
use super::ago;
use crate::api::{Answered, ChatRepo, ChatTab, Chip, FileRef, Link, Msg, QueuedMsg, ThreadPage, ThreadRow, ThreadView};
use crate::conductor::{self, of};
use crate::config::Config;
use crate::events::{closed, Event};
use crate::runner::Runner;
use crate::Ctx;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// `a` if `c`, else `b`.
fn or<'a>(c: bool, a: &'a str, b: &'a str) -> &'a str {
    [b, a][usize::from(c)]
}

/// The files a message carries.
pub fn files_of(e: &Event) -> impl Iterator<Item = &Value> {
    e.data.get("files").and_then(Value::as_array).into_iter().flatten()
}

/// A file name's extension, lower case.
pub fn ext(name: &str) -> String {
    name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase()
}

/// Its open asks (`ask --thread`): they wait in the thread, never in the needs-you stack; #541: and the lane asks put
/// to the owner here, while open.
fn open_ask<'a>(events: &'a [Event], id: &'a str) -> Option<&'a Event> {
    let lane = |e: &Event| (e.kind == "ask-owner").then(|| conductor::lane_of(events, e)).flatten();
    of(events, id).find(|e| {
        let a = if e.kind == "needs-decision" { Some(*e) } else { lane(e) };
        a.is_some_and(|a| closed(events, a).is_none())
    })
}

/// #541: a conductor's FYI line on lane ask `id`: its ask, while the owner may still [change] it (the conductor's
/// answer the last, under `CHANGE` old).
fn change<'a>(ctx: &Ctx, events: &'a [Event], e: &Event) -> Option<&'a Event> {
    let id = e.str("change")?;
    let is = |d: &&Event| d.kind == "decision" && d.fleet == e.fleet && d.str("id") == Some(id);
    let d = events.iter().rfind(is)?;
    let by = d.str("by").unwrap_or_default();
    (by.starts_with("conductor-") && ctx.now < d.ts + crate::CHANGE).then_some(())?;
    events
        .iter()
        .find(|a| a.kind == "needs-decision" && a.fleet == e.fleet && a.str("id") == Some(id))
}

/// #407: a queued message's id, and its files.
fn qid(e: &Event) -> u64 {
    e.data.get("q").and_then(Value::as_u64).unwrap_or(e.seq)
}
fn qfiles(e: &Event) -> Vec<Value> {
    files_of(e).cloned().collect()
}

/// #407: the thread's queued messages, oldest first, by id (their first `thread-queue`'s seq; an edit names it `q`).
fn queued<'a>(events: &'a [Event], id: &'a str) -> BTreeMap<u64, &'a Event> {
    let mut q = BTreeMap::new();
    for e in of(events, id) {
        let n = qid(e);
        match e.kind.as_str() {
            "thread-queue" => drop(q.insert(n, e)),
            "thread-unqueue" => drop(q.remove(&n)),
            _ => {}
        }
    }
    q
}

/// `choose` (an open ask), `working` (mid-turn), `asleep`, `awake` or `new` (no conductor yet).
fn state(events: &[Event], id: &str, agents: &[Value]) -> &'static str {
    let lane = of(events, id).rfind(|e| e.kind.starts_with("conductor-"));
    let a = lane.and_then(|l| agents.iter().find(|a| l.data.get("agent") == Some(&a["name"])));
    match lane {
        _ if open_ask(events, id).is_some() => "choose",
        Some(l) if l.kind == "conductor-sleep" => "asleep",
        Some(_) if a.is_some_and(|a| a["agent_status"] == "working") => "working",
        Some(_) => "awake",
        None => "new",
    }
}

/// `[chat·N]`: the threads where a choice waits for the owner.
pub fn choosing(events: &[Event]) -> usize {
    let ids = events.iter().filter(|e| e.kind == "thread-new");
    ids.filter(|t| open_ask(events, t.str("thread").unwrap_or_default()).is_some())
        .count()
}

/// #413: a linked event's chip (`from t5`, `continued in t6`); a tap opens that thread (serve.js).
fn link(e: &Event) -> Option<Link> {
    conductor::link(e).map(|(w, t)| Link {
        w: w.into(),
        t: t.into(),
    })
}

/// An ask as its thread shows it: decided, the answer; else open (no `closed` event) for the choice rows.
fn ask(events: &[Event], a: &Event, m: Msg) -> Msg {
    let is = |e: &&Event| e.kind == "decision" && e.fleet == a.fleet && e.str("id") == a.str("id");
    let answer = events.iter().rfind(is).map(|d| {
        let c = d.str("choice").unwrap_or_default();
        let opts = a.data["options"].as_array().into_iter().flatten();
        Answered {
            n: (opts.filter_map(Value::as_str).zip(1..))
                .find(|o| o.0 == c)
                .map(|o| o.1),
            choice: c.into(),
            by: d.str("by").map(Into::into),
            ts: d.ts,
        }
    });
    Msg {
        open: answer.is_none() && closed(events, a).is_none(),
        routine: a.str("kind") == Some("routine"),
        ask: Some(super::v1::ask(a)),
        answer,
        ..m
    }
}

/// #408: the thread's last `n` messages before seq `before` (web/src/chat.ts draws each block, tagged `data-n`), under
/// `thread started …` or, older ones left, the marker serve.js loads them from (API v1 `thread/<t>?before=`). #407: a
/// finished turn's commands fold, each run of them one `fold` (tagged as its first command); a run after `open` (the
/// live turn's start) stays rows.
fn page(ctx: &Ctx, cfg: &Config, events: &[Event], t: &Event, (before, n): (u64, usize), open: u64) -> ThreadPage {
    let id = t.str("thread").unwrap_or_default();
    let (mut msgs, mut run, sent) = (vec![], vec![], conductor::forwarded(events));
    let fold = |msgs: &mut Vec<Msg>, run: &mut Vec<Msg>| {
        let Some((seq, ts)) = run.first().map(|f| (f.seq, f.ts)) else {
            return;
        };
        if seq > open {
            return msgs.append(run);
        }
        let cmds = run.drain(..).map(|c| c.text).collect();
        msgs.push(Msg {
            seq,
            ts,
            kind: "fold".into(),
            cmds,
            ..Msg::default()
        });
    };
    for e in of(events, id).filter(|e| e.seq < before) {
        let s = |k| e.str(k).unwrap_or_default().to_string();
        let m = |kind: &str, text: String| Msg {
            seq: e.seq,
            ts: e.ts,
            kind: kind.into(),
            text,
            link: link(e),
            ..Msg::default()
        };
        let main = cfg.hosts.iter().find(|h| h.1.machine.is_none());
        let m = match (e.kind.as_str(), e.str("from").unwrap_or_default()) {
            // #387: a signed-out line's [sign in]: the harness's login in a setup terminal on the main host (#303).
            ("thread-msg", _) if e.data.get("lane_ask").is_some() => continue,
            ("thread-msg", "system") => Msg {
                ask: change(ctx, events, e).map(super::v1::ask),
                open: change(ctx, events, e).is_some(),
                retry: e.str("line").is_some(),
                signin: e.str("signin").map(Into::into),
                host: main.map_or("local", |h| h.0.as_str()).into(),
                ..m("note", s("text"))
            },
            ("thread-msg", _) if s("text").starts_with("DECISION ") => continue,
            ("thread-handoff", _) => m("handoff", conductor::handoff_text(e)),
            ("thread-msg", f) => {
                let file = |f: &Value| FileRef {
                    name: f["name"].as_str().unwrap_or_default().into(),
                    size: f["size"].as_u64().unwrap_or(0),
                };
                let fwd = if sent.contains(&e.seq) { " (forwarded)" } else { "" };
                Msg {
                    from: f.into(),
                    files: files_of(e).map(file).collect(),
                    ..m("msg", s("text") + fwd)
                }
            }
            ("thread-cmd", _) => m("cmd", s("cmd")),
            ("needs-decision", _) => ask(events, e, m("ask", s("question"))),
            ("ask-owner", _) => {
                let Some(mut a) = conductor::lane_of(events, e).cloned() else {
                    continue;
                };
                a.data["recommend"] = e.data["recommend"].clone();
                ask(events, &a, m("ask", s("question")))
            }
            ("config-propose", _) if crate::propose::settled(events, &s("id")).is_none() => Msg {
                from: s("id"),
                open: !crate::propose::stale(ctx, e), // #460: stale, it can no longer apply
                ..m("config", s("diff"))
            },
            ("secret-request", _)
                if crate::secret::open(events, id, &s("name")).is_some_and(|o| std::ptr::eq(o, e)) =>
            {
                Msg {
                    from: s("name"),
                    ..m("secret", s("why"))
                }
            }
            ("conductor-start", _) if e.data.get("resumed") == Some(&json!(true)) => m("resumed", s("session")),
            ("thread-model", _) => m("note", conductor::picked(e)),
            ("conductor-sleep", _) if s("why") != "model" => m("sleep", String::new()),
            _ => continue,
        };
        if m.kind == "cmd" {
            run.push(m);
        } else {
            fold(&mut msgs, &mut run);
            msgs.push(m);
        }
    }
    fold(&mut msgs, &mut run);
    // #460: a waiting config card is never cut from the first page: past its window it stays right under the marker
    // (so older pages leave it out).
    let pins: Vec<u64> = crate::propose::waiting(ctx, events, id).map(|e| e.seq).collect();
    if before != u64::MAX {
        msgs.retain(|m| !pins.contains(&m.seq));
    }
    let k = msgs.len().saturating_sub(n);
    let dir = conductor::dir(ctx, cfg, id).display().to_string();
    let home = crate::config::home().display().to_string();
    ThreadPage {
        id: id.into(),
        archived: conductor::archived(events, id),
        older: (k > 0).then(|| msgs[k].seq),
        started: t.ts,
        dir: dir.strip_prefix(&home).map_or(dir.clone(), |d| format!("~{d}")),
        msgs: {
            let w = msgs.split_off(k);
            msgs.retain(|m| pins.contains(&m.seq));
            msgs.into_iter().chain(w).collect()
        },
    }
}

/// #408: API v1 `thread/<t>?before=<seq>&n=30`: the page of messages before `before`.
pub fn older(ctx: &Ctx, events: &[Event], id: &str, q: &BTreeMap<String, String>) -> Option<ThreadPage> {
    let (cfg, t) = (Config::load(&ctx.config).ok()?, conductor::find(events, id).ok()?);
    let before = q.get("before").and_then(|v| v.parse().ok()).unwrap_or(u64::MAX);
    let n = q.get("n").and_then(|v| v.parse().ok()).unwrap_or(30);
    Some(page(ctx, &cfg, events, t, (before, n), u64::MAX))
}

/// One thread: the bar (fleet · harness · model · state), its last 30 messages and the live line's state; #407:
/// working, the turn's start (`since`, serve.js times its pill), and the queued messages above the box.
fn thread(ctx: &Ctx, cfg: &Config, events: &[Event], t: &Event, agents: &[Value]) -> ThreadView {
    let id = t.str("thread").unwrap_or_default();
    let st = state(events, id, agents);
    let me = |e: &&Event| e.kind == "thread-msg" && e.str("from") == Some("owner");
    let turn = of(events, id).rfind(me).map_or((0, t.ts), |e| (e.seq, e.ts));
    let open = if st == "working" { turn.0 } else { u64::MAX };
    let q = |(q, e): (&u64, &&Event)| QueuedMsg {
        q: *q,
        text: e.str("text").unwrap_or_default().into(),
    };
    let role = conductor::thread_role(cfg, events, t).ok().map(|r| r.1);
    let role = role.as_ref();
    let (h, model) = (
        role.map_or("", |r| r.harness.as_str()),
        role.and_then(|r| r.model.as_deref()),
    );
    let shown = or(st == "choose", "awake", or(st == "new", "", st));
    ThreadView {
        id: id.into(),
        title: conductor::title(events, id).into(),
        ds: super::join(&[t.fleet.as_str(), h, model.unwrap_or_default(), shown]),
        state: st.into(),
        waiting: waiting(ctx, events, id),
        since: (st == "working").then_some(turn.1),
        queued: queued(events, id).iter().map(q).collect(),
        chips: role.map(|r| chips(cfg, r)).unwrap_or_default(),
        page: page(ctx, cfg, events, t, (u64::MAX, 30), open),
    }
}

/// #404: a chip of `name`'s choices; `cur` none: the default.
fn chip(name: &str, cur: Option<&str>, all: &[&str]) -> Chip {
    Chip {
        name: name.into(),
        cur: cur.map(Into::into),
        all: all.iter().map(ToString::to_string).collect(),
    }
}

/// #404: the thread's model and effort chips, its harness's choices (one with a single choice set is left out).
fn chips(cfg: &Config, r: &crate::config::Role) -> Vec<Chip> {
    let mut models = conductor::harness_models(cfg, &r.harness);
    if !models.is_empty() {
        models.extend(r.model.as_deref().filter(|m| !models.contains(m)));
    }
    let effort = r.effort_or(&cfg.limits);
    let all = [
        chip("model", r.model.as_deref(), &models),
        chip("effort", effort.as_deref(), crate::harness::efforts(&r.harness)),
    ];
    all.into_iter()
        .filter(|c| c.all.len() > 1 || c.cur.is_none() && !c.all.is_empty())
        .collect()
}

/// #460: the ids of the thread's config proposals that wait for the owner's tap.
fn waiting(ctx: &Ctx, events: &[Event], id: &str) -> Vec<String> {
    let w = crate::propose::waiting(ctx, events, id);
    w.filter_map(|e| e.str("id").map(Into::into)).collect()
}

/// A thread's row: unread, title, age; its state and the last message; the threads it links to (not its flow-back
/// lines), once each.
fn row(ctx: &Ctx, events: &[Event], t: &Event, agents: &[Value]) -> (u64, ThreadRow) {
    let id = t.str("thread").unwrap_or_default();
    let mut msgs = of(events, id).filter(|e| matches!(e.kind.as_str(), "thread-msg" | "needs-decision"));
    let last = msgs.rfind(|e| e.str("from") != Some("system"));
    let at = last.map_or(t.ts, |e| e.ts);
    let line = last.map_or(String::new(), |e| {
        let who = or(e.str("from") == Some("owner"), "you", "conductor");
        format!("{who}: {}", e.str("text").or(e.str("question")).unwrap_or_default())
    });
    let lk: std::collections::BTreeSet<_> = of(events, id)
        .filter_map(|e| conductor::link(e).filter(|_| e.str("from") != Some("system")))
        .collect();
    let row = ThreadRow {
        id: id.into(),
        title: conductor::title(events, id).into(),
        age: ago(ctx.now, at),
        unread: conductor::unread(events, id),
        state: state(events, id, agents).into(),
        waiting: waiting(ctx, events, id),
        links: lk
            .into_iter()
            .map(|(w, t)| Link {
                w: w.into(),
                t: t.into(),
            })
            .collect(),
        line: line.lines().next().unwrap_or_default().into(),
    };
    (at, row)
}

/// #269: API v1's `chat`, the tab (web/src/chat.ts draws it): "all repos", then each fleet by name, a card each with
/// its threads newest first (the archived ones apart); then every thread's view (serve.js shows the open one).
pub fn tab(ctx: &Ctx, cfg: Result<&Config, &str>, events: &[Event], agents: &[Value], rw: bool) -> ChatTab {
    let mut tab = ChatTab {
        now: ctx.now,
        rw,
        ..ChatTab::default()
    };
    let cfg = match cfg {
        Ok(c) => c,
        Err(e) => {
            tab.error = Some(e.into());
            return tab;
        }
    };
    let threads: Vec<&Event> = events.iter().filter(|e| e.kind == "thread-new").collect();
    let repos = std::iter::once(("all", "x")).chain(cfg.fleet.iter().map(|(n, f)| (n.as_str(), f.color(n))));
    for (repo, k) in repos {
        let (mut rows, mut arch) = (vec![], vec![]);
        for t in threads.iter().filter(|t| t.fleet == repo) {
            let on = conductor::archived(events, t.str("thread").unwrap_or_default());
            [&mut rows, &mut arch][usize::from(on)].push(row(ctx, events, t, agents));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        arch.sort_by_key(|r| std::cmp::Reverse(r.0));
        tab.repos.push(ChatRepo {
            repo: repo.into(),
            k: k.into(),
            rows: rows.into_iter().map(|r| r.1).collect(),
            archived: arch.into_iter().map(|r| r.1).collect(),
            harness: chip(
                "harness",
                conductor::defaults(cfg, repo).ok().map(|d| d.1.harness).as_deref(),
                &crate::harness::NAMES,
            ),
        });
    }
    tab.threads = threads.iter().map(|t| thread(ctx, cfg, events, t, agents)).collect();
    tab
}

impl super::Server<'_> {
    /// #269: the [chat] tab's data for a device that writes (`rw`) or only reads. #620: the fold is kept by the log's
    /// last seq, fleet.toml's mtime, the agents, `rw` and the minute; a request in between reuses it, the log unread.
    pub(super) fn chat_tab(&self, ctx: &Ctx, rw: bool) -> Value {
        let agents = self.herdr_json(ctx.now, "agent");
        let agents = agents
            .and_then(|j| j["result"]["agents"].as_array().cloned())
            .unwrap_or_default();
        let key = json!([
            crate::db::last_seq(&ctx.state),
            super::mtime(ctx),
            agents,
            rw,
            ctx.now / 60
        ]);
        let mut c = self.chat.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if c.0 != key {
            // #483: a config or log read error shows its own text (as the Queue tab does).
            let (cfg, events) = match (Config::load(&ctx.config), crate::events::log(&ctx.state)) {
                (Ok(c), Ok(e)) => (Ok(c), e),
                (Err(e), _) | (_, Err(e)) => (Err(format!("{e:#}")), std::sync::Arc::default()),
            };
            *c = (
                key,
                json!(tab(ctx, cfg.as_ref().map_err(String::as_str), &events, &agents, rw)),
            );
        }
        c.1.clone()
    }
}

/// #405: a new thread's title: its first message's first line, cut to one row.
fn first(text: &str) -> String {
    let l = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let cut: String = l.chars().take(60).collect();
    cut.replace(char::is_control, " ") + or(l.chars().count() > 60, "…", "")
}

/// The newest file named `name` a message in thread `id` carries (the owner's upload or the conductor's reply), if
/// it lies in the thread's conductor folder; `demo` (the demo export) also takes the repo's relative fixtures.
pub fn file(ctx: &Ctx, events: &[Event], id: &str, name: &str, demo: bool) -> Option<std::path::PathBuf> {
    let f = of(events, id).rev().flat_map(files_of).find(|f| f["name"] == name);
    let p = std::path::Path::new(f.and_then(|f| f["path"].as_str())?);
    let fixture = p.components().all(|c| matches!(c, std::path::Component::Normal(_)));
    if demo && fixture {
        return Some(p.into());
    }
    conductor::inside(ctx, &Config::load(&ctx.config).ok()?, id, p)
}

/// `POST /thread` from a paired owner device: `new` (#405: a first message starts a thread and its conductor), `say`
/// (#405: with files, `name`s `/`-separated, base64 `data` `,`-separated), #405's `archive` and `restore`, `answer`
/// (a choice row or the "other" box), #387's `config` ([apply]/[discard]) and `secret` ([save] with `value`, or
/// [dismiss]), `read`, or #403's `retry` (a red system line's [retry], its event `n`).
/// #403: `new`, `say` and `retry` only log; `later` gets the line `deliver` types once the reply is out.
pub fn post(
    ctx: &Ctx,
    r: &dyn Runner,
    f: &BTreeMap<String, String>,
    later: &mut Vec<(Event, String)>,
) -> Result<String> {
    let cfg = Config::load(&ctx.config)?;
    let events = crate::events::log(&ctx.state)?;
    let g = |k: &str| f.get(k).map_or("", String::as_str);
    let (id, mut out) = (g("t"), vec![]);
    match g("op") {
        // #405: `new` is a first message (in `repo`): the thread, titled with its first line, then the message.
        // #407: `queue` keeps it for the turn's end (`q`: an edit of queued message q, its files kept unless new
        // ones come); `steer` interrupts the turn (Esc) and sends it now (`q`: queued message q's text and files).
        "new" | "say" | "queue" | "steer" => {
            let mut names = g("name").split('/').map(|n| n.replace(char::is_control, ""));
            let data = g("data").split(',').filter(|d| !d.is_empty());
            let decode = |d| {
                Ok((
                    names.next().unwrap_or_default(),
                    B64.decode(d).context("a file is not base64")?,
                ))
            };
            let files = data.map(decode).collect::<Result<Vec<_>>>()?;
            let all: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
            let all = all.join(", ");
            let text = or(g("text").trim().is_empty(), &all, g("text"));
            let new = g("op") == "new";
            let id = if new {
                conductor::role(&cfg, g("repo"))?;
                conductor::text_ok(text)?;
                conductor::files_ok(&files)?;
                conductor::new(
                    ctx,
                    &cfg,
                    g("repo"),
                    &first(text),
                    Some(g("harness")).filter(|h| !h.is_empty()),
                )?
            } else {
                conductor::find(&events, id)?;
                id.to_string()
            };
            let mut files = conductor::upload(ctx, &cfg, &id, &files)?;
            let q = g("q").parse::<u64>().ok();
            let old = q
                .and_then(|q| queued(&events, &id).remove(&q))
                .context("no such queued message");
            let mut text = text.to_string();
            if g("op") == "queue" {
                conductor::text_ok(&text)?;
                let mut d = json!({"thread": id, "text": text, "files": files});
                if let Ok(o) = old {
                    d["q"] = json!(q);
                    (d["files"] == json!([])).then(|| d["files"] = json!(qfiles(o)));
                }
                let t = conductor::find(&events, &id)?;
                crate::events::append(&ctx.state, &Event::new(ctx.now, &t.fleet, None, "thread-queue", d))?;
                return Ok("queued".into());
            }
            if g("op") == "steer" {
                if q.is_some() {
                    let o = old?;
                    text = o.str("text").unwrap_or_default().into();
                    files = qfiles(o);
                    unqueue(ctx, o, "sent")?;
                }
                stop(r, &events, &id);
            }
            later.push(conductor::post(ctx, &cfg, (&id, &text, &files), ("owner", false))?);
            Ok(if new { format!("{id} started") } else { "sent".into() })
        }
        "unqueue" => {
            let q = g("q").parse::<u64>().unwrap_or_default();
            unqueue(
                ctx,
                queued(&events, id).remove(&q).context("no such queued message")?,
                "removed",
            )?;
            Ok("removed".into())
        }
        "model" => {
            conductor::pick(ctx, &cfg, id, g("model"), g("effort"))?;
            Ok("picked".into())
        }
        "stop" => {
            conductor::find(&events, id)?;
            stop(r, &events, id);
            Ok("stopping".into())
        }
        "archive" | "restore" => {
            conductor::archive(ctx, &cfg, r, id, g("op") == "archive")?;
            Ok(format!("{id} {}d", g("op")))
        }
        "retry" => {
            let e = of(&events, id).find(|e| e.seq.to_string() == g("n"));
            let line = e.and_then(|e| e.str("line")).context("nothing to retry")?;
            later.push((conductor::find(&events, id)?.clone(), line.into()));
            Ok("retrying".into())
        }
        "answer" => {
            let (choice, by) = (
                or(g("choice").is_empty(), g("text"), g("choice")),
                ("owner", Some("web")),
            );
            crate::answer(ctx, &cfg, r, &mut out, (g("fleet"), g("id")), choice, by)?;
            Ok(format!("answered {}", g("id")))
        }
        "config" => crate::propose::settle(ctx, &cfg, r, g("id"), g("act") == "apply"),
        "secret" => crate::secret::settle(ctx, &cfg, r, (id, g("name")), (g("act") == "save").then(|| g("value"))),
        "read" => {
            conductor::mark_read(ctx, &cfg, &events, conductor::find(&events, id)?)?;
            Ok(String::new())
        }
        op => bail!("unknown thread op `{op}`"),
    }
}

/// #407: queued message `e` leaves the queue: `sent` or `removed`.
fn unqueue(ctx: &Ctx, e: &Event, why: &str) -> Result<()> {
    let d = json!({"thread": e.str("thread"), "q": qid(e), "why": why});
    crate::events::append(&ctx.state, &Event::new(ctx.now, &e.fleet, None, "thread-unqueue", d))
}

/// #407: Esc into the thread's live conductor: its turn stops (an asleep one has none).
fn stop(r: &dyn Runner, events: &[Event], id: &str) {
    let last = of(events, id).rfind(|e| e.kind.starts_with("conductor-"));
    if let Some(p) = last.filter(|e| e.kind == "conductor-start").and_then(|e| e.str("pane")) {
        drop(crate::herdr::keys(r, p, "Escape"));
    }
}

/// #407: at a turn's end (the conductor no longer working) its queued messages go in, oldest first, each logged as
/// the owner's and unqueued; the lines for `deliver`.
pub fn flush(ctx: &Ctx, agents: &[Value]) -> Vec<(Event, String)> {
    let (Ok(cfg), Ok(events)) = (Config::load(&ctx.config), crate::events::log(&ctx.state)) else {
        return vec![];
    };
    let mut out = vec![];
    for t in events.iter().filter(|e| e.kind == "thread-new") {
        let id = t.str("thread").unwrap_or_default();
        for e in queued(&events, id)
            .values()
            .take_while(|_| state(&events, id, agents) != "working")
        {
            let files = qfiles(e);
            let sent = unqueue(ctx, e, "sent");
            let p = sent.and_then(|()| {
                conductor::post(
                    ctx,
                    &cfg,
                    (id, e.str("text").unwrap_or_default(), &files),
                    ("owner", false),
                )
            });
            out.extend(p.ok());
        }
    }
    out
}

/// #403: type a logged message into its conductor (after the reply went out); a failure is a red system line whose
/// [retry] types it again.
pub fn deliver(ctx: &Ctx, r: &dyn Runner, (t, line): &(Event, String)) {
    let Ok(cfg) = Config::load(&ctx.config) else { return };
    if let Err(e) = conductor::deliver(ctx, &cfg, r, t, line) {
        let text = format!("✗ not delivered: {e:#}");
        let d = json!({"thread": t.str("thread"), "from": "system", "text": text, "line": line});
        drop(crate::events::emit(
            &ctx.state,
            &cfg,
            Event::new(ctx.now, &t.fleet, None, "thread-msg", d),
        ));
    }
}
