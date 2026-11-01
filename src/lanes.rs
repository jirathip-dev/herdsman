//! The page's Lanes tab (docs/serve.md): herdr agents joined with the log, grouped, plus masked pane text.
use crate::config::Config;
use crate::events::{Event, States};
use crate::runner::{ok, Runner};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Lane {
    /// The registered fleet the agent works for; None = Others.
    pub fleet: Option<String>,
    /// Agent name, or the workspace label for an unnamed agent outside any fleet.
    pub name: String,
    pub issue: Option<u64>,
    /// orch | impl | rev | design, or `other` when herdsman did not spawn it.
    pub role: String,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    /// herdr's live state (idle | working | done | blocked | unknown), or `dead` | `parked` (log only, no pane).
    pub state: String,
    /// When herdsman spawned it (0: unknown).
    pub since: u64,
    pub pane: Option<String>,
    /// #83: a log-only lane whose worktree is clean and pushed: its short head sha.
    pub parked: Option<String>,
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
    let spawns: Vec<&Event> = events.iter().rev().filter(|e| e.kind == "spawn").collect();
    let (mut out, mut seen) = (vec![], BTreeSet::new());
    for a in agents {
        let s = |k: &str| a[k].as_str().filter(|v| !v.is_empty());
        let (name, pane) = (s("name"), s("pane_id"));
        // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
        let sp = spawns
            .iter()
            .find(|e| name.is_some() && e.str("agent") == name)
            .or_else(|| {
                spawns
                    .iter()
                    .find(|e| name.is_none() && pane.is_some() && e.str("pane") == pane)
            });
        let mut l = Lane {
            state: s("agent_status").unwrap_or("unknown").into(),
            pane: pane.map(String::from),
            ..Lane::default()
        };
        if let Some(e) = sp {
            seen.insert(e.str("agent").unwrap_or_default().to_string());
            l = Lane {
                name: e.str("agent").unwrap_or_default().into(),
                ..from_spawn(e, l)
            };
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
        if let Some(e) = spawns.iter().find(|e| e.str("agent") == Some(agent)) {
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
    out
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
        since: e.ts,
        ..l
    }
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
            std::cmp::Reverse(changed(l)),
        )
    });
    let mut out: Groups = vec![];
    for l in v {
        let (s, f) = (SECTIONS[section(l)], l.fleet.clone().unwrap_or_else(|| "Others".into()));
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
fn chrome(l: &str) -> bool {
    l.chars().all(|c| "─━│┃╭╮╰╯┌┐└┘═╌┄▔▁ ".contains(c)) || l.starts_with(['❯', '⏵', '>', '›'])
}

/// A screen's transcript and live status. Not transcript: the frame from the second-last divider down, and the line
/// above it opening with a glyph and holding a timer (`(25m 52s`: a spinner, a status bar) plus blank/detail lines.
pub fn chat(screen: &str) -> (Vec<&str>, Option<&str>) {
    let lines: Vec<&str> = screen.lines().collect();
    let rule = |l: &&str| l.split(|c| !GLYPHS[0].contains(c)).any(|r| r.chars().count() >= 20);
    let rules: Vec<usize> = (0..lines.len()).filter(|&i| rule(&lines[i])).collect();
    let cut = rules.len().checked_sub(2).map_or(lines.len(), |i| rules[i]);
    let after = |l: &str| {
        let (t, pad) = (l.trim(), l.len() - l.trim_start().len());
        t.is_empty() || pad >= 20 || pad > 0 && t.starts_with(|c| GLYPHS[1].contains(c))
    };
    let timer = |w: &str| matches!(w.strip_suffix(['h', 'm', 's']).map(str::parse::<u32>), Some(Ok(_)));
    let live = |t: &str| t.starts_with(|c: char| !c.is_alphanumeric()) && t.split(['(', ' ', ')']).any(timer);
    let i = lines[..cut].iter().rposition(|l| !after(l));
    let status = i.filter(|&i| live(lines[i].trim()));
    let body = lines[..status.unwrap_or(cut)].iter().copied().filter(|l| !rule(l));
    (body.collect(), status.map(|i| lines[i].trim()))
}

/// The live status line, else the latest action (Claude `⏺`, Hermes `💻`/`┊`, else non-chrome), masked, ≤ 60 chars.
pub fn activity(harness: &str, screen: &str) -> String {
    let (body, status) = chat(screen);
    let lines: Vec<&str> = body.into_iter().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = match harness {
        "claude" => lines.iter().rev().find(|l| l.starts_with('⏺')),
        "hermes" => lines.iter().rev().find(|l| l.contains('💻') || l.starts_with('┊')),
        _ => lines.iter().rev().find(|l| !chrome(l)),
    };
    let m = mask(status.or(line.copied()).unwrap_or_default());
    if m.chars().count() > 60 {
        m.chars().take(59).collect::<String>() + "…"
    } else {
        m
    }
}

/// The pane's last `n` non-empty lines, masked.
pub fn tail(screen: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| mask(l.trim_end()))
        .collect()
}

/// A blocked pane's pending prompt: the question and a Claude menu's options (a `❯`-marked run
/// numbered 1, 2, …; wrapped lines skipped), or no options for a free-text question. None: no prompt seen.
pub fn pending(screen: &str) -> Option<(String, Vec<String>)> {
    let lines: Vec<&str> = screen
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '│'))
        .filter(|l| !l.is_empty())
        .collect();
    let opt = |l: &str| {
        let (n, t) = l.trim_start_matches(['❯', '›']).trim_start().split_once(". ")?;
        Some((n.parse::<usize>().ok()?, t.trim().to_string()))
    };
    let question = |ls: &[&str]| {
        ls.iter()
            .rev()
            .take(6)
            .find(|l| l.contains('?'))
            .map(std::string::ToString::to_string)
    };
    let (mut opts, mut i, mut gap) = (
        vec![],
        lines.iter().rposition(|l| opt(l).is_some()).map_or(0, |i| i + 1),
        0,
    );
    while i > 0 && gap <= 2 && opts.last().is_none_or(|o: &(usize, String)| o.0 > 1) {
        i -= 1;
        match opt(lines[i]) {
            Some(o) => (opts.push(o), gap = 0).1,
            None => gap += 1,
        }
    }
    opts.reverse();
    let menu = opts.len() >= 2
        && opts.iter().zip(1..).all(|(o, n)| o.0 == n)
        && lines[i..].iter().any(|l| l.starts_with('❯') && opt(l).is_some());
    if menu {
        Some((
            question(&lines[..i]).or(lines[..i].last().map(std::string::ToString::to_string))?,
            opts.into_iter().map(|o| o.1).collect(),
        ))
    } else {
        Some((question(&lines)?, vec![]))
    }
}

/// #127: a menu's card from its block (after the last divider above the question): `title` its first line, `command`
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

/// Transcript lines per turn (a typed `❯`/`>`/`›` prompt opens one, as `me`): text runs, and tool calls
/// (Claude `⏺ Name(…)` plus its indented/`⎿` output, Hermes `┊` runs) as `{"tool": head, "body": …}`.
pub fn turns(lines: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for l in lines {
        let t = l.trim_start();
        let typed = t.starts_with(['❯', '>', '›']);
        if out.is_empty() || typed {
            out.push(serde_json::json!({"me": typed.then_some(t), "items": []}));
            if typed {
                continue;
            }
        }
        let items = out.last_mut().and_then(|t| t["items"].as_array_mut()).expect("a turn");
        let head = t
            .strip_prefix("⏺ ")
            .and_then(|r| r.split_once('('))
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(char::is_alphanumeric));
        let tool = items.last().is_some_and(|i| i.get("tool").is_some());
        let (k, v) = match items.last_mut() {
            Some(i) if tool && !head && (l.starts_with(' ') || t.starts_with(['⎿', '┊'])) => ("body", i),
            Some(i) if !tool && !head && !t.starts_with('┊') => ("text", i),
            _ => {
                let k = if head || t.starts_with('┊') { "tool" } else { "text" };
                items.push(serde_json::json!({k: t}));
                continue;
            }
        };
        let s = v[k].as_str().unwrap_or_default();
        v[k] = Value::from(if s.is_empty() { l.clone() } else { format!("{s}\n{l}") });
    }
    out
}
