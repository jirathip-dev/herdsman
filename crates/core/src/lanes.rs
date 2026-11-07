//! The page's Lanes tab (docs/serve.md): herdr agents joined with the log, grouped, plus masked pane text.
use crate::config::Config;
use crate::events::{Event, States};
use crate::harness;
use crate::runner::{ok, Runner};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Lane {
    /// The registered fleet the agent works for; None = Others.
    pub fleet: Option<String>,
    /// Agent name, or the workspace label for an unnamed agent outside any fleet.
    pub name: String,
    pub issue: Option<u64>,
    /// impl | rev | design | conductor, or `other` when herdsman did not spawn it.
    pub role: String,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    pub port: Option<String>,
    /// herdr's live state (idle | working | done | blocked | unknown), or `dead` | `parked` (log only, no pane).
    pub state: String,
    /// When herdsman spawned it (0: unknown).
    pub since: u64,
    pub pane: Option<String>,
    /// #83: a log-only lane whose worktree is clean and pushed: its short head sha.
    pub parked: Option<String>,
    /// #157: the `[hosts]` host it runs on, when fleet.toml has hosts, and its herdr machine (none: this one).
    pub host: Option<String>,
    pub machine: Option<String>,
    /// #248: why `check` keeps this done lane's session open (its newest `lane-kept-dirty`, none once closed).
    pub kept: Option<String>,
    /// #260: its worktree, from the spawn.
    pub worktree: Option<String>,
    /// #302: `on <entry> (<entries> resting until 18:40)` when a `fallback` entry carries on.
    pub resting: Option<String>,
    /// #422: a conductor's chat thread (`t6`); with no pane it is `asleep`.
    pub thread: Option<String>,
}

/// Join `herdr agent list` rows with the log. `ws` maps workspace id -> label.
pub fn join(
    cfg: &Config,
    r: &dyn Runner,
    worktrees: &Path,
    events: &[Event],
    st: &States,
    agents: &[Value],
    ws: &BTreeMap<String, String>,
) -> Vec<Lane> {
    // #632: each agent's and pane's newest spawn, indexed once.
    let (mut by_name, mut by_pane) = (BTreeMap::new(), BTreeMap::new());
    for e in events.iter().filter(|e| e.kind == "spawn") {
        by_name.insert(e.str("agent"), e);
        by_pane.insert(e.str("pane"), e);
    }
    let (mut out, mut seen) = (vec![], BTreeSet::new());
    // #422: each thread's last `conductor-start`, by its agent (`conductor-<thread>`).
    let starts = events.iter().filter(|e| e.kind == "conductor-start");
    let mut cs: BTreeMap<&str, &Event> = starts.filter_map(|e| Some((e.str("agent")?, e))).collect();
    for a in agents {
        let s = |k: &str| a[k].as_str().filter(|v| !v.is_empty());
        let (name, pane) = (s("name"), s("pane_id"));
        // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
        let sp = match (name, pane) {
            (Some(_), _) => by_name.get(&name),
            (None, Some(_)) => by_pane.get(&pane),
            _ => None,
        }
        .copied();
        let mut l = Lane {
            state: s("agent_status").unwrap_or("unknown").into(),
            pane: pane.map(String::from),
            host: s("host").map(String::from),
            machine: s("machine").map(String::from),
            ..Lane::default()
        };
        if let Some(e) = sp {
            let name = e.str("agent").unwrap_or_default();
            l = Lane {
                name: name.into(),
                ..from_spawn(e, l)
            };
            // #516: one lane per agent: a second live row for it (herdr's and shed's, a remote host's) folds into
            // the first, the busier state (blocked, then working) kept.
            if !seen.insert(name.to_string()) {
                let rank = |s: &str| SECTIONS.iter().position(|x| *x == s).unwrap_or(2);
                if let Some(o) = out.iter_mut().find(|o: &&mut Lane| o.name == name) {
                    if rank(&l.state) < rank(&o.state) {
                        *o = l;
                    }
                }
                continue;
            }
        } else if let Some(e) = name.and_then(|n| cs.remove(n)) {
            l = conductor(events, e, l);
        } else {
            let inside = |d: &Path| {
                ["cwd", "foreground_cwd"]
                    .iter()
                    .any(|k| s(k).is_some_and(|c| Path::new(c).starts_with(d)))
            };
            let fleet = cfg
                .fleet
                .iter()
                .find(|(_, f)| inside(&f.local_path()) || inside(&worktrees.join(f.repo_name())));
            l.fleet = fleet.map(|f| f.0.clone());
            l.role = "other".into();
            l.harness = s("agent").unwrap_or("shell").trim_end_matches("-tui").into();
            let label = s("workspace_id").map(|w| ws.get(w).cloned().unwrap_or_else(|| w.into()));
            l.name = name.map(String::from).or(label).unwrap_or_else(|| "?".into());
        }
        out.push(l);
    }
    for s in st.values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        let Some((_, agent)) = s.active_lane().filter(|(_, a)| !seen.contains(*a)) else {
            continue;
        };
        if let Some(e) = by_name.get(&Some(agent)) {
            let r = &crate::hosts::On::of(r, Some(e));
            let wt = e
                .str("worktree")
                .filter(|wt| crate::spawn::dirty(r, Path::new(wt)).is_none());
            let head = wt.map(|wt| ok(r, "git", &["-C", wt, "rev-parse", "--short=7", "HEAD"]).unwrap_or_default());
            let state = if head.is_some() { "parked" } else { "dead" }.into();
            out.push(Lane {
                name: agent.into(),
                ..from_spawn(
                    e,
                    Lane {
                        state,
                        parked: head,
                        ..Lane::default()
                    },
                )
            });
        }
    }
    out.extend(cs.into_values().map(|e| conductor(events, e, Lane::default())));
    for l in &mut out {
        let mark = |e: &&Event| matches!(e.kind.as_str(), "spawn" | "lane-closed" | "lane-kept-dirty");
        let last = events.iter().rev().find(|e| mark(e) && e.str("agent") == Some(&l.name));
        let why = last.filter(|e| e.kind == "lane-kept-dirty").and_then(|e| e.str("why"));
        l.kept = why.filter(|_| l.state == "done").map(String::from);
    }
    out
}

impl Lane {
    /// #157: `r` on the lane's host.
    pub fn on<'a>(&'a self, r: &'a dyn Runner) -> crate::hosts::On<'a> {
        crate::hosts::On::new(r, self.machine.as_deref())
    }
    /// #260: a local lane's newest session transcript, when its harness keeps one (`Harness::transcript`).
    pub fn session(&self) -> Option<std::path::PathBuf> {
        let wt = self.worktree.as_ref().filter(|_| self.machine.is_none())?;
        harness::get(&self.harness).transcript(&crate::config::home(), wt)
    }
}

/// The newest session transcript any harness keeps for worktree `wt` under `home`.
pub fn session_file(home: &Path, wt: &str) -> Option<std::path::PathBuf> {
    harness::all().find_map(|h| h.1.transcript(home, wt))
}

fn from_spawn(e: &Event, l: Lane) -> Lane {
    let s = |k: &str| e.str(k).map(String::from);
    Lane {
        fleet: Some(e.fleet.clone()),
        issue: e.issue,
        role: s("role").unwrap_or_default(),
        harness: s("harness").unwrap_or_default(),
        model: s("model"),
        effort: s("effort"),
        account: s("account"),
        port: e.data.get("port").and_then(Value::as_u64).map(|p| format!(":{p}")),
        since: e.ts,
        host: s("host").or(l.host),
        machine: s("machine").or(l.machine),
        worktree: s("worktree"),
        resting: s("resting"),
        ..l
    }
}

/// #422: the conductor `e` started: role `conductor`, named `<thread title> · <thread>`, `asleep` with no pane.
fn conductor(events: &[Event], e: &Event, l: Lane) -> Lane {
    let (id, mut l) = (e.str("thread").unwrap_or_default(), from_spawn(e, l));
    let t = (events.iter()).find(|t| t.kind == "thread-new" && t.str("thread") == Some(id));
    l.name = format!("{} · {id}", t.and_then(|t| t.str("title")).unwrap_or_default());
    (l.role, l.thread) = ("conductor".into(), Some(id.into()));
    l.state = l.pane.as_ref().map_or("asleep".into(), |_| l.state);
    l
}

/// Section order on the page; any other herdr state (unknown) counts as idle.
pub const SECTIONS: &[&str] = &["blocked", "working", "idle", "done", "parked", "dead"];

pub type Groups<'a> = Vec<(&'static str, Vec<(String, Vec<&'a Lane>)>)>;

/// Non-empty [`SECTIONS`], each split by fleet (alphabetical, Others last), newest `changed` first.
pub fn group(all: &[Lane], changed: impl Fn(&Lane) -> u64) -> Groups<'_> {
    let section = |l: &Lane| SECTIONS.iter().position(|s| *s == l.state).unwrap_or(2);
    let mut v: Vec<&Lane> = all.iter().collect();
    v.sort_by_key(|l| {
        (
            section(l),
            l.fleet.is_none(),
            l.fleet.clone(),
            l.role != "conductor", // #422: a repo's conductors first
            std::cmp::Reverse(changed(l)),
        )
    });
    let mut out: Groups = vec![];
    for l in v {
        let f = (l.fleet.as_deref()).map_or("Others", |f| if f == "all" { "all repos" } else { f }); // #422
        let (s, f) = (SECTIONS[section(l)], f.to_string());
        if out.last().is_none_or(|x| x.0 != s) {
            out.push((s, vec![]));
        }
        let subs = &mut out.last_mut().unwrap().1;
        if subs.last().is_none_or(|x| x.0 != f) {
            subs.push((f, vec![]));
        }
        subs.last_mut().unwrap().1.push(l);
    }
    out
}

const HIDE: &str = "•••";
pub const PREFIXES: &[&str] = &["sk-", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "xox", "xapp-"];

/// `l` with each value `secret` finds (`SECRET_IN_WIP`'s check) as `•••`.
pub fn redact(l: &str) -> String {
    let mut m = l.to_owned();
    while let Some(v) = secret(&m) {
        m = m.replace(v, HIDE);
    }
    m
}

/// The push guards' check (stricter than [`mask`]): the secret on line `l`, if any — a `PREFIXES`/AWS/Google token,
/// a JWT, a PEM private key, or a high-entropy non-code value assigned to a `*KEY|TOKEN|SECRET|PASSWORD` name.
#[allow(clippy::cast_precision_loss)] // char counts of one line
pub fn secret(l: &str) -> Option<&str> {
    let has = |s: &str, f: fn(&u8) -> bool| s.as_bytes().iter().any(f);
    let pre = |w: &str, p: &str| w.starts_with(p) && w.len() >= p.len() + 16 && has(&w[p.len()..], u8::is_ascii_digit);
    let b64 = |s: &str| !s.is_empty() && !has(s, |b| !b.is_ascii_alphanumeric() && !b"-_".contains(b));
    let jwt = |w: &str| w.starts_with("eyJ") && w.split('.').count() == 3 && w.split('.').all(b64);
    let mut words = l.split(|c: char| !c.is_ascii_alphanumeric() && !"-_.".contains(c));
    let pem = l.contains("-----BEGIN") && l.contains("PRIVATE KEY-----");
    let tok = words.find(|w| jwt(w) || PREFIXES.iter().chain(&["AKIA", "ASIA", "AIza"]).any(|p| pre(w, p)));
    // A Telegram bot token (bare, `bot<id>:` or in an API URL): the 8+ digit id right before `:AA`, then 28+ chars.
    let tg = l.split([' ', '"', '\'', '=', ',', '`', '/']).find_map(|w| {
        let (d, t) = w.split_once(":AA")?;
        let id = d.bytes().rev().take_while(u8::is_ascii_digit).count();
        (id > 7 && t.len() > 27).then(|| &w[d.len() - id..])
    });
    // The name is the word before this `=`/`:` or before the line's first one (`const API_KEY: &str = "…"`).
    let named = |s: &str| {
        let up = s.split_whitespace().last().unwrap_or("").to_uppercase();
        ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|k| up.contains(k))
    };
    let first = &l[..l.find(['=', ':']).unwrap_or(0)];
    tok.or(tg).or(pem.then_some(l.trim())).or_else(|| {
        // `NAME = value` or `NAME: value` (JSON/YAML): the quoted value, else the first whitespace-delimited token.
        l.match_indices(['=', ':']).find_map(|(i, _)| {
            let r = l[i + 1..].trim_start().trim_start_matches(['"', '\'']);
            let bearer = r.starts_with("Bearer ");
            let v = r
                .trim_start_matches("Bearer ")
                .split(['"', '\'', ' ', '\t'])
                .next()?
                .trim_end_matches([';', ',']);
            let n = v.chars().count() as f64;
            let bits: f64 = v.chars().map(|c| -(v.matches(c).count() as f64 / n).log2() / n).sum();
            // Words, `snake_case`/`camelCase` identifiers, code and paths are never secrets.
            let word = !has(v, u8::is_ascii_digit) || v.contains('_') && v == v.to_ascii_lowercase();
            let code = v.contains(['(', '{', ' ']) || v.contains("::") || v.starts_with(['/', '~', '.', '$']);
            ((named(&l[..i]) || named(first) || bearer) && n >= 20.0 && bits >= 3.5 && !word && !code).then_some(v)
        })
    })
}

/// Mask secret-looking runs: `PREFIXES` tokens, JWTs, 32+ char hex/base64, `*KEY=`/`*TOKEN=`/`*SECRET=` values.
pub fn mask(s: &str) -> String {
    let tok = |c: char| c.is_ascii_alphanumeric() || "-_+/=.".contains(c);
    let (mut out, mut rest, mut hide_next) = (String::new(), s, false);
    while let Some(i) = rest.find(tok) {
        out += &rest[..i];
        let n = rest[i..].find(|c| !tok(c)).unwrap_or(rest.len() - i);
        let w = &rest[i..i + n];
        let (word, after_eq) = if std::mem::take(&mut hide_next) {
            (HIDE.to_string(), false)
        } else {
            word(w)
        };
        out += &word;
        // `TOKEN="value"`: the value is the next run.
        hide_next = after_eq && rest[i + n..].starts_with(['"', '\'']);
        rest = &rest[i + n..];
    }
    out + rest
}

/// One run of token chars, masked; true when it ends in a secret-named `KEY=`.
fn word(w: &str) -> (String, bool) {
    if let Some((k, v)) = w.split_once('=') {
        let up = k.to_ascii_uppercase();
        if ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|s| up.contains(s)) {
            return if v.is_empty() {
                (w.into(), true)
            } else {
                (format!("{k}={HIDE}"), false)
            };
        }
    }
    if let Some(p) = PREFIXES.iter().find(|p| w.starts_with(**p) && w.len() >= p.len() + 10) {
        return (format!("{p}{HIDE}"), false);
    }
    let jwt = w.starts_with("eyJ") && w.matches('.').count() >= 2;
    let long = |seg: &str| {
        seg.len() >= 32 && seg.chars().any(|c| c.is_ascii_digit()) && seg.chars().any(|c| c.is_ascii_alphabetic())
    };
    if jwt || w.split(['.', '/', '-']).any(long) {
        return (HIDE.into(), false);
    }
    (w.into(), false)
}

/// #105, one set for every harness: ≥ 20 rule glyphs in a row make a divider; detail glyphs open a status's lines.
const GLYPHS: &[&str] = &["─━═╌┄▔▁", "⎿┊│"];

/// Box-drawing rules, prompts and status bars: no activity there.
pub fn chrome(l: &str) -> bool {
    l.chars().all(|c| "─━│┃╭╮╰╯┌┐└┘═╌┄▔▁ ".contains(c)) || l.starts_with(['❯', '⏵', '>', '›'])
}

/// A screen's transcript and live status. Not transcript: the frame from the second-last divider down, and the line
/// above it opening with a glyph and holding a timer (`(25m 52s`: a spinner, a status bar) plus blank/detail lines.
/// #138: an ANSI screen parses on its text (SGR codes dropped) and keeps its codes.
pub fn chat(screen: &str) -> (Vec<&str>, Option<&str>) {
    let lines: Vec<&str> = screen.lines().collect();
    let text: Vec<String> = lines.iter().map(|l| strip(l)).collect();
    let rule = |l: &str| l.split(|c| !GLYPHS[0].contains(c)).any(|r| r.chars().count() >= 20);
    let rules: Vec<usize> = (0..lines.len()).filter(|&i| rule(&text[i])).collect();
    let cut = rules.len().checked_sub(2).map_or(lines.len(), |i| rules[i]);
    let after = |l: &str| {
        let (t, pad) = (l.trim(), l.len() - l.trim_start().len());
        t.is_empty() || pad >= 20 || pad > 0 && t.starts_with(|c| GLYPHS[1].contains(c))
    };
    let timer = |w: &str| matches!(w.strip_suffix(['h', 'm', 's']).map(str::parse::<u32>), Some(Ok(_)));
    let live = |t: &str| t.starts_with(|c: char| !c.is_alphanumeric()) && t.split(['(', ' ', ')']).any(timer);
    let i = text[..cut].iter().rposition(|l| !after(l));
    let status = i.filter(|&i| live(text[i].trim()));
    let body = (0..status.unwrap_or(cut))
        .filter(|&i| !rule(&text[i]))
        .map(|i| lines[i]);
    (body.collect(), status.map(|i| lines[i].trim()))
}

/// `f(code, text)` per run of an ANSI line: the first run has no code, every later one its ESC-led CSI code.
fn sgr(l: &str, f: impl Fn(&str, &str) -> String) -> String {
    let mut parts = l.split('\x1b');
    let first = f("", parts.next().unwrap_or_default());
    let run = |p: &str| {
        let (c, t) = p.split_at(p.find(|c: char| c.is_ascii_alphabetic()).map_or(0, |j| j + 1));
        f(&format!("\x1b{c}"), t)
    };
    first + &parts.map(run).collect::<String>()
}

/// An ANSI line's text.
pub fn strip(l: &str) -> String {
    sgr(l, |_, t| t.into())
}

/// #138: a screen's transcript lines, masked run by run (an SGR code never glues onto a secret).
pub fn body(screen: &str) -> Vec<String> {
    let lines = chat(screen).0.into_iter();
    lines
        .map(|l| sgr(l.trim_end(), |c, t| format!("{c}{}", mask(t))))
        .collect()
}

/// #138: a lane's rolling transcript, `<state dir>/transcripts/<agent>.log`; None for a name that is no plain file name.
pub fn log(state: &Path, agent: &str) -> Option<std::path::PathBuf> {
    let plain = !agent.starts_with('.') && agent.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    plain.then(|| state.with_file_name("transcripts").join(format!("{agent}.log")))
}

/// #248: a closed lane's saved session (its last 2 MB), the newest `.jsonl` for worktree `wt` (`session_file`),
/// as sheet lines: `❯` prompts, `⏺` replies and tool calls (`Bash(cargo test)`), masked.
pub fn session(home: &Path, wt: &str) -> Option<Vec<String>> {
    let file = session_file(home, wt)?;
    let part = |p: &Value| match p["type"].as_str()? {
        "text" => p["text"].as_str().map(String::from),
        "tool_use" => {
            let arg = p["input"].as_object()?.values().find_map(Value::as_str);
            let arg = arg.and_then(|a| a.lines().next()).unwrap_or_default();
            Some(format!("{}({arg})", p["name"].as_str()?))
        }
        _ => None,
    };
    let (text, mut out) = (read_tail(&file, 1 << 21)?, vec![]);
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let mark = ["❯ ", "⏺ "][usize::from(v["type"] == "assistant")];
        let c = &v["message"]["content"];
        let parts: Vec<String> = match c.as_str() {
            Some(t) if v["type"] == "user" => vec![t.into()],
            _ => c.as_array().into_iter().flatten().filter_map(part).collect(),
        };
        let lines = parts.iter().flat_map(|t| t.lines().enumerate());
        out.extend(lines.map(|(i, l)| mask(&format!("{}{l}", if i == 0 { mark } else { "  " }))));
    }
    Some(out)
}

/// The line where two reads had no overlap: the lane scrolled past between them.
pub const GAP: &str = "┄ not captured: scrolled past between reads ┄";

/// #138: append a read's settled lines (above its last block head, which a re-render still changes) to `file` past
/// their longest overlap with its tail (compared as text), else after a `GAP` line; the last 5,000 lines, mode 0600.
/// The file's lines plus the live block, and the live block's length.
pub fn roll(file: &Path, body: &[String]) -> (Vec<String>, usize) {
    let head = |l: &String| strip(l).trim_start().starts_with(|c| "⏺❯›>$•┊💻".contains(c));
    let (new, live) = body.split_at(body.iter().rposition(head).unwrap_or(0));
    let mut all: Vec<String> = std::fs::read_to_string(file).map_or(vec![], |s| s.lines().map(String::from).collect());
    let same = |a: &[String], b: &[String]| a.iter().zip(b).all(|(x, y)| strip(x) == strip(y));
    let m = new.len().min(all.len());
    let k = (0..=m).rfind(|&k| same(&all[all.len() - k..], &new[..k])).unwrap_or(0);
    if k < new.len() {
        if k == 0 && !all.is_empty() {
            all.push(GAP.into());
        }
        all.extend_from_slice(&new[k..]);
        all.drain(..all.len().saturating_sub(5000));
        let _ = crate::devices::write0600(file, (all.join("\n") + "\n").as_bytes());
    }
    all.extend_from_slice(live);
    (all, live.len())
}

/// The live status line, else the latest action (`Harness::now`), masked, ≤ 60 chars.
pub fn activity(harness: &str, screen: &str) -> String {
    let (body, status) = chat(screen);
    let lines: Vec<&str> = body.into_iter().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = lines.into_iter().rev().find(|l| harness::get(harness).now_line(l));
    let m = mask(status.or(line).unwrap_or_default());
    if m.chars().count() > 60 {
        m.chars().take(59).collect::<String>() + "…"
    } else {
        m
    }
}

/// #260: a tool call as a masked `now:` line (the row ellipsises it): `running <cmd>`, `editing <file>`, …, else the tool.
fn doing(tool: &str, arg: &str) -> String {
    let rows = "running Bash Shell $|editing Edit Write Update MultiEdit|reading Read".split('|');
    let row = rows.into_iter().find(|r| r.split(' ').skip(1).any(|w| w == tool));
    let mut first = arg.lines().next().unwrap_or_default().to_string();
    if let Some(h) = std::env::var("HOME").ok().filter(|h| h.len() > 1) {
        first = first.replace(&h, "~"); // #276
    }
    mask(&format!("now: {} {}", row.map_or(tool, |r| &r[..7]), first.trim()))
}

/// #276: a shell command as its first real command and target (`git merge origin/staging`, `sed src/usage.rs`): `cd`,
/// `VAR=…`, redirections and piped parts dropped, `+N` for the rest of a chain, a heredoc as `python3 (script)`.
pub fn short(cmd: &str) -> String {
    let (mut segs, mut seg, mut tok, mut q, mut d, mut piped) = (vec![], vec![], String::new(), None, 0, false);
    let mut cs = cmd.chars().chain([';']).peekable();
    while let Some(c) = cs.next() {
        match (q, c) {
            (Some(x), _) if x == c => q = None,
            (None, '\'' | '"') => q = Some(c),
            (None, '(' | ')') => d += if c == '(' { 1 } else { -1 },
            (None, '&') if tok.ends_with('>') || cs.peek() == Some(&'>') => {}
            (None, ';' | '\n' | '&' | '|' | ' ' | '\t') if d <= 0 => {
                seg.extend((!tok.is_empty()).then(|| std::mem::take(&mut tok)));
                if !" \t".contains(c) {
                    let pipe = c == '|' && cs.next_if_eq(&'|').is_none();
                    segs.push((std::mem::take(&mut seg), std::mem::replace(&mut piped, pipe)));
                }
                continue;
            }
            _ => {}
        }
        tok.push(c);
    }
    let id = |c: char| c == '_' || c.is_alphanumeric();
    let var = |w: &&String| w.split_once('=').is_some_and(|(k, _)| k.chars().all(id));
    let real: Vec<Vec<&String>> = (segs.iter().filter(|s| !s.1))
        .map(|s| s.0.iter().skip_while(var).collect::<Vec<_>>())
        .filter(|s| s.first().is_some_and(|c| *c != "cd"))
        .collect();
    let Some(s) = real.first() else {
        return cmd.trim().to_string();
    };
    let word = |w: &&&String| !w.starts_with('-') && w.chars().all(|c| c.is_alphanumeric() || "./~:@_+-".contains(c));
    let redir = |w: &&String| ["<", ">", ">>", "2>", "&>"].contains(&w.as_str());
    let args = s.iter().zip(&s[1..]).filter(|(p, _)| !redir(p)).map(|x| x.1);
    let args: Vec<_> = args.filter(word).take(2).collect();
    let end = args
        .iter()
        .position(|w| w.contains(['/', '.']))
        .map_or(args.len(), |i| i + 1);
    let out = args[..end].iter().fold(s[0].clone(), |o, w| o + " " + w);
    let (more, script) = (real.len() - 1, s.iter().any(|w| w.starts_with("<<")));
    let tail = [(script, " (script)".to_string()), (more > 0, format!(" +{more}"))];
    out + &tail.into_iter().filter(|t| t.0).map(|t| t.1).collect::<String>()
}

/// #276: the live spinner's word and elapsed time (`Harness::status_word`).
pub fn spin(harness: &str, screen: &str) -> Option<(String, Option<String>)> {
    harness::get(harness).status_word(screen)
}

/// #260: the pane's last tool line, as any harness reads it (`Harness::tool_line`), as a `now:` line.
pub fn now(screen: &str) -> Option<String> {
    chat(screen).0.iter().rev().find_map(|l| {
        let t = strip(l);
        let (tool, arg) = harness::all().find_map(|h| h.1.tool_line(t.trim()))?;
        Some(doing(&tool, &arg))
    })
}

/// A blocked pane's pending prompt, as any harness reads it (`Harness::pending_menu`).
pub fn pending(screen: &str) -> Option<(String, Vec<String>)> {
    harness::all().find_map(|h| h.1.pending_menu(screen))
}

/// #362: a transcript's cached tail: its inode, the offset read up to (past its last full line), the 64 bytes before
/// it, and the text read (whole lines, its last 2 MB).
#[derive(Default)]
pub struct Tail {
    ino: u64,
    offset: u64,
    last: Vec<u8>,
    pub text: String,
}

impl Tail {
    /// #362: bring the tail up to a file of inode `ino` and `len` bytes, reading only `[offset, len)` while the inode
    /// holds, the file did not shrink and its 64 bytes before `offset` are unchanged; else its last 2 MB. Returns the
    /// unfinished last line, read again next time.
    pub fn read(&mut self, f: &mut (impl Read + Seek), ino: u64, len: u64) -> std::io::Result<String> {
        let mut seen = vec![0; self.last.len()];
        let at = SeekFrom::Start(self.offset - seen.len() as u64);
        let same = self.ino == ino && len >= self.offset && f.seek(at).and_then(|_| f.read_exact(&mut seen)).is_ok();
        if !same || seen != self.last {
            (self.ino, self.offset, self.last, self.text) = (ino, len.saturating_sub(1 << 21), vec![], String::new());
        }
        let mut b = vec![];
        f.seek(SeekFrom::Start(self.offset))?;
        f.take(len - self.offset).read_to_end(&mut b)?;
        let n = b.iter().rposition(|&c| c == b'\n').map_or(0, |i| i + 1);
        self.offset += n as u64;
        self.last.extend_from_slice(&b[..n]);
        self.last.drain(..self.last.len().saturating_sub(64));
        self.text.push_str(&String::from_utf8_lossy(&b[..n]));
        self.text.drain(..line_from(&self.text, 1 << 21));
        Ok(String::from_utf8_lossy(&b[n..]).into_owned())
    }
}

/// Where the last `n` bytes of `s` start, moved up to a line start.
fn line_from(s: &str, n: usize) -> usize {
    match s.len().saturating_sub(n) {
        0 => 0,
        cut => (s.as_bytes()[cut - 1..].iter().position(|&c| c == b'\n')).map_or(s.len(), |j| cut + j),
    }
}

/// #260: a file's last `n` (≤ 2 MB) bytes as text, whole lines (a session transcript is tens of MB). #362: read
/// incrementally through a `Tail` per file, the 32 most recently read kept.
pub fn read_tail(p: &Path, n: u64) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    static CACHE: std::sync::Mutex<Vec<(std::path::PathBuf, Tail)>> = std::sync::Mutex::new(vec![]);
    let mut f = std::fs::File::open(p).ok()?;
    let mut cache = CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (i, m) = (cache.iter().position(|e| e.0 == p), f.metadata().ok()?);
    let mut t = i.map_or_else(Tail::default, |i| cache.remove(i).1);
    let part = t.read(&mut f, m.ino(), m.len()).ok()?;
    let out = t.text[line_from(&t.text, usize::try_from(n).unwrap_or(usize::MAX))..].to_string() + &part;
    cache.insert(0, (p.into(), t));
    cache.truncate(32);
    Some(out)
}

/// #260: a lane row's `now:` line: its session transcript's current tool call (in its last 256 KB), else the pane's.
pub fn now_of(l: &Lane, screen: &str) -> Option<String> {
    let tail = l.session().and_then(|p| read_tail(&p, 1 << 18));
    tail.map_or_else(|| now(screen), |t| said(&t).1)
}

/// #260: a Claude session transcript (`.jsonl`): its prompts and each turn's final text reply as `{k: me | ai, text, at}`
/// (masked; meta lines, tool results and `<…>` wrappers dropped), and its last tool call as a `now:` line (none once text
/// follows it: the turn replied).
pub fn said(jsonl: &str) -> (Vec<Value>, Option<String>) {
    let (mut out, mut now, keys) = (Vec::<Value>::new(), None, ["command", "file_path", "path", "pattern"]);
    for v in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (c, k) = (&v["message"]["content"], if v["type"] == "user" { "me" } else { "ai" });
        if v["isMeta"] == true || !["user", "assistant"].contains(&v["type"].as_str().unwrap_or_default()) {
            continue;
        }
        let one = vec![json!({"type": "text", "text": c})];
        for p in c.as_array().cloned().unwrap_or(one) {
            let t = p["text"].as_str().map(str::trim).unwrap_or_default();
            if p["type"] == "tool_use" {
                let arg = keys.iter().find_map(|k| p["input"][k].as_str()).unwrap_or_default();
                now = Some(doing(p["name"].as_str().unwrap_or_default(), arg));
            } else if p["type"] == "text" && !t.is_empty() && !t.starts_with('<') {
                if k == "ai" && out.last().is_some_and(|l| l["k"] == "ai") {
                    out.pop();
                }
                now = None;
                out.push(json!({"k": k, "text": mask(t), "at": v["timestamp"]}));
            }
        }
    }
    (out, now)
}

/// The pane's last `n` non-empty lines, masked.
pub fn tail(screen: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| mask(l.trim_end()))
        .collect()
}

/// #127: menu (`Harness::pending_menu`)'s card from its block (after the last divider above the question): `title` its first line, `command`
/// the `│` lines but those opening with a word (`Dangerous rm …`), `countdown` from `in M:SS`, the rest `reason`.
pub fn prompt(screen: &str) -> Option<Value> {
    let (question, options) = pending(screen).filter(|p| !p.1.is_empty())?;
    let lines: Vec<&str> = screen.lines().map(str::trim).collect();
    let q = lines.iter().rposition(|l| l.contains(question.as_str()))?;
    let rule = |l: &&str| l.split(|c| !GLYPHS[0].contains(c)).any(|r| r.chars().count() >= 20);
    let bare = |l: &&str| mask(l.trim_matches(|c: char| c.is_whitespace() || c == '│'));
    let top = lines[..q].iter().rposition(rule).map_or(0, |i| i + 1);
    let mut block = lines[top..q].iter().filter(|l| !bare(l).is_empty());
    let (title, mut cmd, mut why, mut cd) = (bare(block.next()?), vec![], vec![], None);
    let mm_ss = |(m, s): (&str, &str)| Some(m.parse::<u64>().ok()? * 60 + s.get(..2)?.parse::<u64>().ok()?);
    let prose = |l: &str| l.starts_with(char::is_uppercase) && l.chars().nth(1).is_some_and(char::is_lowercase);
    for l in block {
        match l.split(" in ").skip(1).find_map(|s| mm_ss(s.split_once(':')?)) {
            Some(s) if cd.is_none() => cd = Some(s),
            _ if l.starts_with('│') && !prose(&bare(l)) => cmd.push(bare(l)),
            _ => why.push(bare(l)),
        }
    }
    why.truncate(2);
    let card = json!({"title": title, "reason": why.join(" "), "command": cmd.join("\n"), "countdown": cd,
        "question": mask(&question), "options": options, "push": cd.is_none_or(|s| s == 0)});
    Some(card)
}
